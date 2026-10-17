#!/bin/sh
# Copies the release programs out of the packages CI already uploaded for this commit.
# found=true means target/release holds tinyclient, chatd, mediad and tinyclient-login.
# A commit CI did not build (or whose CI failed) leaves found=false, and the caller compiles.
set -eu

kind=${1:?usage: reuse-binaries.sh macos|linux-x86_64|windows}
case "$kind" in
    macos) artifact=tinyclient-macos ;;
    linux-x86_64) artifact=tinyclient-linux-x86_64 ;;
    windows) artifact=tinyclient-windows-x86_64 ;;
    *)
        echo "reuse-binaries.sh: nothing to reuse for $kind"
        echo "found=false" >> "${GITHUB_OUTPUT:?reuse-binaries.sh is for GitHub Actions}"
        exit 0
        ;;
esac

programs="tinyclient chatd mediad tinyclient-login"

# Returns 0 when the four programs are in target/release.
take() {
    stage=$1
    mkdir -p target/release
    case "$kind" in
        macos)
            app=$(find "$stage" -type d -name 'tinyclient.app' | head -1)
            [ -n "$app" ] || return 1
            for program in $programs; do
                cp "$app/Contents/MacOS/$program" "target/release/$program"
                chmod +x "target/release/$program"
            done
            ;;
        linux-x86_64)
            archive=$(find "$stage" -name 'tinyclient-*-linux-*.tar.gz' | head -1)
            [ -n "$archive" ] || return 1
            unpack=$(mktemp -d)
            tar -xzf "$archive" -C "$unpack"
            for program in $programs; do
                cp "$unpack/tinyclient/$program" "target/release/$program"
                chmod +x "target/release/$program"
            done
            rm -rf "$unpack"
            ;;
        windows)
            archive=$(find "$stage" -name '*.zip' | head -1)
            [ -n "$archive" ] || return 1
            if command -v unzip >/dev/null 2>&1; then
                unzip -qo "$archive" -d target/release
            else
                win=$(cygpath -w "$archive")
                dest=$(cygpath -w "$PWD/target/release")
                powershell.exe -NoProfile -Command "Expand-Archive -Force -LiteralPath '$win' -DestinationPath '$dest'"
            fi
            ;;
    esac
    for program in $programs; do
        if [ "$kind" = windows ]; then
            [ -f "target/release/$program.exe" ] || return 1
        else
            [ -x "target/release/$program" ] || return 1
        fi
    done
}

found=false
run=$(gh run list --workflow ci.yml --commit "$GITHUB_SHA" --status success --limit 1 \
    --json databaseId --jq '.[0].databaseId // empty' || true)

if [ -n "$run" ]; then
    stage=$(mktemp -d)
    if gh run download "$run" -n "$artifact" -D "$stage" && take "$stage"; then
        found=true
        echo "reuse-binaries.sh: using $artifact from CI run $run"
    else
        echo "reuse-binaries.sh: CI run $run has no usable $artifact; compiling"
    fi
    rm -rf "$stage"
else
    echo "reuse-binaries.sh: no successful CI run for $GITHUB_SHA; compiling"
fi

echo "found=$found" >> "${GITHUB_OUTPUT:?reuse-binaries.sh is for GitHub Actions}"
