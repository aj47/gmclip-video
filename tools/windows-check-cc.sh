#!/bin/sh
# Check-only stand-in for the Windows C compiler, archiver and resource compiler
# (tools/check_windows.sh).
#
# `cargo check --target x86_64-pc-windows-msvc` on a Mac still runs build scripts:
# libsqlite3-sys (rusqlite "bundled") compiles sqlite3.c with cc-rs, and the app's
# tauri-build compiles a Windows resource (icon, version) with embed-resource. There is
# no Windows SDK on the Mac, so neither can really run. `cargo check` only emits Rust
# metadata and never links, so their outputs are never read: this script creates the
# empty output file it is asked for and exits 0. As the resource compiler it answers
# `/?` like llvm-rc so embed-resource takes it.
#
# NEVER use this for `cargo build`: the result would link against empty libraries.
# The real Windows build uses MSVC (cl.exe / lib.exe / rc.exe) on the PC.
out=""
prev=""
for a in "$@"; do
    case "$a" in
        "/?") echo "OVERVIEW: Resource Converter (gmclip check-only stub; no-preprocess)"; exit 0 ;;
    esac
done
for a in "$@"; do
    case "$prev" in
        -o|/fo|/FO) out="$a" ;;
    esac
    case "$a" in
        -Fo*) out="${a#-Fo}" ;;
        /Fo*) out="${a#/Fo}" ;;
        -out:*) out="${a#-out:}" ;;
        /OUT:*) out="${a#/OUT:}" ;;
        -E) exit 0 ;; # cc-rs compiler-family probe: empty preprocessor output
    esac
    prev="$a"
done
if [ -z "$out" ]; then
    # `ar cq <lib> <objs…>` / `ar s <lib>` style
    case "$1" in
        *q*|*r*|s) out="$2" ;;
    esac
fi
if [ -n "$out" ]; then
    : >"$out"
fi
exit 0
