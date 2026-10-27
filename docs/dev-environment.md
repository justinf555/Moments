# Development Environment

**Status:** Reference
**Date:** 2026-10-27

How to build, run, debug and package Moments. The short version of the rules lives in `CLAUDE.md`; this document has the details and the reasons behind them.

## Native locally, Flatpak in CI

Moments is built with Meson and packaged as a Flatpak against the GNOME 50 runtime. There are two ways to work locally:

- **The dev container** (`.devcontainer/`, recommended). Fedora 44 ships GNOME 50, so the app builds and runs natively against the same GTK, libadwaita and GStreamer versions as the runtime. Run, Debug and rust-analyzer work as in any Rust project.
- **The host or a toolbox.** Every cargo-based target (`check`, `test`, `lint`, `fmt`, `coverage`, `audit`, …) runs inside `org.gnome.Sdk//50` with the `rust-stable` extension.

The Makefile picks the right one: inside the dev container (`MOMENTS_DEVCONTAINER=1`, set by its Containerfile) the cargo targets run natively; anywhere else they run in the SDK. Don't run `cargo` against a toolbox's or host's own GTK: their versions drift from the runtime, so a clean result there proves nothing.

**Formal builds are always Flatpak.** CI's Build & test job (`build-aux/…ci.json`), the release and nightly bundles, and Flathub all build in the SDK, and CI is the authoritative check.

**What the dev container doesn't test:** anything that only happens inside the sandbox, such as permissions and `finish-args`, portals (the file chooser, and the keyring going through the Secret portal), codecs, and the runtime's exact library versions. Before a release, and for any change to manifests, portals or the keyring, install a CI or nightly bundle (or run `make run-dev` from the host) and smoke-test it.

## Dev container

Open the repository in VS Code and choose **Reopen in Container**. The setup follows the gtk-widgets field guide's container, built for rootless Podman on an SELinux-enforcing Fedora host:

- **Image:** Fedora 44 with rustup Rust (pinned to the SDK's version), the `-devel` packages for GTK, libadwaita, GStreamer, libheif and libsecret, and meson, blueprint-compiler and gettext for the meson build. RPM Fusion Free adds the codecs Fedora leaves out and the Flatpak runtime has: `libheif-freeworld` (the HEVC decoder for HEIC photos) and the full `ffmpeg` (H.264/HEVC video). When the runtime moves to GNOME 51, move the image to the Fedora release that ships it.
- **Display:** the host's Wayland socket, session bus and accessibility bus are mounted at their host paths, plus `/dev/dri`. There's no X11 fallback.
- **SELinux:** the container runs with `--security-opt=label=disable`. The confined `container_t` domain may not connect to the compositor's or the bus's socket, and relabelling the sockets with `:z` would change labels on live host session sockets. The container still runs rootless, in a user namespace, and gets only the sockets listed in `devcontainer.json`.
- **Volumes:** the cargo registry and git caches, `CARGO_TARGET_DIR`, the dev app's data (`~/.local/share`, where libraries live), its GSettings keyfile (`GSETTINGS_BACKEND=keyfile`, so settings never reach the host's dconf) and the `gh` config all survive rebuilds. Run `gh auth login` once inside.
- **Photos:** the host's `~/Pictures` is mounted read-only at its host path, so a referenced-mode library records the same paths in the container as in the Flatpak. Keep test photos in `~/Pictures/Test`. `.devcontainer/post-create.sh` adds a "Pictures" entry and a "Test photos" bookmark to the file chooser's sidebar.
- **GPU:** `runArgs` passes `/dev/dri` (Mesa: AMD, Intel, nouveau) and `--device=nvidia.com/gpu=all` for the proprietary NVIDIA driver, which Mesa can't use. Without the NVIDIA device GTK falls back to software rendering (Mesa's `llvmpipe` threads), which works but scrolls noticeably less smoothly than the Flatpak. The NVIDIA device needs a CDI spec on the host, and the container won't start without one, so **delete that line on a host with no NVIDIA GPU**. One-time host setup (the toolkit isn't packaged by Fedora or RPM Fusion):

  ```
  curl -fsSL https://nvidia.github.io/libnvidia-container/stable/rpm/nvidia-container-toolkit.repo \
    | sudo tee /etc/yum.repos.d/nvidia-container-toolkit.repo
  sudo dnf install nvidia-container-toolkit
  sudo nvidia-ctk cdi generate --output=/etc/cdi/nvidia.yaml
  ```

  Regenerate the spec after every NVIDIA driver update, or the container fails to start with a library version mismatch.

### Running and debugging natively

