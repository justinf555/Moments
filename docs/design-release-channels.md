# Release Channels

**Status:** Implemented
**Date:** 2026-08-20

## Overview

Moments ships from three channels, each installed under its own Flatpak
application id so all three can coexist on one machine:

| Channel | Application id | Built from | Icon |
|---|---|---|---|
| Release | `io.github.justinf555.Moments` | a `v*` tag | blue, no banner |
| Nightly | `io.github.justinf555.Moments.Nightly` | tip of `main`, every merge | orange, `NIGHTLY` banner |
| Development | `io.github.justinf555.Moments.Devel` | the local working tree | purple + hazard stripes, `DEV` banner |

The channel is selected by the Meson option `-Dprofile={default,nightly,development}`,
which is set by whichever Flatpak manifest is being built.

## Problem

Before this, there were two channels — production and a local `.Devel` build — and
nothing between them. Code merged to `main` had no installable artefact until
someone cut a release, so the only way to try an unreleased fix was to build it
yourself. A nightly closes that gap.

Running a nightly must not cost you your production install. Since the application
id drives the GSettings path, the data directory, the keyring entry, the desktop
file, the D-Bus name and the icon name, a distinct id is all that's needed for the
two to coexist with separate libraries and settings.

## Distinguishing the channels

Three installs of the same app are only useful if you can tell which one you're
looking at. Three signals carry that, in descending order of robustness:

1. **Background colour** — blue / orange / purple. The only signal that survives
   rasterisation to 32px, which is the size used in the dash, alt-tab and
   notifications. Orange was chosen for Nightly rather than a darker "night" tone
   because blue-vs-purple is already the weakest pair for red-green colour
   blindness; a third blue-ish tone would have made it worse.
2. **Application name** — "Moments", "Moments (Nightly)", "Moments (Development)".
   Set once in `meson.build` as `application_name` and threaded through the
   desktop file (`Name=`), the AppStream metainfo (`<name>`) and `config::APP_NAME`
   for the About dialog. This is what disambiguates in search results and window
   lists, where the icon is small or absent.
3. **Icon banner** — a bar across the bottom of the icon reading `NIGHTLY` or
   `DEV`. Legible at 128px and 64px, marginal at 48px, gone at 32px. It is
   deliberately the *secondary* signal for that reason.

Non-production channels also get the GNOME "devel" style (the striped headerbar):

```rust
// src/ui/window/mod.rs
if crate::config::PROFILE != "default" {
    win.add_css_class("devel");
}
```

The symbolic icon stays shared across all three. At 16px monochrome there is no
room for a marker that survives rasterisation, so a variant would differ from the
production icon without conveying anything.

### Banner letterforms

The banner text is emitted as stroked polylines, not `<text>`. The icon is
rasterised by librsvg wherever the shell happens to run it, and no particular font
is guaranteed to be installed there — a `<text>` element would silently fall back
to whatever metrics that host has, or render nothing.

The banner runs the full width of the icon rather than sitting in a corner as a
diagonal ribbon: a corner ribbon gives a seven-glyph word about 55px of run, where
the bottom bar gives 96px, and `NIGHTLY` needs every pixel it can get.

## Versioning

A nightly reports the version it was built from, so a bug report identifies an
exact commit:

```
0.4.1+6.g7ecfa4a
```

That is the released version plus semver *build metadata* — six commits past the
`v0.4.1` tag, at short sha `7ecfa4a`. Build metadata rather than a pre-release
suffix (`0.4.2-dev.6`) because the latter would name a version that has not been
released and may never be: the next release could just as easily be `0.5.0`.

The suffix reaches the build through the `version_suffix` Meson option:

```
conf.set_quoted('VERSION', meson.project_version() + get_option('version_suffix'))
```

`flatpak-builder` accepts no `config-opts` on the command line, so `make bundle
CHANNEL=nightly` substitutes the computed suffix into a resolved copy of the
manifest. That copy lives next to the template in `build-aux/`, because manifest
source paths (`"path": ".."`) resolve relative to the manifest's own directory.

## Publishing

`nightly.yml` runs on every push to `main`, plus a daily cron as a safety net so
the channel recovers on its own if a push build fails for an infrastructure
reason. It calls the existing `bundle.yml` with `channel: nightly` and republishes
the result to a **rolling `nightly` prerelease**, so the download URL never
changes:

```
https://github.com/justinf555/Moments/releases/download/nightly/moments-nightly-x86_64.flatpak
```

Two consequences follow from that URL being the deliverable:

- **The bundle filename carries no version.** A versioned filename would move the
  URL on every build, which is the one thing this is designed to avoid. The
  version lives inside the app and in the release notes instead.
- **The release is edited in place, not deleted and recreated.** A delete/create
  cycle leaves a window where the URL 404s. The workflow force-moves the `nightly`
  tag to the built commit, then `gh release edit` + `gh release upload --clobber`.

`concurrency: {group: nightly, cancel-in-progress: true}` means a merge train
doesn't race two builds onto the same release — the newest merge wins.

### Build cache and the CodeQL cache-poisoning alert

`bundle.yml` caches `.flatpak-builder` between runs, which is what keeps a nightly
build to a few minutes instead of a cold ~20. CodeQL's
`actions/cache-poisoning/direct-cache` flags that write, and the alert is
**dismissed as a false positive**. The reasoning, recorded here because the
dismissal comment is capped at 280 characters:

- **No untrusted code reaches the workflow.** Every trigger requires repository
  write access — `push` and `schedule` are default-branch only, `workflow_dispatch`
  requires write, and the release path is `pull_request: closed` on a `release/v*`
  branch. `bundle.yml` is never triggered by `pull_request`, so there is no
  fork-PR path, which is the scenario the query is really about.
- **Cache scopes are per-branch.** A workflow run restores caches from its own
  branch or the default branch, never from a sibling. A dispatch build on a feature
  branch therefore writes to *that branch's* scope, and a build on `main` will never
  read it. Writing the scope `main` reads requires running on `main`, which requires
  write access to `main`.

The one path that was genuinely a gap — a `workflow_dispatch` **on main** pointing
`ref` at an arbitrary commit, whose output would land in main's cache scope — is
closed by splitting `actions/cache@v4` into `cache/restore` plus a `cache/save`
gated on `github.event_name != 'workflow_dispatch'`. Dispatch builds read the cache;
they never write it.

Inside a reusable workflow `github.event_name` is the **caller's** event, so that
guard sees `push` from `nightly.yml`, `pull_request` from `release.yml`, and
`workflow_dispatch` only when `bundle.yml` is dispatched directly or via a dispatch
of `nightly.yml`.

If the trigger set ever grows a `pull_request` entry — anything that builds code
from a fork — this reasoning no longer holds and the cache save must be
reconsidered.

## Flow

```
  merge to main
        │
        ▼
  nightly.yml  ──►  bundle.yml (channel: nightly)
                          │
                          │  make bundle CHANNEL=nightly
                          │    ├─ sed @VERSION_SUFFIX@ → .nightly.resolved.json
                          │    ├─ flatpak-builder → OSTree repo
                          │    └─ flatpak build-bundle → moments-nightly-<arch>.flatpak
                          ▼
                  upload-artifact
                          │
                          ▼
              rolling `nightly` prerelease  (stable URL)


  release/vX.Y.Z PR merged
        │
        ▼
  release.yml  ──►  bundle.yml (channel: release, default)
                          │
                          ▼
                  vX.Y.Z GitHub Release  (versioned filename)
```

## Files

| File | Role |
|---|---|
| `meson_options.txt` | `profile` combo gains `nightly`; new `version_suffix` string |
| `meson.build` | maps profile → `application_id` + `application_name` |
| `data/meson.build` | exposes `APP_NAME` to the configured data files |
| `build-aux/…Moments.nightly.json` | nightly manifest, `-Dprofile=nightly` |
| `data/icons/…/…Moments.Nightly.svg` | orange icon with the `NIGHTLY` banner |
| `Makefile` | `CHANNEL` selects manifest, app id and bundle filename |
| `.github/workflows/nightly.yml` | push-to-main trigger, rolling release |
| `.github/workflows/bundle.yml` | gains a `channel` input |

## Future work

- **A Flatpak repo instead of bundles.** A hosted OSTree repo (GitHub Pages) with
  a `.flatpakref` would let `flatpak update` pull new nightlies, which a one-off
  bundle install cannot do. This is how `nightly.gnome.org` works and is the
  natural next step if the nightly channel gets real users.
- **In-app channel indicator.** The About dialog names the channel; the window
  itself only has the striped headerbar.
