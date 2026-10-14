#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Justin F
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Stand-in for `cargo` (rust-analyzer.runnables.command in .vscode/settings.json):
# runs `cargo "$@"` inside the GNOME SDK through `make sdk-cargo`, the same
# environment as `make test`. printf %q keeps each argument intact through
# the SDK's `bash -c`.
set -euo pipefail
cd "$(dirname "$0")/.."
exec make -s --no-print-directory sdk-cargo CARGO_ARGS="$(printf '%q ' "$@")"
