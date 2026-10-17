#!/bin/sh
# Builds tinyclient's Linux packages into dist/: a .deb, an .rpm, an AppImage, and a .tar.gz that
# carries install.sh. The release workflow runs it on every v* tag (.github/workflows/release.yml).
#
#   packaging/linux.sh                    build --release, then package as tinyclient-v<version>-linux-<arch>.*
#   packaging/linux.sh --no-build LABEL   package what is already in target/release as tinyclient-LABEL-linux-<arch>.*
#   TARGET_DIR=/tmp/t packaging/linux.sh  the same under another target directory
#
# Building needs (Debian and Ubuntu names):
#
#   sudo apt-get install build-essential pkg-config libssl-dev libasound2-dev libgtk-3-dev \
#     libwebkit2gtk-4.1-dev libsoup-3.0-dev libvulkan-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
#     libx11-dev libxcb1-dev libfontconfig1-dev protobuf-compiler
#
# Packaging needs librsvg2-bin, binutils and dpkg-dev, and nfpm (github.com/goreleaser/nfpm) and
# appimagetool (github.com/AppImage/appimagetool) on the PATH. APPIMAGE_RUNTIME names a type2
# runtime file for appimagetool to use instead of downloading the newest.
#
# Every package installs the four programs side by side (/usr/lib/tinyclient in the .deb and .rpm,
# usr/bin in the AppImage): each finds the others beside itself (`current_exe().with_file_name(..)`).
# Libraries are the system's. The .deb's Depends come from dpkg-shlibdeps; the .rpm's Requires are
# sonames, as rpmbuild would write them, so one .rpm suits Fedora and openSUSE alike. Nothing is
# bundled into the AppImage either: WebKitGTK and GTK must match the system they run on.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
target=${TARGET_DIR:-$root/target}
release=$target/release
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
programs="tinyclient chatd mediad tinyclient-login"
id=dev.tinyclient.app

build=1
label=
for arg in "$@"; do
    case $arg in
        --no-build) build=0 ;;
        -*) echo "linux.sh: unknown option $arg" >&2; exit 64 ;;
        *) label=$arg ;;
    esac
done
label=${label:-v$version}

arch=$(uname -m)
case $arch in
    x86_64) pkg_arch=amd64 ;;
    aarch64) pkg_arch=arm64 ;;
    *) echo "linux.sh: no packages for $arch" >&2; exit 1 ;;
esac
name=tinyclient-$label-linux-$arch
dist=$root/dist
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ $build = 1 ]; then
    cargo build --release -j "${JOBS:-6}" --manifest-path "$root/Cargo.toml" --target-dir "$target" \
        -p tinyclient --features tinyclient/gui -p chatd -p mediad -p login
fi
for program in $programs; do
    [ -x "$release/$program" ] || { echo "linux.sh: $release/$program is not built" >&2; exit 1; }
done
mkdir -p "$dist"

# The tree every format installs, rooted at /usr.
usr=$work/usr
mkdir -p "$usr/lib/tinyclient" "$usr/share/applications" "$usr/share/metainfo"
for program in $programs; do
    install -m 755 "$release/$program" "$usr/lib/tinyclient/$program"
done
install -Dm644 "$root/packaging/linux/tinyclient-chatd.service" \
    "$usr/lib/systemd/user/tinyclient-chatd.service"
cp "$root/packaging/linux/$id.desktop" "$usr/share/applications/"
sed -e "s/@VERSION@/$version/" -e "s/@DATE@/$(date -u +%Y-%m-%d)/" \
    "$root/packaging/linux/$id.metainfo.xml" > "$usr/share/metainfo/$id.metainfo.xml"

# Icons, drawn from the SVGs at each size. 32 px and below come from icon-small.svg, the drawing
# made for small sizes (as in packaging/bundle.sh).
icons=$usr/share/icons/hicolor
for size in 16 22 24 32 48 64 128 256 512; do
    svg=$root/packaging/icon.svg
    [ $size -le 32 ] && svg=$root/packaging/icon-small.svg
    mkdir -p "$icons/${size}x$size/apps"
    rsvg-convert -w $size -h $size "$svg" -o "$icons/${size}x$size/apps/$id.png"
