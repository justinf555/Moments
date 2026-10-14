#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Justin F
# SPDX-License-Identifier: GPL-3.0-or-later
#
# pipeTransport for VS Code's C/C++ debugger (.vscode/launch.json): runs the
# debugger command it appends (e.g. gdb --interpreter=mi) inside the
# `make dev` sandbox, with the SDK's gdb. stdout must carry nothing but gdb's
# MI output, hence make's -s and --no-print-directory.
set -euo pipefail
cd "$(dirname "$0")/.."
cmd="$*"
# Depending on its version the C/C++ extension may pass just "gdb"; VS Code
# only understands gdb's machine interface.
[[ $cmd == *--interpreter=* ]] || cmd+=" --interpreter=mi"
exec make -s --no-print-directory dev-gdb GDB_CMD="$cmd"
