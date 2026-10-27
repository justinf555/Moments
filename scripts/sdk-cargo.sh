#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Justin F
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Stand-in for `cargo` (rust-analyzer.runnables.command in .vscode/settings.json):
# runs `cargo "$@"` inside the GNOME SDK through `make sdk-cargo`, the same
# environment as `make test`. printf %q keeps each argument intact through
# the SDK's `bash -c`.
#
# In the dev container (.devcontainer/) the native toolchain is the right one,
# so it runs cargo directly, after making sure src/config.rs exists. Before
# `cargo run` it runs `make native-build`: the app loads its gresource and
# GSettings schema from _install/, which plain cargo doesn't build.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ -n "${MOMENTS_DEVCONTAINER:-}" ]; then
    if [ "${1:-}" = run ]; then
        make -s --no-print-directory native-build >&2
    else
        make -s --no-print-directory config >&2
    fi
    exec cargo "$@"
fi
exec make -s --no-print-directory sdk-cargo CARGO_ARGS="$(printf '%q ' "$@")"
