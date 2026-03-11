#!/bin/bash
set -e

DEFAULTS='{"hasCompletedOnboarding":true}'
CF="$HOME/.claude.json"
SEED="/tmp/claude-seed.json"

if [ -f "$SEED" ]; then
    jq -s '.[0] * .[1]' <(echo "$DEFAULTS") "$SEED" > "$CF"
else
    echo "$DEFAULTS" > "$CF"
fi

exec claude --dangerously-skip-permissions "$@"
