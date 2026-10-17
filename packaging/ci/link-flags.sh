#!/bin/sh
# Compiler flags for CI and release. Line tables (not full debug info) are what make dev and
# test links, and the actions cache that stores them, a fraction of the old size. Release
# codegen is left alone: those binaries are the ones a tag publishes.
set -eu

case "${RUNNER_OS:-}" in
    Linux)
        # Stable rustc links x86_64 Linux with its own lld. Turn that off so mold is the
        # linker on x86_64 and on aarch64 (where lld is not the default yet).
        echo 'RUSTFLAGS=-C linker-features=-lld -C link-arg=-fuse-ld=mold' >> "$GITHUB_ENV"
        ;;
    Windows)
        sysroot=$(rustc --print sysroot)
        gcc_ld="$sysroot/lib/rustlib/x86_64-pc-windows-msvc/bin/gcc-ld"
        if [ -x "$gcc_ld/lld-link.exe" ]; then
            echo "$(cygpath -w "$gcc_ld")" >> "$GITHUB_PATH"
            echo 'RUSTFLAGS=-C linker=lld-link' >> "$GITHUB_ENV"
        else
            echo "link-flags.sh: rust-lld is not in the sysroot; keeping the MSVC linker"
        fi
        ;;
esac

echo 'CARGO_PROFILE_DEV_DEBUG=line-tables-only' >> "$GITHUB_ENV"
echo 'CARGO_PROFILE_TEST_DEBUG=line-tables-only' >> "$GITHUB_ENV"