| Command | What it does |
|---|---|
| `make native-build` | Configures `_build-native/` (development profile, prefix `_install/`) if needed, then builds and installs into `_install/` |
| `make run-native` | `native-build`, then runs `_install/bin/moments` |
| `make clean-native` | Removes `_build-native/` and `_install/` |

The app needs meson's output to start: `src/config.rs` bakes in `PKGDATADIR`, `main.rs` loads `moments.gresource` from there, and the GSettings schema is compiled under the prefix. Installing into a prefix inside the checkout covers all three, and `XDG_DATA_DIRS` points at it.

`src/config.rs` is shared with SDK builds, which configure `_build/` with the default prefix. `native-build` reconfigures when `config.rs` doesn't point at `_install/`, so switching between the two is safe.

The container's environment (`containerEnv` in `devcontainer.json`, matching `NATIVE_ENV` in the Makefile) adds `_install/share` to `XDG_DATA_DIRS`, so every way of launching the app finds the schema, and sets `GDK_DEBUG=no-portals`. The host's portals would answer the file chooser with host paths, which don't exist in the container, so GTK's own file chooser is used instead. The same switch makes libadwaita read the colour scheme from GSettings rather than the Settings portal, so the app starts in light mode.

The app runs unsandboxed on the host's session bus:

- **Keyring:** libsecret talks to the host's Secret Service directly, so the `.Devel` app's Immich token is stored in your real keyring (as its own entry).
- **D-Bus name:** the app owns `io.github.justinf555.Moments.Devel` on the host bus. A `.Devel` Flatpak already running on the host would take the launch instead; close it first.

In VS Code:

- **Debug Moments (native, dev container)** (`.vscode/launch.json`) launches `_install/bin/moments` under CodeLLDB with `native-build` as its pre-launch task. Ctrl+F5 runs it without the debugger. Its environment duplicates `NATIVE_ENV` in the Makefile; keep them in step.
- **rust-analyzer Run / Run Test / Debug** run the native cargo: `scripts/sdk-cargo.sh` execs `cargo` directly when `MOMENTS_DEVCONTAINER` is set. Before `cargo run` it runs `make native-build`, because the app can't start without the gresource and schema. rust-analyzer's **Debug** button doesn't go through the script, so run `make native-build` once (or use the launch config) before debugging the binary that way.

### Toolbox containers

Outside the dev container, development can happen inside a Fedora toolbox, which has no `flatpak` binary, only `flatpak-spawn`. Flatpak has to run on the host anyway, because nested sandboxes don't work. The Makefile handles this itself:

- `HOST_CMD` detects a container via `/run/.containerenv` and resolves to `flatpak-spawn --host` (empty on the host).
- `$(FLATPAK)` is `$(HOST_CMD) flatpak`.
- `$(FLATPAK_BUILDER)` probes the *host* PATH for `flatpak-builder` and otherwise falls back to `$(FLATPAK) run org.flatpak.Builder`. Override with `make run FLATPAK_BUILDER=…` if you have both.

How `flatpak-spawn --host` behaves:

- **cwd is inherited.** The toolbox shares `$HOME`, so relative paths resolve the same on both sides and the manifests' relative source paths keep working.
- **Exit codes propagate,** so a failing target still fails the build.
- **Environment variables are not inherited.** `FOO=bar flatpak-spawn --host …` arrives empty. SDK invocations therefore pass everything through explicit `--env=` flags. A target that needs a variable takes it as a make argument (`make bundle GPG_KEY=…`).

## Build loops

| Loop | Commands | Notes |
|---|---|---|
| Full dev build | `make run-dev` | `io.github.justinf555.Moments.dev.json` (`type: dir`), app ID `.Devel`, state in `.flatpak-builder-dev/`. Rebuilds the module from scratch each time. |
| Fast incremental | `make dev-bootstrap` once, then `make dev` | Mirrors GNOME Builder: `flatpak-builder --stop-at=moments` sets up the SDK and dependencies, then `flatpak build` drives meson and cargo directly, so `.flatpak-builder-dev/builddir` and cargo's `target/` persist. Re-run `dev-bootstrap` after manifest changes; `make clean-dev` resets. |
| Build without running | `make dev-build` | Incremental build + install into `.flatpak-builder-dev/app`. VS Code's debug launch uses it as its pre-launch task. |
| Production ID | `make run` | `io.github.justinf555.Moments.json` pulls from this git repo (`type: git`, branch `main`), so **commit before rebuilding**. Installs `--user` so icons are exported to GNOME Shell. |

The `.Devel` app ID has its own GSettings, data dirs and keyring entries, so dev and production builds run side by side.

`make clean` removes only `flatpak-build-dir` and `flatpak-build-dev`. The `.flatpak-builder*/` state dirs and `_build/` stay; delete them by hand to reclaim space.