done
mkdir -p "$icons/scalable/apps"
cp "$root/packaging/icon.svg" "$icons/scalable/apps/$id.svg"

bins=
for program in $programs; do bins="$bins $usr/lib/tinyclient/$program"; done
# shellcheck disable=SC2086
linked=$(for b in $bins; do objdump -p "$b" | awk '$1 == "NEEDED" { print $2 }'; done | sort -u)
# Opened with dlopen at run time, so no NEEDED entry names them: wgpu's Vulkan loader, and the
# Wayland client library (GPUI loads it only in a Wayland session). wgpu's GL fallback (libEGL,
# libwayland-egl) is left to the system: Vulkan is what it draws with.
opened="libvulkan.so.1 libwayland-client.so.0"
# shellcheck disable=SC2086
glibc=$(for b in $bins; do objdump -T "$b"; done | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)
echo "linux.sh: linked: $(echo "$linked" | tr "\n" " ")"
echo "linux.sh: opened at run time: $opened"
# What else the programs name that could be opened at run time, for whoever revises `opened`.
# shellcheck disable=SC2086
echo "linux.sh: other library names in the programs: $(strings $bins | grep -oE 'lib[A-Za-z0-9_+-]+\.so(\.[0-9]+)*' | sort -u | grep -vxF "$linked" | tr '\n' ' ')"
echo "linux.sh: newest glibc symbol: $glibc"

# .deb: dpkg-shlibdeps reads the libraries' symbols files for the lowest versions that will do.
mkdir -p "$work/shlibs/debian"
printf 'Source: tinyclient\n\nPackage: tinyclient\nArchitecture: any\n' > "$work/shlibs/debian/control"
# shellcheck disable=SC2086
deb_depends=$(cd "$work/shlibs" && dpkg-shlibdeps -O $bins | sed -n 's/^shlibs:Depends=//p')
for soname in $opened; do
    path=$(ldconfig -p | awk -v so="$soname" '$1 == so { print $NF; exit }')
    [ -n "$path" ] || { echo "linux.sh: $soname is not installed here" >&2; exit 1; }
    package=$( (dpkg -S "$path" 2>/dev/null || dpkg -S "$(readlink -f "$path")") | head -1 | cut -d: -f1)
    deb_depends="$deb_depends, $package"
done
echo "linux.sh: Depends: $deb_depends"

# .rpm: a soname for each library and a soname(version) for each versioned symbol set used, the
# names rpmbuild's own dependency generator writes and every rpm distribution provides.
rpm_requires=$(
    for soname in $linked $opened; do echo "$soname()(64bit)"; done
    # shellcheck disable=SC2086
    for b in $bins; do
        objdump -p "$b" | awk '
            /required from/ { lib = $3; sub(/:$/, "", lib); next }
            lib != "" && NF == 4 && $1 ~ /^0x/ && $4 !~ /PRIVATE/ { print lib "(" $4 ")(64bit)" }
            /^$/ { lib = "" }'
    done
)
rpm_requires=$(echo "$rpm_requires" | sort -u)
echo "linux.sh: Requires: $(echo "$rpm_requires" | tr "\n" " ")"

# Licences: the source is MIT; chatd links Signal's AGPL-3.0 libraries, so as shipped it is a
# combined work under the AGPL-3.0 (README.md, License).
{
    echo "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/"
    echo "Upstream-Name: tinyclient"
    echo "Source: https://github.com/itz4blitz/tinyclient"
    echo
    echo "Files: *"
    echo "Copyright: 2026 Justin Scroggins"
    echo "License: MIT"
    echo "Comment: chatd links presage, libsignal-service-rs and libsignal, which are AGPL-3.0,"
    echo " so the chatd in this package is distributed under the AGPL-3.0 as a combined work."
    echo " /usr/share/doc/tinyclient/AGPL-3.0.txt is its text."
    echo
    echo "License: MIT"
    sed -e 's/^$/./' -e 's/^/ /' "$root/LICENSE"
} > "$work/copyright"

