.PHONY: run run-dev run-dhat dev-bootstrap dev clean clean-dev clean-bundle \
        check test test-nextest test-integration test-all \
        lint fmt fmt-check typos audit coverage metrics \
        check-potfiles ci-all stack attach release \
        bundle bundle-verify install-bundle uninstall-bundle

run:
	flatpak-builder --user --install --force-clean flatpak-build-dir io.github.justinf555.Moments.json && \
	flatpak run io.github.justinf555.Moments

run-dev:
	flatpak-builder --user --install --force-clean \
		--state-dir=.flatpak-builder-dev \
		flatpak-build-dev io.github.justinf555.Moments.dev.json && \
	flatpak run --env=RUST_LOG=moments=debug io.github.justinf555.Moments.Devel

# Fast iterative dev build (mirrors GNOME Builder's inner loop).
#
# Builder doesn't use flatpak-builder for the moments module itself —
# it uses `flatpak-builder --stop-at=moments` to set up the SDK + deps,
# then drives meson and cargo directly via `flatpak build` so the build
# dir (and cargo's target/) persist across runs. Editing one .rs file
# then triggers an incremental cargo rebuild instead of a full one.
#
# `make dev-bootstrap` is the one-time setup (also re-run after manifest
# changes). `make dev` is the fast inner loop. `make clean-dev` resets.
#
# Use `make run-dev` if you want the unmodified full-rebuild flow.
DEV_APP_DIR    = .flatpak-builder-dev/app
DEV_BUILD_DIR  = .flatpak-builder-dev/builddir
DEV_STATE_DIR  = .flatpak-builder-dev

dev-bootstrap:
	flatpak-builder --user --force-clean \
		--keep-build-dirs --disable-rofiles-fuse --ccache \
		--stop-at=moments \
		--state-dir=$(DEV_STATE_DIR) \
		$(DEV_APP_DIR) io.github.justinf555.Moments.dev.json
	flatpak build \
		--filesystem=$(CURDIR) \
		--filesystem=$(CURDIR)/$(DEV_BUILD_DIR):create \
		--env=PATH=/usr/lib/sdk/rust-stable/bin:/app/bin:/usr/bin \
		--env=RUST_BACKTRACE=1 \
		$(DEV_APP_DIR) \
		meson setup --prefix=/app --libdir=lib -Dprofile=development \
			$(CURDIR)/$(DEV_BUILD_DIR) $(CURDIR)

dev:
	@if [ ! -f $(DEV_BUILD_DIR)/build.ninja ]; then \
		echo "==> No build dir — running dev-bootstrap first"; \
		$(MAKE) dev-bootstrap; \
	fi
	flatpak build --share=network \
		--filesystem=$(CURDIR) \
		--filesystem=$(CURDIR)/$(DEV_BUILD_DIR) \
		--env=PATH=/usr/lib/sdk/rust-stable/bin:/app/bin:/usr/bin \
		--env=RUST_BACKTRACE=1 \
		$(DEV_APP_DIR) \
		meson install -C $(CURDIR)/$(DEV_BUILD_DIR)
	flatpak build \
		--share=network --share=ipc \
		--socket=wayland --socket=fallback-x11 \
		--device=dri --socket=pulseaudio \
		--talk-name=org.freedesktop.secrets \
		--talk-name=org.freedesktop.portal.* \
		--bind-mount=/run/user/$(shell id -u)/doc=/run/user/$(shell id -u)/doc/by-app/io.github.justinf555.Moments.Devel \
		--filesystem=$(HOME)/.var/app/io.github.justinf555.Moments.Devel:create \
		--env=GTK_A11Y=none \
		--env=RUST_LOG=moments=debug \
		--env=XDG_DATA_HOME=$(HOME)/.var/app/io.github.justinf555.Moments.Devel/data \
		--env=XDG_CONFIG_HOME=$(HOME)/.var/app/io.github.justinf555.Moments.Devel/config \
		--env=XDG_CACHE_HOME=$(HOME)/.var/app/io.github.justinf555.Moments.Devel/cache \
		$(DEV_APP_DIR) moments

DHAT_OUT = $(HOME)/.var/app/io.github.justinf555.Moments.Devel/cache/moments-dhat-heap.json

