#!/bin/sh
# Libraries the Linux build and the packaging scripts link and scan.
# mold is the linker. dpkg-dev is what linux.sh uses to write the .deb dependencies, and patchelf
# what it points the AppImage's tinyclient at the libraries bundled beside it.
#
# A mirror that stops answering fails this in minutes instead of holding the job: when the
# runners could not resolve Ubuntu's mirror, apt waited out every package in turn and Linux
# jobs sat here for most of an hour. Each request gives up after 20 seconds, and an index that
# does not download fails the update instead of being ignored, so a dead mirror stops here. The
# update has 2 minutes and the download 10; the install itself reads only what was downloaded
# and is never cut short, because a dpkg stopped halfway breaks the next try. A failed try is
# tried once more after a minute, in case the mirror was only blinking.
set -eu

packages="build-essential pkg-config libssl-dev libasound2-dev libgtk-3-dev libwebkit2gtk-4.1-dev
    libsoup-3.0-dev libvulkan-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-dev
    libxcb1-dev libfontconfig1-dev librsvg2-bin desktop-file-utils dpkg-dev patchelf mold"

# apt <seconds> <apt-get arguments...>
apt() {
    limit=$1
    shift
    sudo timeout "$limit" apt-get -o Acquire::Retries=2 -o Acquire::http::Timeout=20 \
        -o Acquire::https::Timeout=20 -o DPkg::Lock::Timeout=60 "$@"
}

for try in 1 2; do
    # shellcheck disable=SC2086 # the package list splits into words on purpose
    if apt 120 update --error-on=any && apt 600 install -y --download-only $packages; then
        # shellcheck disable=SC2086
        exec sudo apt-get install -y --no-download $packages
    fi
    if [ "$try" = 1 ]; then
        echo "::warning::apt did not finish; trying once more in a minute"
        sleep 60
    fi
done
echo "::error::Ubuntu's package mirror did not answer. This is the runner's network, not the build: re-run the job."
exit 1