yaml_list() {
    echo "$1" | tr ',' '\n' | sed -e 's/^ *//' -e '/^$/d' -e 's/.*/      - "&"/'
}
{
    cat <<EOF
name: tinyclient
arch: $pkg_arch
platform: linux
version: $version
release: 1
section: net
priority: optional
maintainer: Justin Scroggins <itz4blitz@users.noreply.github.com>
vendor: tinyclient
homepage: https://tinyclient.io
license: MIT AND AGPL-3.0-only
description: |-
  One small native app for Slack, Discord, Telegram, Teams and Signal.
  tinyclient signs in to each network directly and shows them in one window.
  Messages are cached on your disk and sessions kept in the Secret Service;
  there is no tinyclient server and no telemetry.
overrides:
  deb:
    depends:
$(yaml_list "$deb_depends")
    recommends:
      - "gnome-keyring | kwalletmanager"
      - "mesa-vulkan-drivers"
  rpm:
    depends:
$(echo "$rpm_requires" | sed 's/.*/      - "&"/')
    recommends:
      - "mesa-vulkan-drivers"
rpm:
  summary: One small native app for Slack, Discord, Telegram, Teams and Signal
  group: Applications/Internet
contents:
  # The package's own directories, so rpm removes them with it (dpkg would anyway).
  - dst: /usr/lib/tinyclient
    type: dir
    file_info:
      mode: 0755
  - dst: /usr/share/licenses/tinyclient
    type: dir
    file_info:
      mode: 0755
    packager: rpm
  - src: /usr/lib/tinyclient/tinyclient
    dst: /usr/bin/tinyclient
    type: symlink
  - src: $work/copyright
    dst: /usr/share/doc/tinyclient/copyright
    packager: deb
  - src: $root/packaging/AGPL-3.0.txt
    dst: /usr/share/doc/tinyclient/AGPL-3.0.txt
    packager: deb
  - src: $root/LICENSE
    dst: /usr/share/licenses/tinyclient/LICENSE
    type: license
    packager: rpm
  - src: $root/packaging/AGPL-3.0.txt
    dst: /usr/share/licenses/tinyclient/AGPL-3.0.txt
    type: license
    packager: rpm
EOF
    (cd "$work" && find usr -type f | sort) | while read -r file; do
        mode=0644
        case $file in usr/lib/tinyclient/*) mode=0755 ;; esac
        printf '  - src: %s\n    dst: /%s\n    file_info:\n      mode: %s\n' "$work/$file" "$file" "$mode"
    done
} > "$work/nfpm.yaml"

nfpm package --config "$work/nfpm.yaml" --packager deb --target "$dist/$name.deb"
nfpm package --config "$work/nfpm.yaml" --packager rpm --target "$dist/$name.rpm"

# .tar.gz: the programs, the desktop files under share/, and install.sh to put them in place.
tree=$work/tar/tinyclient
mkdir -p "$tree/share"
cp "$usr/lib/tinyclient/"* "$root/packaging/linux/install.sh" "$root/LICENSE" "$root/packaging/AGPL-3.0.txt" "$tree/"
cp -R "$usr/share/applications" "$usr/share/metainfo" "$usr/share/icons" "$tree/share/"
tar -czf "$dist/$name.tar.gz" -C "$work/tar" --owner=0 --group=0 tinyclient

# AppImage: the same tree under usr/, with the programs in usr/bin.
app=$work/AppDir
mkdir -p "$app/usr/bin" "$app/usr/share/metainfo"
cp "$usr/lib/tinyclient/"* "$app/usr/bin/"
cp -R "$usr/share/applications" "$usr/share/icons" "$app/usr/share/"
# appimagetool looks for <desktop file name>.appdata.xml.
cp "$usr/share/metainfo/$id.metainfo.xml" "$app/usr/share/metainfo/$id.appdata.xml"
cp "$root/packaging/linux/AppRun" "$app/AppRun"
cp "$usr/share/applications/$id.desktop" "$app/"
cp "$icons/256x256/apps/$id.png" "$app/$id.png"
ln -s "$id.png" "$app/.DirIcon"
runtime=
[ -n "${APPIMAGE_RUNTIME:-}" ] && runtime="--runtime-file $APPIMAGE_RUNTIME"
# shellcheck disable=SC2086
ARCH=$arch appimagetool --no-appstream $runtime "$app" "$dist/$name.AppImage"

ls -l "$dist/$name".*