# One-shot heap profile: rebuild with dhat-heap enabled, run, restore.
# Requires `make dev-bootstrap` to have been done. Captures every alloc
# during the session and writes a JSON dump on exit. View at:
#   https://nnethercote.github.io/dh_view/dh_view.html
run-dhat:
	@if [ ! -f $(DEV_BUILD_DIR)/build.ninja ]; then \
		echo "==> No build dir — running dev-bootstrap first"; \
		$(MAKE) dev-bootstrap; \
	fi
	flatpak build --share=network \
		--filesystem=$(CURDIR) \
		--filesystem=$(CURDIR)/$(DEV_BUILD_DIR) \
		--env=PATH=/usr/lib/sdk/rust-stable/bin:/app/bin:/usr/bin \
		$(DEV_APP_DIR) \
		meson configure -Ddhat-heap=true $(CURDIR)/$(DEV_BUILD_DIR)
	-$(MAKE) dev
	flatpak build --share=network \
		--filesystem=$(CURDIR) \
		--filesystem=$(CURDIR)/$(DEV_BUILD_DIR) \
		--env=PATH=/usr/lib/sdk/rust-stable/bin:/app/bin:/usr/bin \
		$(DEV_APP_DIR) \
		meson configure -Ddhat-heap=false $(CURDIR)/$(DEV_BUILD_DIR)
	@echo "==> dhat-heap capture written to: $(DHAT_OUT)"
	@echo "==> View at: https://nnethercote.github.io/dh_view/dh_view.html"

clean:
	rm -rf flatpak-build-dir flatpak-build-dev

clean-dev:
	rm -rf .flatpak-builder-dev

# ── Distributable bundle ─────────────────────────────────────────────────────
#
# `make bundle` produces a single-file `.flatpak` bundle that anyone can
# install without adding a remote:
#
#   flatpak install --user moments-0.4.0-x86_64.flatpak
#
# It builds from the working tree (build-aux/…release.json uses a `dir`
# source), exports the result into a local OSTree repo, then packs that
# repo into the bundle. This is the same path release.yml takes to attach
# the bundle to a GitHub Release.
#
# The bundle records BUNDLE_RUNTIME_REPO as its runtime source, so
# installing it on a machine without the GNOME 50 runtime prompts to add
# Flathub and pull the runtime instead of failing.
#
# Optional GPG signing — set GPG_KEY to a key id or fingerprint to sign
# both the OSTree commit and the bundle. The exported public key is
# embedded in the bundle, so `flatpak install` verifies the signature
# against it (trust-on-first-use) and records it for the origin remote:
#
#   make bundle GPG_KEY=A1B2C3D4 [GPG_HOMEDIR=/path/to/gnupg]
#
# Without GPG_KEY the bundle is unsigned and installs with a warning.

APP_ID          = io.github.justinf555.Moments
BUNDLE_VERSION := $(shell sed -n "s/^[[:space:]]*version:[[:space:]]*'\([0-9][0-9.]*\)'.*/\1/p" meson.build | head -1)
BUNDLE_ARCH    ?= $(shell flatpak --default-arch 2>/dev/null || uname -m)
BUNDLE_MANIFEST = build-aux/$(APP_ID).release.json
BUNDLE_REPO     = flatpak-bundle-repo
BUNDLE_BUILD    = flatpak-bundle-build
BUNDLE          = moments-$(BUNDLE_VERSION)-$(BUNDLE_ARCH).flatpak
BUNDLE_RUNTIME_REPO ?= https://dl.flathub.org/repo/flathub.flatpakrepo

# Extra flags for flatpak-builder (CI passes --disable-rofiles-fuse etc.)
FLATPAK_BUILDER_ARGS ?=

GPG_KEY     ?=
GPG_HOMEDIR ?=
BUNDLE_PUBKEY = $(BUNDLE_BUILD)/moments.gpg

ifneq ($(GPG_KEY),)
GPG_ARGS = --gpg-sign=$(GPG_KEY)
ifneq ($(GPG_HOMEDIR),)
GPG_ARGS += --gpg-homedir=$(GPG_HOMEDIR)
GPG_EXPORT_ARGS = --homedir $(GPG_HOMEDIR)
endif
BUNDLE_GPG_ARGS = $(GPG_ARGS) --gpg-keys=$(BUNDLE_PUBKEY)
endif

bundle:
	@if [ -z "$(BUNDLE_VERSION)" ]; then \
		echo "could not read version from meson.build" >&2; exit 1; \
	fi
	@echo "==> Building $(APP_ID) $(BUNDLE_VERSION) ($(BUNDLE_ARCH))"
	mkdir -p $(BUNDLE_BUILD)
ifneq ($(GPG_KEY),)
	gpg $(GPG_EXPORT_ARGS) --export "$(GPG_KEY)" > $(BUNDLE_PUBKEY)
	@test -s $(BUNDLE_PUBKEY) || { echo "gpg exported an empty key for $(GPG_KEY)" >&2; exit 1; }
else
	@echo "==> GPG_KEY not set — bundle will be unsigned"
