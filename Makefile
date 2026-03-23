run:
	flatpak-builder flatpak-build-dir io.github.justinf555.Moments.json && \
	flatpak-builder --run flatpak-build-dir io.github.justinf555.Moments.json moments

clean:
	flatpak-builder --force-clean flatpak-build-dir io.github.justinf555.Moments.json
