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
# Steps:
#   1. Updates version in meson.build and Cargo.toml
#   2. Commits the version bump
#   3. Creates a git tag (v0.2.0)
#   4. Pushes commit + tag to origin
#   5. Updates the Flathub manifest with the new tag and commit hash
#   6. Commits and pushes the Flathub manifest update

release:
ifndef VERSION
	$(error VERSION is required. Usage: make release VERSION=0.2.0)
endif
	@echo "==> Releasing v$(VERSION)"
	@# ── 1. Update version in source files ────────────────────────────────
	sed -i "s/version: '[0-9]*\.[0-9]*\.[0-9]*'/version: '$(VERSION)'/" meson.build
	sed -i 's/^version = "[0-9]*\.[0-9]*\.[0-9]*"/version = "$(VERSION)"/' Cargo.toml
	@# ── 2. Update Cargo.lock ─────────────────────────────────────────────
	cargo check --quiet 2>/dev/null || true
	@# ── 3. Commit version bump ───────────────────────────────────────────
	git add meson.build Cargo.toml Cargo.lock
	git commit -m "chore: bump version to $(VERSION)"
	@# ── 4. Create tag and push ───────────────────────────────────────────
	git tag -a "v$(VERSION)" -m "Release v$(VERSION)"
	git push origin main
	git push origin "v$(VERSION)"
	@# ── 5. Update Flathub manifest ───────────────────────────────────────
	$(eval COMMIT_HASH := $(shell git rev-parse HEAD))
	sed -i 's/"tag" : "v[0-9]*\.[0-9]*\.[0-9]*"/"tag" : "v$(VERSION)"/' io.github.justinf555.Moments.flathub.json
	sed -i 's/"commit" : "[0-9a-f]*"/"commit" : "$(COMMIT_HASH)"/' io.github.justinf555.Moments.flathub.json
	@# ── 6. Commit Flathub manifest ───────────────────────────────────────
	git add io.github.justinf555.Moments.flathub.json
	git commit -m "chore: update Flathub manifest for v$(VERSION)"
	git push origin main
	@echo "==> Released v$(VERSION) (tag: v$(VERSION), commit: $(COMMIT_HASH))"
	@echo "==> Don't forget to update the Flathub fork with the new manifest"
