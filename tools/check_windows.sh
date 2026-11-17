#!/bin/sh
# Cross-checks the Windows build from the Mac (decisions/0019): type-checks and lints
# every crate, test and the app for a Windows target without a Windows SDK.
#
#   tools/check_windows.sh                          # x86_64-pc-windows-msvc
#   tools/check_windows.sh aarch64-pc-windows-msvc
#   tools/check_windows.sh x86_64-pc-windows-msvc --no-app
#
# `cargo check` emits metadata only and never links, so it needs no MSVC. Build scripts
# still run: libsqlite3-sys compiles sqlite3.c and the app's tauri-build compiles a
# Windows resource. tools/windows-check-cc.sh stands in for cl/lib/rc and writes empty
# outputs; it is set for this target only (CC_/AR_/RC_<target>) and only here. Never
# use it for `cargo build`. The real build is on the Windows PC (MSVC Build Tools).
#
# The app checks into the root target dir (override with CARGO_TARGET_DIR) to save disk.
set -eu
cd "$(dirname "$0")/.."
ROOT=$PWD
TARGET=x86_64-pc-windows-msvc
APP=1
for a in "$@"; do
    case "$a" in
        --no-app) APP=0 ;;
        -*) echo "usage: tools/check_windows.sh [target] [--no-app]" >&2; exit 2 ;;
        *) TARGET=$a ;;
    esac
done
case "$TARGET" in
    *-windows-msvc) ;;
    *) echo "not a Windows MSVC target: $TARGET" >&2; exit 2 ;;
esac
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
STUB="$ROOT/tools/windows-check-cc.sh"
T=$(printf '%s' "$TARGET" | sed 's/-/_/g')
stub() { env "CC_$T=$STUB" "AR_$T=$STUB" "RC_$T=$STUB" "$@"; }

echo "== workspace: cargo check --all-targets ($TARGET)"
stub cargo check --workspace --all-targets --locked --target "$TARGET"
echo "== workspace: cargo clippy --all-targets -D warnings ($TARGET)"
stub cargo clippy --workspace --all-targets --locked --target "$TARGET" -- -D warnings
if [ "$APP" = 1 ]; then
    cd "$ROOT/app/src-tauri"
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
    echo "== app: cargo check ($TARGET)"
    stub cargo check --locked --target "$TARGET"
    echo "== app: cargo clippy --all-targets -D warnings ($TARGET)"
    stub cargo clippy --locked --all-targets --target "$TARGET" -- -D warnings
fi
echo "windows check ok: $TARGET"
