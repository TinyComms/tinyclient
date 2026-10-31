#!/bin/sh
# Installs tinyclient from the directory this script is in: the four programs side by side, a
# menu entry, icons and AppStream metadata. Nothing is downloaded and nothing runs as root unless
# you ask for --system.
#
#   ./install.sh                         for you only: ~/.local/lib/tinyclient, ~/.local/bin/tinyclient
#   sudo ./install.sh --system           for everyone: /usr/local/lib/tinyclient, /usr/local/bin/tinyclient
#   ~/.local/lib/tinyclient/install.sh --uninstall            removes the first
#   sudo /usr/local/lib/tinyclient/install.sh --uninstall --system   removes the second
#
# Uninstalling leaves your messages and sign-ins alone; https://tinyclient.io/docs/#sign-out says
# where they are. Run it again from a newer download to upgrade.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
action=install
scope=user
for arg in "$@"; do
    case $arg in
        --system) scope=system ;;
        --user) scope=user ;;
        --uninstall) action=uninstall ;;
        -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "install.sh: unknown option $arg (try --help)" >&2; exit 64 ;;
    esac
done

if [ $scope = system ]; then
    prefix=/usr/local
    share=$prefix/share
    if [ "$(id -u)" -ne 0 ]; then
        echo "install.sh: --system writes to $prefix; run it with sudo" >&2
        exit 1
    fi
else
    prefix=$HOME/.local
    share=${XDG_DATA_HOME:-$HOME/.local/share}
fi
lib=$prefix/lib/tinyclient
bin=$prefix/bin
id=dev.tinyclient.app

# Whatever of ours is still running from the old copy: chatd outlives the window by design.
stop_running() {
    command -v pkill >/dev/null 2>&1 || return 0
    for program in tinyclient chatd mediad tinyclient-login; do
        pkill -f "^$lib/$program( |\$)" 2>/dev/null || true
    done
}

refresh_caches() {
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$share/applications" 2>/dev/null || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$share/icons/hicolor" 2>/dev/null || true
}

if [ $action = uninstall ]; then
    stop_running
    rm -rf "$lib"
    [ -L "$bin/tinyclient" ] && rm -f "$bin/tinyclient"
    rm -f "$share/applications/$id.desktop" "$share/metainfo/$id.metainfo.xml"
    rm -f "$share/icons/hicolor"/*/apps/$id.png "$share/icons/hicolor/scalable/apps/$id.svg"
    refresh_caches
    echo "tinyclient removed from $prefix. Your data is kept: see https://tinyclient.io/docs/#sign-out"
    exit 0
fi

for program in tinyclient chatd mediad tinyclient-login; do
    [ -x "$here/$program" ] || { echo "install.sh: $here/$program is missing; run it from the unpacked archive" >&2; exit 1; }
done

stop_running
mkdir -p "$lib" "$bin" "$share/applications" "$share/metainfo"
# Copied beside and renamed into place, so a program that is still running is never written over.
for file in tinyclient chatd mediad tinyclient-login install.sh; do
    cp "$here/$file" "$lib/.$file.new"
    chmod 755 "$lib/.$file.new"
    mv -f "$lib/.$file.new" "$lib/$file"
done
cp "$here/LICENSE" "$here/AGPL-3.0.txt" "$lib/"
ln -sf "$lib/tinyclient" "$bin/tinyclient"

# The menu entry names the program by its full path: ~/.local/bin is not on every session's PATH.
sed "s|^Exec=.*|Exec=$lib/tinyclient %u|" "$here/share/applications/$id.desktop" > "$share/applications/$id.desktop"
cp "$here/share/metainfo/$id.metainfo.xml" "$share/metainfo/"
(cd "$here/share" && find icons -type f) | while read -r icon; do
    mkdir -p "$share/$(dirname "$icon")"
    cp "$here/share/$icon" "$share/$icon"
done
refresh_caches

echo "tinyclient installed in $lib"
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "Start it from your applications menu, or add $bin to PATH to run 'tinyclient'." ;;
esac
echo "Remove it with: $lib/install.sh --uninstall$( [ $scope = system ] && printf ' --system')"
