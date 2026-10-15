#!/bin/sh
# Installs the Linux packages in dist/ on the system it runs on, a clean container in the release
# workflow, and checks that each program starts and that removal leaves nothing behind.
#
#   deb        apt installs the .deb, pulling its Depends; every program answers --version; apt removes it
#   deb-full   the same, then validates the desktop entry and metainfo, runs the AppImage, and
#              installs and uninstalls the .tar.gz with install.sh, for a user and system-wide
#   rpm        dnf or zypper installs the .rpm, pulling its Requires; --version; removes it
#
# Needs LABEL, VERSION and ARCH (packaging/ci-label.sh); run from the repository root.
set -eu
: "${LABEL:?}" "${VERSION:?}" "${ARCH:?}"
start=$PWD
base=dist/tinyclient-$LABEL-linux-$ARCH
id=dev.tinyclient.app
programs="tinyclient chatd mediad tinyclient-login"

# Prints the program's --version and fails unless it names this version.
answers() { # name path
    out=$("$2" --version)
    echo "$2: $out"
    [ "$out" = "$1 $VERSION" ] || { echo "smoke.sh: expected '$1 $VERSION'" >&2; exit 1; }
}

all_answer() { # dir
    for program in $programs; do answers "$program" "$1/$program"; done
}

# The libraries opened with dlopen (packaging/linux.sh), which --version never loads.
loadable() {
    for soname in libvulkan.so.1 libwayland-client.so.0; do
        ldconfig -p | grep -q "^[[:space:]]*$soname " || { echo "smoke.sh: $soname was not installed" >&2; exit 1; }
    done
}

case ${1:?usage: smoke.sh deb|deb-full|rpm} in
deb|deb-full)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -q
    apt-get install -y -q "./$base.deb"
    dpkg -s tinyclient | sed -n '/^Depends/p;/^Recommends/p'
    answers tinyclient /usr/bin/tinyclient
    all_answer /usr/lib/tinyclient
    loadable
    test -f /usr/share/icons/hicolor/256x256/apps/$id.png
    test -f /usr/share/doc/tinyclient/copyright
    if [ "$1" = deb-full ]; then
        apt-get install -y -q desktop-file-utils appstream
        desktop-file-validate /usr/share/applications/$id.desktop
        appstreamcli validate --no-net /usr/share/metainfo/$id.metainfo.xml

        # The AppImage, without FUSE: the runtime unpacks itself and runs AppRun.
        chmod +x "$base.AppImage"
        out=$("./$base.AppImage" --appimage-extract-and-run --version)
        echo "AppImage: $out"
        [ "$out" = "tinyclient $VERSION" ]
        tmp=$(mktemp -d)
        (cd "$tmp" && "$start/$base.AppImage" --appimage-extract >/dev/null)
        all_answer "$tmp/squashfs-root/usr/bin"
        desktop-file-validate "$tmp/squashfs-root/$id.desktop"

        # The .tar.gz, for a user with no root, then for everyone.
        useradd -m tester
        cp "$base.tar.gz" /tmp/tinyclient.tar.gz
        chmod 644 /tmp/tinyclient.tar.gz
        su tester -c "
            set -eu
            cd \$(mktemp -d)
            tar -xzf /tmp/tinyclient.tar.gz
            tinyclient/install.sh
            test \"\$(~/.local/bin/tinyclient --version)\" = 'tinyclient $VERSION'
            grep -q '^Exec=/home/tester/.local/lib/tinyclient/tinyclient$' ~/.local/share/applications/$id.desktop
            desktop-file-validate ~/.local/share/applications/$id.desktop
            test -f ~/.local/share/icons/hicolor/16x16/apps/$id.png
            ~/.local/lib/tinyclient/install.sh --uninstall
            test ! -e ~/.local/lib/tinyclient
            test ! -e ~/.local/bin/tinyclient
            test ! -e ~/.local/share/applications/$id.desktop
            test -z \"\$(find ~/.local/share/icons -name '$id.*')\"
        "
        cd "$(mktemp -d)"
        tar -xzf /tmp/tinyclient.tar.gz
        tinyclient/install.sh --system
        answers tinyclient /usr/local/bin/tinyclient
        all_answer /usr/local/lib/tinyclient
        /usr/local/lib/tinyclient/install.sh --uninstall --system
        test ! -e /usr/local/lib/tinyclient
        test ! -e /usr/local/share/applications/$id.desktop
        cd "$start"
    fi
    apt-get remove -y -q tinyclient
    test ! -e /usr/lib/tinyclient
    test ! -e /usr/bin/tinyclient
    ;;
rpm)
    if command -v dnf >/dev/null 2>&1; then
        dnf install -y "./$base.rpm"
    else
        zypper --non-interactive install --allow-unsigned-rpm "./$base.rpm"
    fi
    rpm -qi tinyclient
    rpm -qR tinyclient | head -50
    answers tinyclient /usr/bin/tinyclient
    all_answer /usr/lib/tinyclient
    loadable
    # Listed rather than looked for: container images install without docs and licences (nodocs).
    rpm -ql tinyclient | grep -qx /usr/share/licenses/tinyclient/AGPL-3.0.txt
    rpm -e tinyclient
    test ! -e /usr/lib/tinyclient
    ;;
*)
    echo "smoke.sh: unknown check $1" >&2
    exit 64
    ;;
esac
echo "smoke.sh: $1 passed"
