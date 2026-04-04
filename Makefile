run:
	flatpak-builder --user --install --force-clean flatpak-build-dir io.github.justinf555.Moments.json && \
	flatpak run io.github.justinf555.Moments

run-dev:
	flatpak-builder --user --install flatpak-build-dir io.github.justinf555.Moments.dev.json && \
	flatpak run --env=RUST_LOG=moments=debug io.github.justinf555.Moments

clean:
	rm -rf flatpak-build-dir

# ── Testing (inside GNOME 50 Flatpak SDK) ────────────────────────────────────
#
# All test targets run inside the Flatpak SDK so that libadwaita 1.9
# and other GNOME 50 dependencies are available.

# Flatpak SDK runner — uses an isolated CARGO_HOME to avoid rustup shims
# in ~/.cargo/bin shadowing the SDK's toolchain. Registry and git caches
# are symlinked from the host for speed.
FLATPAK_RUN = flatpak run --share=network \
	--filesystem=$(CURDIR) \
	--filesystem=$(HOME)/.cargo/registry:ro \
	--filesystem=$(HOME)/.cargo/git:ro \
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
	$(FLATPAK_RUN) -c '$(SDK_INIT) && cargo clippy --all-targets -- -D warnings'

audit:
	cargo audit --ignore RUSTSEC-2023-0071
	cargo deny check

coverage:
	$(FLATPAK_RUN) -c '$(SDK_INIT) && \
		cargo install cargo-llvm-cov --locked 2>/dev/null || true && \
		cargo llvm-cov --html && \
		echo "Coverage report: target/llvm-cov/html/index.html"'

metrics:
	rust-code-analysis-cli --metrics -p src/ 2>/dev/null | head -50

# ── Full CI locally ─────────────────────────────────────────────────────────

ci-all: lint test test-integration audit

# ── Release ───────────────────────────────────────────────────────────────────
#
# Usage: make release VERSION=0.2.0
#
# Creates a release/v0.2.0 branch with version bumps in meson.build,
# Cargo.toml, and Cargo.lock, then opens a PR. On merge, the
# release.yml GitHub Action automatically:
#   - Creates an annotated git tag (v0.2.0)
#   - Updates the Flathub manifest with the new tag and commit hash
#   - Creates a GitHub Release

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
