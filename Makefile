run:
	flatpak-builder --user --install --force-clean flatpak-build-dir io.github.justinf555.Moments.json && \
	flatpak run io.github.justinf555.Moments

run-dev:
	flatpak-builder --user --install --force-clean flatpak-build-dir io.github.justinf555.Moments.dev.json && \
	flatpak run --env=RUST_LOG=moments=debug io.github.justinf555.Moments

clean:
	rm -rf flatpak-build-dir

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
