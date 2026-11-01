#!/bin/sh
# Libraries the Linux build and the packaging scripts link and scan.
# mold is the linker. dpkg-dev is what linux.sh uses to write the .deb dependencies, and patchelf
# what it points the AppImage's tinyclient at the libraries bundled beside it.
set -eu

sudo apt-get update
sudo apt-get install -y \
    build-essential pkg-config libssl-dev libasound2-dev libgtk-3-dev libwebkit2gtk-4.1-dev \
    libsoup-3.0-dev libvulkan-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-dev \
    libxcb1-dev libfontconfig1-dev librsvg2-bin desktop-file-utils dpkg-dev patchelf mold