endif
	flatpak-builder --force-clean --repo=$(BUNDLE_REPO) \
		$(GPG_ARGS) $(FLATPAK_BUILDER_ARGS) \
		$(BUNDLE_BUILD)/app $(BUNDLE_MANIFEST)
	flatpak build-bundle --arch=$(BUNDLE_ARCH) \
		--runtime-repo=$(BUNDLE_RUNTIME_REPO) $(BUNDLE_GPG_ARGS) \
		$(BUNDLE_REPO) $(BUNDLE) $(APP_ID)
	sha256sum $(BUNDLE) > $(BUNDLE).sha256
	@echo "==> $(BUNDLE) ($$(du -h $(BUNDLE) | cut -f1))"
	@echo "==> Install with: flatpak install --user $(BUNDLE)"

install-bundle:
	@test -f $(BUNDLE) || $(MAKE) bundle
	flatpak install --user --bundle -y $(BUNDLE)

uninstall-bundle:
	-flatpak uninstall --user -y $(APP_ID)

clean-bundle:
	rm -rf $(BUNDLE_REPO) $(BUNDLE_BUILD) moments-*.flatpak moments-*.flatpak.sha256

# ── Testing (inside GNOME 50 Flatpak SDK) ────────────────────────────────────
#
# All test targets run inside the Flatpak SDK so that libadwaita 1.9
# and other GNOME 50 dependencies are available.

# Flatpak SDK runner — uses an isolated CARGO_HOME to avoid rustup shims
# in ~/.cargo/bin shadowing the SDK's toolchain. Registry and git caches
# are symlinked from the host for speed.
FLATPAK_RUN = flatpak run --share=network \
	--filesystem=$(CURDIR) \
	--filesystem=$(HOME)/.cargo/registry:create \
	--filesystem=$(HOME)/.cargo/git:create \
	--env=SQLX_OFFLINE=true \
	--env=CARGO_HOME=/tmp/flatpak-cargo \
	--command=bash org.gnome.Sdk//50

# Preamble sourced before every SDK command — sets up toolchain and cargo home.
# Creates an isolated CARGO_HOME with bin/ writable (for cargo install)
# and registry/git symlinked from the host cache for speed.
SDK_INIT = source /usr/lib/sdk/rust-stable/enable.sh && \
	mkdir -p /tmp/flatpak-cargo/bin && \
	ln -sf $(HOME)/.cargo/registry /tmp/flatpak-cargo/registry 2>/dev/null; \
	ln -sf $(HOME)/.cargo/git /tmp/flatpak-cargo/git 2>/dev/null; \
	export PATH=/tmp/flatpak-cargo/bin:$$PATH && \
	cd $(CURDIR)

check:
	$(FLATPAK_RUN) -c '$(SDK_INIT) && cargo check'

test:
	$(FLATPAK_RUN) -c '$(SDK_INIT) && cargo test'

test-nextest:
	$(FLATPAK_RUN) -c '$(SDK_INIT) && \
		cargo install cargo-nextest --locked 2>/dev/null || true && \
		cargo nextest run --profile ci'

test-integration:
	flatpak run --share=network \
	  --socket=wayland \
	  --filesystem=$(CURDIR) \
	  --filesystem=$(HOME)/.cargo/registry:ro \
	  --filesystem=$(HOME)/.cargo/git:ro \
	  --filesystem=$(XDG_RUNTIME_DIR) \
	  --env=SQLX_OFFLINE=true \
	  --env=CARGO_HOME=/tmp/flatpak-cargo \
	  --env=GSK_RENDERER=cairo \
	  --env=GTK_A11Y=none \
	  --env=GIO_USE_VFS=local \
	  --env=XDG_RUNTIME_DIR=$(XDG_RUNTIME_DIR) \
	  --env=WAYLAND_DISPLAY=$(WAYLAND_DISPLAY) \
	  --command=bash org.gnome.Sdk//50 \
	  -c '$(SDK_INIT) && cargo test --features integration-tests -- --test-threads=1'

test-all: test test-integration

# ── Linting & Analysis ──────────────────────────────────────────────────────

lint:
	$(FLATPAK_RUN) -c '$(SDK_INIT) && \
		cargo fmt -- --check && \
		cargo clippy --all-targets -- -D warnings && \
		cargo clippy --all-targets --features dhat-heap -- -D warnings'

fmt:
	$(FLATPAK_RUN) -c '$(SDK_INIT) && cargo fmt'

fmt-check:
	$(FLATPAK_RUN) -c '$(SDK_INIT) && cargo fmt -- --check'

typos:
	typos

audit:
	cargo audit --ignore RUSTSEC-2023-0071
	cargo deny check

coverage:
	$(FLATPAK_RUN) -c '$(SDK_INIT) && \
		cargo install cargo-llvm-cov --locked 2>/dev/null || true && \
		cargo llvm-cov --html && \
		echo "Coverage report: target/llvm-cov/html/index.html"'

