#!/bin/sh
# Installs protoc 29.3 onto PATH. Ubuntu 22.04's protobuf-compiler is 3.12, which cannot
# compile Signal's protobufs (proto3 optional fields). Every runner uses this release.
set -eu

version=29.3
case "$(uname -s)" in
    Linux)
        case "$(uname -m)" in
            x86_64) asset=protoc-$version-linux-x86_64.zip ;;
            aarch64) asset=protoc-$version-linux-aarch_64.zip ;;
            *) echo "protoc.sh: no protoc build for Linux $(uname -m)" >&2; exit 1 ;;
        esac
        ;;
    Darwin)
        case "$(uname -m)" in
            arm64) asset=protoc-$version-osx-aarch_64.zip ;;
            x86_64) asset=protoc-$version-osx-x86_64.zip ;;
            *) echo "protoc.sh: no protoc build for Darwin $(uname -m)" >&2; exit 1 ;;
        esac
        ;;
    *)
        asset=protoc-$version-win64.zip
        ;;
esac

dest="${RUNNER_TEMP:-/tmp}/protoc"
mkdir -p "$dest"
curl -fsSL --retry 3 -o "$dest/protoc.zip" \
    "https://github.com/protocolbuffers/protobuf/releases/download/v$version/$asset"
unzip -qo "$dest/protoc.zip" -d "$dest"

bin="$dest/bin"
if command -v cygpath >/dev/null 2>&1; then
    bin=$(cygpath -w "$bin")
fi
echo "$bin" >> "${GITHUB_PATH:?protoc.sh is for GitHub Actions}"
