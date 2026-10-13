#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Justin F
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Check that AI assistance is attributed the way AI_DISCLOSURE.md says.
#
#   scripts/check-ai-attribution.sh <base>..<head> [pr-body-file]
#
# Commits in the range must not name an AI tool in a Co-Authored-By trailer,
# and any Assisted-by trailer must follow the project's format (AI_DISCLOSURE.md):
#
#   Assisted-by: AGENT_NAME:MODEL_VERSION [TOOL1] [TOOL2]
#
# MODEL_VERSION may be omitted when the tool does not expose it.
#
# When a PR body file is given, it must contain an "AI assistance" section
# holding either one or more Assisted-by lines or the word "None".

set -euo pipefail

range="${1:-}"
body_file="${2:-}"

if [[ -z "$range" ]]; then
    echo "usage: $0 <base>..<head> [pr-body-file]" >&2
    exit 2
fi

# Email domains and names that identify an AI tool rather than a person.
ai_pattern='anthropic\.com|copilot|openai|codex|gemini|cursor|devin|aider|claude'
assisted_re='^Assisted-by: [A-Za-z0-9][A-Za-z0-9 ._-]*[A-Za-z0-9](:[A-Za-z0-9][A-Za-z0-9._-]*)?( \[[^][]+\])*$'

status=0

for sha in $(git rev-list --no-merges "$range"); do
    short=$(git rev-parse --short "$sha")
    body=$(git show -s --format=%B "$sha")

    if grep -iE '^Co-Authored-By:' <<<"$body" | grep -qiE "$ai_pattern"; then
        echo "::error::$short names an AI tool in a Co-Authored-By trailer; use Assisted-by instead (see AI_DISCLOSURE.md)"
        status=1
    fi

    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        if ! grep -qE "$assisted_re" <<<"$line"; then
            echo "::error::$short has a malformed trailer: '$line' (expected 'Assisted-by: AGENT:MODEL [TOOL]' or 'Assisted-by: AGENT')"
            status=1
        fi
    done < <(grep -E '^Assisted-by:' <<<"$body" || true)
done

if [[ -n "$body_file" ]]; then
    # Take the "AI assistance" section up to the next heading, drop HTML
    # comments and blank lines.
    section=$(awk '
        /^## AI assistance[[:space:]]*$/ { found = 1; next }
        found && /^## / { exit }
        found { print }
    ' "$body_file" | sed -E 's/<!--.*-->//g' | sed -E '/^[[:space:]]*$/d' || true)

    if ! grep -qE '^## AI assistance[[:space:]]*$' "$body_file"; then
        echo "::error::PR description has no '## AI assistance' section (see .github/pull_request_template.md)"
        status=1
    elif [[ -z "$section" ]]; then
        echo "::error::'## AI assistance' section is empty; list Assisted-by lines or write 'None'"
        status=1
    elif [[ "$(tr -d '[:space:]' <<<"$section")" != "None" ]]; then
        while IFS= read -r line; do
            if ! grep -qE "$assisted_re" <<<"$line"; then
                echo "::error::'## AI assistance' section contains '$line'; each line must be an Assisted-by trailer, or the section must be exactly 'None'"
                status=1
            fi
        done <<<"$section"
    fi
fi

if [[ $status -eq 0 ]]; then
    echo "AI attribution check passed"
fi
exit $status
