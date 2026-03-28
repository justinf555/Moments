run:
	flatpak-builder --user --install --force-clean flatpak-build-dir io.github.justinf555.Moments.json && \
	flatpak run io.github.justinf555.Moments

run-dev:
	flatpak-builder --force-clean flatpak-build-dir io.github.justinf555.Moments.dev.json && \
	flatpak-builder --run flatpak-build-dir io.github.justinf555.Moments.dev.json moments

clean:
	rm -rf flatpak-build-dir
