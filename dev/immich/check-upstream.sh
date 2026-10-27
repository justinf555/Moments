#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Justin F
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Compare the Valkey and Postgres images in dev/immich/compose.yml with the
# latest Immich release's docker-compose.yml, and warn when they differ.
# Immich occasionally requires a new Postgres image; a server on the old one
# then fails at startup or migration. Never fails the build: no network, or a
# changed upstream layout, only prints a note.
set -uo pipefail

LOCAL="$(dirname "$0")/compose.yml"
UPSTREAM_URL="https://github.com/immich-app/immich/releases/latest/download/docker-compose.yml"

images() { grep -E '^\s*image:\s*\S*(valkey|postgres)' | sed -E 's/^\s*image:\s*//' | sort; }

upstream=$(curl -fsSL --max-time 15 "$UPSTREAM_URL" 2>/dev/null | images)
if [ -z "$upstream" ]; then
    echo "==> Couldn't fetch Immich's latest compose file; skipping the image check"
    exit 0
fi

if [ "$upstream" = "$(images < "$LOCAL")" ]; then
    echo "==> Valkey and Postgres images match the latest Immich release"
else
    cat <<MSG
==> WARNING: dev/immich/compose.yml's Valkey/Postgres images differ from the
    latest Immich release. Update them from $UPSTREAM_URL
    upstream:
$(echo "$upstream" | sed 's/^/      /')
    local:
$(images < "$LOCAL" | sed 's/^/      /')
MSG
fi
