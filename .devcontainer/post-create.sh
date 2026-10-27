#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Justin F
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Run once when the dev container is created (postCreateCommand). Gives GTK's
# file chooser a Pictures entry and a "Test photos" bookmark (Pictures/Test)
# for the host folder devcontainer.json mounts read-only at its host path.
# GTK builds the sidebar from these files, and the container's home has
# neither.
#
# Usage: post-create.sh PICTURES_DIR
set -euo pipefail

pictures="$1"

mkdir -p ~/.config/gtk-3.0
printf 'XDG_PICTURES_DIR="%s"\n' "$pictures" > ~/.config/user-dirs.dirs
# GTK 4 still reads its bookmarks from gtk-3.0/bookmarks.
printf 'file://%s/Test Test photos\n' "$pictures" > ~/.config/gtk-3.0/bookmarks
