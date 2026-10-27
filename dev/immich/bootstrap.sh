#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Justin F
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Wait for the test Immich server (dev/immich/compose.yml) to answer, then
# create its admin account so Moments can sign in straight away. Safe to run
# again: Immich refuses a second admin sign-up, which is reported, not fatal.
#
# Usage: dev/immich/bootstrap.sh [URL]   (default http://localhost:2283)
set -euo pipefail

URL="${1:-http://localhost:2283}"
EMAIL="admin@moments.test"
PASSWORD="moments"
NAME="Moments Test"

printf '==> Waiting for Immich at %s ' "$URL"
for _ in $(seq 1 120); do
    if curl -fsS "$URL/api/server/ping" 2>/dev/null | grep -q pong; then
        echo " up"
        break
    fi
    printf '.'
    sleep 2
done
curl -fsS "$URL/api/server/ping" >/dev/null 2>&1 || {
    echo
    echo "Immich didn't come up after 4 minutes; check: make immich-logs" >&2
    exit 1
}

status=$(curl -sS -o /dev/null -w '%{http_code}' \
    -H 'Content-Type: application/json' \
    -d "{\"email\":\"$EMAIL\",\"password\":\"$PASSWORD\",\"name\":\"$NAME\"}" \
    "$URL/api/auth/admin-sign-up")
case "$status" in
    201) echo "==> Created the admin account" ;;
    400) echo "==> Admin account already exists" ;;
    *)   echo "admin sign-up failed: HTTP $status" >&2; exit 1 ;;
esac

cat <<EOF
==> Immich is ready
    Web UI:   $URL
    Email:    $EMAIL
    Password: $PASSWORD
    Server URL in Moments:
      Flatpak or host build:  http://localhost:2283
      dev container:          http://host.containers.internal:2283
EOF
