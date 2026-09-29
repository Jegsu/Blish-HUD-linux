#!/usr/bin/env bash
#
# Checks the bridge: formatting, clippy with warnings as errors, and the unit tests.
#
# The tests run on the Linux host rather than the Windows target: the modules they cover
# (protocol, config, keybinds, click routing) have no platform dependencies.
#
set -euo pipefail

cd "$(dirname "$0")"

export BUILD_UID="$(id -u)"
export BUILD_GID="$(id -g)"

docker compose run --rm bridge sh -c '
    cargo fmt --check &&
    cargo clippy --all-targets -- -D warnings &&
    cargo test --target x86_64-unknown-linux-gnu
'
