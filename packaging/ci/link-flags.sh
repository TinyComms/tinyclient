#!/bin/sh
# Compiler flags for CI and release. Line tables (not full debug info) are what make dev and
# test links, and the actions cache that stores them, a fraction of the old size. Release
# codegen is left alone: those binaries are the ones a tag publishes.
set -eu

case "${RUNNER_OS:-}" in
    Linux)
        # Stable rustc links x86_64 Linux with its own lld. Turning that off is stabilized
        # only there. On aarch64 the same flag is rejected, and the system linker is already
        # the default, so mold is selected with -fuse-ld alone.
        case "$(uname -m)" in
            x86_64)
                echo 'RUSTFLAGS=-C linker-features=-lld -C link-arg=-fuse-ld=mold' >> "$GITHUB_ENV"
                ;;
            *)
                echo 'RUSTFLAGS=-C link-arg=-fuse-ld=mold' >> "$GITHUB_ENV"
                ;;
        esac
        ;;
    Windows)
        # rustc calls the linker as `rust-lld -flavor link`. The gcc-ld copy named
        # lld-link.exe is the same binary but ignores -flavor and then tries to open
        # a file called "link".
        sysroot=$(rustc --print sysroot)
        lld="$sysroot/lib/rustlib/x86_64-pc-windows-msvc/bin/rust-lld.exe"
        if [ -x "$lld" ]; then
            echo "$(cygpath -w "$(dirname "$lld")")" >> "$GITHUB_PATH"
            echo 'RUSTFLAGS=-C linker=rust-lld.exe' >> "$GITHUB_ENV"
        else
            echo "link-flags.sh: rust-lld is not in the sysroot; keeping the MSVC linker"
        fi
        ;;
esac

echo 'CARGO_PROFILE_DEV_DEBUG=line-tables-only' >> "$GITHUB_ENV"
echo 'CARGO_PROFILE_TEST_DEBUG=line-tables-only' >> "$GITHUB_ENV"