### `src/config.rs`

Meson generates `src/config.rs` from `src/config.rs.in` (`configure_file`, copied into the source tree by `src/meson.build`). It's gitignored, so a fresh checkout has none, and `make run` doesn't create one: it builds from a clone inside `.flatpak-builder/`. Every cargo target therefore depends on the `$(CONFIG_RS)` file target, which runs `meson setup _build`. That target also depends on `meson.build`, so a version bump refreshes a stale `VERSION`.

## Test Immich server

`dev/immich/compose.yml` runs a throwaway Immich for testing the Immich backend. It's Immich's own compose file with named volumes and fixed test credentials. It deliberately tracks Immich's latest release, so a release that breaks Moments shows up here before users report it. Run the targets on the host (they need `podman-compose`, and the dev container has no Podman):

| Command | What it does |
|---|---|
| `make immich-up` | Pulls the latest images, starts the stack, waits for it, creates the admin account and prints the login |
| `make immich-down` | Stops it; the library and database volumes are kept |
| `make immich-reset` | Stops it and deletes the volumes, for a fresh server |
| `make immich-logs` | Follows the server's log |

Sign in as `admin@moments.test` / `moments`. The server URL is `http://localhost:2283` from a Flatpak or host build, and `http://host.containers.internal:2283` from the dev container. The port is bound to 127.0.0.1 only.

- The first start pulls a few GB of images, most of it the machine-learning container. That container does face recognition, so the People view needs it.
- The server and machine-learning images follow Immich's `release` tag. To pin one release, for example to find which one broke something, run `IMMICH_VERSION=v2.x.y make immich-up`. An existing database can't be moved back to an older release; `make immich-reset` first.
- Valkey and Postgres can't simply follow a tag, because Immich occasionally requires a new Postgres image. `make immich-up` runs `dev/immich/check-upstream.sh` first, which compares them with the latest release's `docker-compose.yml` and prints a warning when they differ. Copy the new image lines into `compose.yml` when it does.

## Debugging

In the dev container, debug the native build (see above). On the host or in a toolbox, debug the Flatpak dev build, never a binary run outside the sandbox.

- **`make dev-gdb`** starts the SDK's gdb inside the same sandbox as `make dev`. Both take their sandbox flags from `DEV_RUN_FLAGS`, so add any new sandbox permission there, not to one target. Run `make dev-build` first.
- **`make attach [PID=…]`** attaches interactive gdb to a running `.Devel` process. The PID is auto-detected through `$(HOST_CMD) pgrep`, because a container-local `pgrep` sees a different PID namespace.
- **`make stack [PID=…]`** prints a backtrace of every thread of a running `.Devel` process. It's the quickest way to diagnose a hang.

### VS Code

- **Debug Moments (Flatpak dev build)** (`.vscode/launch.json`, needs `ms-vscode.cpptools`) drives gdb over a `pipeTransport`. `scripts/flatpak-gdb.sh` passes the extension's debugger command to `make dev-gdb` as `GDB_CMD`, adding `--interpreter=mi` if it's missing.
  - stdout must carry nothing but gdb's MI output, so keep `dev-gdb` silent (`@`, and the script's `make -s --no-print-directory`).
  - `GDB_CMD` is expanded inside single quotes, so use double quotes within it.
  - The SDK has gdb but no lldb, so CodeLLDB can't be used.
- **rust-analyzer Run / Run Test** go through `scripts/sdk-cargo.sh` (`rust-analyzer.runnables.command`). It runs `cargo "$@"` in the SDK via `make sdk-cargo CARGO_ARGS=…`, with the same environment and `target/` as `make test`.
  - The script `printf %q`-quotes each argument for the SDK's `bash -c`.
  - `CARGO_ARGS` sits inside single quotes, so it can't contain them.
  - rust-analyzer's **Debug** button isn't covered and still runs outside the sandbox.
- The Flatpak build dirs (`.flatpak-builder*/`, `flatpak-build-*/`, `_build/`) sit inside the repo and hold gigabytes of output, including thousands of vendored `.rs` files and symlinks. `.vscode/settings.json` keeps VS Code out of them:
  - It excludes them from rust-analyzer, the file watcher and file search (`files.exclude`, no symlink following). Otherwise extensions' startup `workspaceContains` searches crawl them indefinitely.
  - It gives rust-analyzer its own target dir.
  - It disables cpptools' C/C++ indexing; that extension is only there for gdb.
  - **Add any new build dir to every list there.**

### Heap profiling (dhat-rs)