metrics:
	@rust-code-analysis-cli --metrics -O json -p src/ 2>/dev/null | \
	python3 scripts/complexity-report.py

# ── Debug a hung process ────────────────────────────────────────────────────
#
# Usage: make stack [PID=<pid>]
#
# When the dev app hangs (no CPU activity, no log progress), this target
# attaches gdb from the GNOME SDK runtime and dumps a 30-frame backtrace
# for every thread. Auto-detects the PID by matching the dev app id; pass
# PID=… explicitly if more than one moments process is running.
#
# Example: a CR2 import sat forever on `typefind:sink`; `make stack` over
# the same process showed `gst::Pipeline::set_state` / `pull_sample` in
# seconds and named the deadlock unambiguously.

stack:
	@PID="$${PID:-$$(pgrep -f io.github.justinf555.Moments.Devel | tail -1)}"; \
	if [ -z "$$PID" ]; then \
		echo "no PID given and no Moments.Devel process found" >&2; exit 1; \
	fi; \
	echo "===== thread backtraces for PID $$PID ====="; \
	flatpak run --command=gdb org.gnome.Sdk//50 -p "$$PID" \
		-ex 'set pagination off' \
		-ex 'thread apply all bt 30' \
		-ex quit 2>&1 | grep -v '^\[New '

# Attach an interactive gdb session to a running dev app. The dev binary
# is built with debug symbols, so Rust function names tab-complete:
#
#   (gdb) break moments::importer::pipeline::ImportPipeline::import_one
#   (gdb) break src/renderer/format/registry.rs:99
#   (gdb) continue
#   (gdb) bt        # when it stops
#   (gdb) detach    # let the app run free again
#
# Auto-detects the dev app's PID; pass PID=… to override.
attach:
	@PID="$${PID:-$$(pgrep -f io.github.justinf555.Moments.Devel | tail -1)}"; \
	if [ -z "$$PID" ]; then \
		echo "no PID given and no Moments.Devel process found" >&2; exit 1; \
	fi; \
	echo "attaching gdb to PID $$PID — type 'continue' to resume, 'detach' to release"; \
	flatpak run --command=gdb org.gnome.Sdk//50 -p "$$PID"

# ── i18n ────────────────────────────────────────────────────────────────────

# Fail if any path listed in po/POTFILES.in is missing on disk.
# Catches stale entries left behind after file moves or deletions —
# the translation pipeline silently skips missing files, so this guard
# is the only thing that surfaces the breakage.
check-potfiles:
	@missing=0; \
	while IFS= read -r f; do \
		case "$$f" in ''|'#'*) continue;; esac; \
		if [ ! -e "$$f" ]; then \
			echo "POTFILES.in: missing $$f" >&2; \
			missing=$$((missing + 1)); \
		fi; \
	done < po/POTFILES.in; \
	if [ "$$missing" -gt 0 ]; then \
		echo "==> $$missing missing path(s) — regenerate po/POTFILES.in" >&2; \
		exit 1; \
	fi

# ── Full CI locally ─────────────────────────────────────────────────────────

ci-all: lint check-potfiles test test-integration audit

# ── Release ───────────────────────────────────────────────────────────────────
#
# Usage: make release VERSION=0.2.0
#
# Creates a release/v0.2.0 branch with version bumps in meson.build,
# Cargo.toml, and Cargo.lock, then opens a PR. On merge, the
# release.yml GitHub Action automatically:
#   - Creates an annotated git tag (v0.2.0)
#   - Creates a GitHub Release
#   - Builds the Flatpak bundle (`make bundle`) from the tag and attaches
#     moments-0.2.0-x86_64.flatpak + .sha256 to that release, GPG-signed
#     when the FLATPAK_GPG_PRIVATE_KEY / FLATPAK_GPG_KEY_ID secrets are set

release:
ifndef VERSION
	$(error VERSION is required. Usage: make release VERSION=0.2.0)
endif
	@echo "==> Preparing release v$(VERSION)"
	git checkout -b "release/v$(VERSION)"
	sed -i "s/version: '[0-9]*\.[0-9]*\.[0-9]*'/version: '$(VERSION)'/" meson.build
	sed -i 's/^version = "[0-9]*\.[0-9]*\.[0-9]*"/version = "$(VERSION)"/' Cargo.toml
	cargo check --quiet 2>/dev/null || true
	git add meson.build Cargo.toml Cargo.lock
	git commit -m "chore: bump version to $(VERSION)"
	git push -u origin "release/v$(VERSION)"
	gh pr create --title "chore: release v$(VERSION)" --body "Bump version to $(VERSION). Merging this PR will automatically create a git tag and GitHub Release."
	@echo "==> PR created. Merge it to trigger the release."
