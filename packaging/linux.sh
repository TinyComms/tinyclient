#!/bin/sh
# Build release binaries locally on Linux (GPUI + wry login). No .app, Flatpak, or MSI.
#
#   sudo apt-get install build-essential pkg-config libssl-dev libasound2-dev libgtk-3-dev \
#     libwebkit2gtk-4.1-dev libsoup-3.0-dev libvulkan-dev libwayland-dev \
#     libxkbcommon-dev libx11-dev libxcb1-dev libfontconfig1-dev
#
#   TINYCLIENT_TELEGRAM_API_ID=... TINYCLIENT_TELEGRAM_API_HASH=... \
#     cargo build --release -p tinyclient --features tinyclient/gui -p chatd -p mediad -p login
#
# Binaries: target/release/{tinyclient,chatd,mediad,tinyclient-login}
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
echo "See comments in packaging/linux.sh for apt packages and the cargo build command."
echo "Run from repo root: $root"