`make run-dhat` reconfigures the dev build with `-Ddhat-heap=true` (Cargo `--features dhat-heap`), which swaps the global allocator for `dhat::Alloc`. It then runs the app and turns the option back off. On exit dhat writes `~/.var/app/io.github.justinf555.Moments.Devel/cache/moments-dhat-heap.json`. Load that file into <https://nnethercote.github.io/dh_view/dh_view.html> for a flame view of allocations by Rust call path. The feature has measurable overhead per allocation, so never ship with it on. `make lint` runs clippy with the feature too, so it keeps compiling.

## Release channels

See `docs/design-release-channels.md` for the design. In short:

| `-Dprofile=` | App ID | Name | Manifest(s) |
|---|---|---|---|
| `default` | `io.github.justinf555.Moments` | Moments | local `…Moments.json`, `build-aux/…release.json`, `…flathub.json` |
| `nightly` | `…Moments.Nightly` | Moments (Nightly) | `build-aux/…nightly.json` |
| `development` | `…Moments.Devel` | Moments (Development) | `…Moments.dev.json`, `build-aux/…ci.json` |

- `nightly` and `development` both get the GNOME "devel" style (striped headerbar); the check is `PROFILE != "default"`.
- Each channel has its own icon at `data/icons/hicolor/scalable/apps/<app-id>.svg`, which `data/icons/meson.build` installs by `application_id`. A new profile without one fails at configure time. The symbolic icon is shared.
- `-Dversion_suffix=` appends git-describe build metadata to the reported version (`0.4.1+6.g7ecfa4a`). It's empty for tagged builds; `make bundle CHANNEL=nightly` computes it.

## Bundles

`make bundle` builds `build-aux/io.github.justinf555.Moments.release.json` into a local OSTree repo. That build uses the production app ID, `-Dbuildtype=release` and a `type: dir` source. It then packs `moments-<version>-<arch>.flatpak` plus a `.sha256`.

- **Signing:** set `GPG_KEY` (and `GPG_HOMEDIR`) to sign the OSTree commit and the bundle; the bundle then embeds the exported public key.
- **Install and clean up:** `make install-bundle` / `make uninstall-bundle` / `make clean-bundle`.

`make bundle CHANNEL=nightly` (alias `make bundle-nightly`) builds `build-aux/…nightly.json` with the `.Nightly` app ID and the **unversioned** file name `moments-nightly-<arch>.flatpak`. The rolling `nightly` prerelease needs a stable download URL, which is why the name never changes. flatpak-builder takes no config-opts on the command line, so the recipe seds the computed version suffix into `build-aux/.…nightly.resolved.json` (gitignored) and builds that. The resolved file sits beside the template because manifest source paths resolve relative to the manifest's own directory.

## CI and release workflows

| Workflow | Trigger | What it does |
|---|---|---|
| `ci.yml` | PRs, pushes to `main` | REUSE, typos and AI-attribution checks, then **Build & test**: a Flatpak build of `build-aux/…ci.json` with `--run-tests` (fmt, clippy, unit and integration tests through meson `test()` targets). Build & test is the required check, so the workflow deliberately has no path filters. |
| `security.yml` | PRs that touch `Cargo.toml`, `Cargo.lock`, `deny.toml` or audit config; pushes to `main`; weekly | `cargo audit` and `cargo deny`. Not a required check. (CodeQL runs through GitHub's default setup, not a workflow file.) |
| `metrics.yml` | PRs, pushes to `main` | Code complexity report as a sticky PR comment |
| `bundle.yml` | reusable, `workflow_dispatch` | Runs `make bundle` in the Flathub CI container for a `channel` input (`release` or `nightly`) and uploads `flatpak-bundle-<channel>`. Use `workflow_dispatch` to smoke-test a bundle build. |
| `release.yml` | merged `release/v*` PR | Tags, creates the GitHub Release, and attaches the bundle |
| `nightly.yml` | push to `main`, daily cron | Builds the nightly bundle and edits the rolling `nightly` release in place, so the URL never 404s. Skips the build when nothing new has landed. |
| `claude-code-review.yml` | PR opened / updated | Automated review comment |
| `claude.yml` | issue and PR comments, reviews, issue opened or assigned | Responds when Claude is mentioned |

Signing happens when the `FLATPAK_GPG_PRIVATE_KEY` / `FLATPAK_GPG_KEY_ID` repo secrets exist. The CI signing key must have no passphrase, because ostree signs non-interactively.

### Cutting a release

`make release VERSION=x.y.z` creates `release/vx.y.z`, bumps `meson.build` and `Cargo.toml`, commits, pushes and opens a PR. Merging it triggers `release.yml`. The recipe runs a bare `cargo check` to refresh `Cargo.lock`, so run it on the host, or check `Cargo.lock` by hand if you run it from the container.
