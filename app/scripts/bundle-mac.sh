#!/bin/sh
# Builds gmclip.app (macOS, unsigned: ad-hoc codesign only — no Developer ID, no
# notarisation, nothing uploaded). The layout is the Tauri bundler's: Contents/Info.plist,
# MacOS/gmclip-app, Resources/icon.icns. Tauri's CLI is not in the lockfile (adding it is
# a supply-chain change for AJ), so this script assembles the bundle itself.
#
#   app/scripts/bundle-mac.sh [OUT_DIR]     (default ~/Development/gmclip-artifacts/app)
#
# Release builds never carry the dev bridge (build.rs refuses it; this script checks the
# binary with `cargo xt assert-no-dev-bridge`). The release target is removed afterwards
# (disk), keeping only the bundle.
set -eu
HERE=$(cd "$(dirname "$0")/.." && pwd)
ROOT=$(cd "$HERE/.." && pwd)
OUT=${1:-$HOME/Development/gmclip-artifacts/app}
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0

echo "== UI (frozen lockfile)"
(cd "$HERE/ui" && pnpm install --frozen-lockfile --ignore-scripts >/dev/null && pnpm run build >/dev/null)

echo "== release binary (custom-protocol, no dev bridge)"
(cd "$HERE/src-tauri" && CARGO_PROFILE_RELEASE_DEBUG=false cargo build --release --locked --features custom-protocol)
BIN="$HERE/src-tauri/target/release/gmclip-app"
(cd "$ROOT" && cargo xt assert-no-dev-bridge "$BIN")

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$HERE/src-tauri/Cargo.toml" | head -1)
APP="$OUT/gmclip.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/gmclip-app"

echo "== fonts (the faces captions use, hash-checked; resolved from the bundle at run time)"
"$BIN" bundle-fonts "$APP/Contents/Resources/fonts"

echo "== icon"
ICONSET=$(mktemp -d)/gmclip.iconset
mkdir -p "$ICONSET"
for s in 16 32 128 256; do
  sips -z $s $s "$HERE/src-tauri/icons/icon.png" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z $d $d "$HERE/src-tauri/icons/icon.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
cp "$HERE/src-tauri/icons/icon.png" "$ICONSET/icon_512x512.png"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/icon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>gmclip</string>
  <key>CFBundleExecutable</key><string>gmclip-app</string>
  <key>CFBundleIconFile</key><string>icon.icns</string>
  <key>CFBundleIdentifier</key><string>dev.gmclip.app</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>gmclip</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.video</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSCameraUsageDescription</key><string>gmclip records your camera as the face file of a recording.</string>
  <key>NSMicrophoneUsageDescription</key><string>gmclip records your voice into the face file of a recording.</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

echo "== ad-hoc codesign (no identity)"
# the requirement names the bundle id, so privacy grants survive a rebuild
codesign --force --deep --sign - --timestamp=none -r='designated => identifier "dev.gmclip.app"' "$APP"
codesign --verify --deep --strict "$APP"
(cd "$ROOT" && cargo xt assert-no-dev-bridge "$APP")

echo "== clean the release target (disk)"
rm -rf "$HERE/src-tauri/target/release"
du -sh "$APP"
