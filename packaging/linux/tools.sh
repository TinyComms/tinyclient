#!/bin/sh
# Fetches the packaging tools packaging/linux.sh runs, at fixed versions checked against their
# SHA-256, into DIR (default target/tools), and prints the environment to use them:
#
#   eval "$(packaging/linux/tools.sh)"        then packaging/linux.sh
#
# nfpm writes the .deb and .rpm; appimagetool and the type2 runtime it is given make the AppImage.
# To move a version, change it here with the hashes of the new files.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
dir=${1:-$root/target/tools}
mkdir -p "$dir/bin"

nfpm=2.48.0
appimagetool=1.9.1
runtime=20251108
case $(uname -m) in
    x86_64)
        nfpm_arch=x86_64
        nfpm_sha=6d7f9a28b2f397934403950fcbba83f598b08d02edbe79a0815b3b4897341750
        appimagetool_sha=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
        runtime_sha=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
        ;;
    aarch64)
        nfpm_arch=arm64
        nfpm_sha=a9a75213763783b7669719e505474c748c5ad7dc66591a195619deb50f85c2f4
        appimagetool_sha=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158
        runtime_sha=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444
        ;;
    *) echo "tools.sh: no tools for $(uname -m)" >&2; exit 1 ;;
esac

fetch() { # url sha256 out
    curl -fsSL --retry 3 -o "$3.part" "$1"
    echo "$2  $3.part" | sha256sum -c --quiet - >&2
    mv "$3.part" "$3"
}

fetch "https://github.com/goreleaser/nfpm/releases/download/v$nfpm/nfpm_${nfpm}_Linux_$nfpm_arch.tar.gz" "$nfpm_sha" "$dir/nfpm.tar.gz"
tar -xzf "$dir/nfpm.tar.gz" -C "$dir/bin" nfpm
fetch "https://github.com/AppImage/appimagetool/releases/download/$appimagetool/appimagetool-$(uname -m).AppImage" "$appimagetool_sha" "$dir/bin/appimagetool"
chmod +x "$dir/bin/appimagetool"
fetch "https://github.com/AppImage/type2-runtime/releases/download/$runtime/runtime-$(uname -m)" "$runtime_sha" "$dir/runtime"

echo "export PATH=\"$dir/bin:\$PATH\""
echo "export APPIMAGE_RUNTIME=\"$dir/runtime\""
# appimagetool is itself an AppImage, and CI machines have no FUSE to mount it with.
echo "export APPIMAGE_EXTRACT_AND_RUN=1"
