#!/bin/sh
# Wraps the debug build in "gmclip dev.app" (ad-hoc signed) and opens it, so macOS asks
# gmclip itself for the camera and microphone. A bare `target/debug/gmclip-app` started
# from a terminal is asked for on behalf of that terminal (or bb), which may never show
# the sheet: the recording then has no face file (decision 0045).
#
#   app/scripts/dev-app-mac.sh [-- gmclip-app args…]
#
# Builds nothing: run `cargo build --features custom-protocol` in app/src-tauri first. The
# ad-hoc signature's requirement names the bundle id rather than the build's hash, so the
# camera and other grants survive a rebuild (with the hash, macOS refuses them silently).
set -eu
HERE=$(cd "$(dirname "$0")/.." && pwd)
BIN="$HERE/src-tauri/target/debug/gmclip-app"
APP="$HERE/src-tauri/target/gmclip dev.app"
[ -x "$BIN" ] || { echo "no $BIN: build it first" >&2; exit 1; }
[ "${1:-}" = "--" ] && shift

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/gmclip-app"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>gmclip-app</string>
  <key>CFBundleIdentifier</key><string>dev.gmclip.app.dev</string>
  <key>CFBundleName</key><string>gmclip dev</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.0.0</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSCameraUsageDescription</key><string>gmclip records your camera as the face file of a recording.</string>
  <key>NSMicrophoneUsageDescription</key><string>gmclip records your voice into the face file of a recording.</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
codesign --force --sign - --timestamp=none -r='designated => identifier "dev.gmclip.app.dev"' "$APP"
# stdout and stderr go to a log, since `open` detaches the app from this terminal
LOG="${TMPDIR:-/tmp}/gmclip-dev.log"
open -n "$APP" --stdout "$LOG" --stderr "$LOG" --args "$@"
echo "opened $APP · log: $LOG"
