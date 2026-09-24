#!/usr/bin/env bash
# Wraps a test/bin invocation in a transient user cgroup scope so a runaway
# allocation dies inside its own cgroup instead of triggering a machine-wide
# OOM kill. Used as the cargo target runner (see .cargo/config.toml) so every
# `cargo test`/`cargo run` invocation of this workspace is capped, from any
# checkout (main repo or worktree).
set -euo pipefail

if [ "${EC_NOMEMGUARD:-0}" = "1" ]; then
    echo "memguard-runner: EC_NOMEMGUARD=1, running without memory cap" >&2
    exec "$@"
fi

if ! systemd-run --user --scope --quiet -p MemoryMax=10G true >/dev/null 2>&1; then
    echo "memguard-runner: systemd-run --user --scope unavailable, running without memory cap" >&2
    exec "$@"
fi

exec systemd-run --user --scope --quiet --same-dir \
    -p MemoryMax=10G -p MemorySwapMax=2G \
    -- "$@"
