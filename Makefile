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

FLATPAK_RUN = flatpak run --share=network \
	--filesystem=$(CURDIR) \
	--filesystem=$(HOME)/.cargo:create \
	--env=SQLX_OFFLINE=true \
	--env=CARGO_HOME=$(HOME)/.cargo \
	--command=bash org.gnome.Sdk//50

check:
	$(FLATPAK_RUN) -c 'source /usr/lib/sdk/rust-stable/enable.sh && cd $(CURDIR) && cargo check'

test:
	$(FLATPAK_RUN) -c 'source /usr/lib/sdk/rust-stable/enable.sh && cd $(CURDIR) && cargo test'

test-integration:
	$(FLATPAK_RUN) \
	  --env=GSK_RENDERER=cairo --env=GTK_A11Y=none --env=GIO_USE_VFS=local \
	  -c 'source /usr/lib/sdk/rust-stable/enable.sh && cd $(CURDIR) && cargo test --features integration-tests -- --test-threads=1'

test-all: test test-integration

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
