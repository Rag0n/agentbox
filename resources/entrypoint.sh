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

# Set up host bridge symlinks if configured
if [ -n "$HOSTEXEC_COMMANDS" ]; then
    mkdir -p /home/user/.local/bin
    for cmd in $HOSTEXEC_COMMANDS; do
        ln -sf /usr/local/bin/hostexec "/home/user/.local/bin/$cmd" 2>/dev/null || true
    done
fi

# Set up command_not_found fallback if enabled
if [ "$HOSTEXEC_FORWARD_NOT_FOUND" = "true" ]; then
    echo 'command_not_found_handle() { /usr/local/bin/hostexec "$@"; }' | sudo tee -a /etc/bash.bashrc > /dev/null
fi

# Shell mode: open bash instead of claude (used by `agentbox shell`)
if [ "$1" = "--shell" ]; then
    shift
    if [ $# -eq 0 ]; then
        exec bash -l
    else
        # Pass tokens as positional args so bash receives distinct words — not a
        # re-parsed string. Using "$*" would break args with single quotes or
        # spaces; exec "$@" preserves boundaries like docker exec does.
        exec bash -lc 'exec "$@"' bash "$@"
    fi
fi

exec claude --dangerously-skip-permissions "$@"
