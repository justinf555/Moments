run:
	flatpak-builder --user --install --force-clean flatpak-build-dir io.github.justinf555.Moments.json && \
	flatpak run io.github.justinf555.Moments

clean:
	flatpak-builder --force-clean flatpak-build-dir io.github.justinf555.Moments.json
