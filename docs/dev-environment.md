# Development Environment

**Status:** Reference
**Date:** 2026-10-14

How to build, run, debug and package Moments. The short version of the rules lives in `CLAUDE.md`; this document has the details and the reasons behind them.

## Everything runs in the Flatpak SDK

Moments is built with Meson and packaged as a Flatpak against the GNOME 50 runtime. Every cargo-based target (`check`, `test`, `lint`, `fmt`, `coverage`, `audit`, …) runs inside `org.gnome.Sdk//50` with the `rust-stable` extension, so the toolchain and the GTK/libadwaita versions match what the app ships with.

Don't work around a Flatpak problem by running `cargo` against the host or container's own GTK. Those versions drift from the runtime, so a clean result there proves nothing.

### Toolbox containers

Development usually happens inside a Fedora toolbox, which has no `flatpak` binary, only `flatpak-spawn`. Flatpak has to run on the host anyway, because nested sandboxes don't work. The Makefile handles this itself:

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

## Debugging

Always debug the Flatpak dev build, never a binary run outside the sandbox.

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
