#!/usr/bin/env bash
#
# Builds both halves of the addon:
#   - the bridge, a d3d11.dll that renders Blish inside the game's own swapchain
#   - the Blish HUD assembly itself
#
# Everything runs in containers, so no local Rust, mingw or .NET install is needed.
# BUILD_UID/BUILD_GID keep the output owned by you rather than root.
#
set -euo pipefail

cd "$(dirname "$0")"

export BUILD_UID="$(id -u)"
export BUILD_GID="$(id -g)"

docker compose run --rm bridge
docker compose run --rm blish

cat <<EOF

Built:
  bridge/target/x86_64-pc-windows-gnu/release/d3d11.dll
  Blish HUD/bin/x64/Release/net472/Blish HUD.exe

Note that content is not built here (see Directory.Build.targets), so copy the
assembly into an install that already has its Content/ directory.
EOF
