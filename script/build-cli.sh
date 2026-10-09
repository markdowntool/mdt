#!/usr/bin/env bash
# Build only the mdt executable, in this checkout's own target tree.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "${1:-}" = --help ]; then
    cat <<'HELP'
Usage: script/build-cli.sh [PLATFORM ...]
Platforms: macos-arm64, macos-x86_64, linux-arm64, linux-x86_64.
Build mdt for all four platforms by default.
Requires the pinned Rust toolchain and target std libraries. Linux cross-builds
use cargo-zigbuild and Zig; native Linux builds may set CLI_NATIVE_LINUX=1.
Output: target/<Rust target>/release/mdt. Logs: logs/cli-builds/.
Uses --locked. CARGO_NET_OFFLINE=true enables cached dependency-only builds.
See docs/releasing.md for setup, packaging and publication.
HELP
    exit 0
fi
if [ -n "${CARGO_TARGET_DIR:-}" ] && [ "$CARGO_TARGET_DIR" != "$PWD/target" ]; then
    echo 'CLI releases must use this checkout’s target directory; unset CARGO_TARGET_DIR.' >&2
    exit 2
fi
export CARGO_TARGET_DIR="$PWD/target"
if [ "$#" = 0 ]; then
    set -- macos-arm64 macos-x86_64 linux-arm64 linux-x86_64
fi
mkdir -p logs/cli-builds
for platform in "$@"; do
    builder=build
    case "$platform" in
        macos-arm64) target=aarch64-apple-darwin ;;
        macos-x86_64) target=x86_64-apple-darwin ;;
        linux-arm64) target=aarch64-unknown-linux-musl ;;
        linux-x86_64) target=x86_64-unknown-linux-musl ;;
        *) echo "Unknown CLI platform: $platform" >&2; exit 2 ;;
    esac
    case "$platform" in
        macos-*) [ "$(uname -s)" = Darwin ] || { echo 'macOS builds require the Apple SDK on macOS.' >&2; exit 2; } ;;
        linux-*) [ "${CLI_NATIVE_LINUX:-0}" = 1 ] || builder=zigbuild ;;
    esac
    echo "Building mdt: $platform ($target)"
    cargo "$builder" --locked --release --target "$target" \
        -p mdt --bin mdt 2>&1 | tee "logs/cli-builds/mdt-$platform.log"
done
