#!/usr/bin/env bash
# Build dist/OurDB.app.
#   --universal  one app for Apple Silicon and Intel Macs (needs: rustup target add x86_64-apple-darwin
#                or aarch64-apple-darwin, the one that is not this Mac's)
#   --dmg        also build dist/OurDB.dmg (drag-to-Applications disk image)
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
# Same minimum macOS for Rust and C code (SQLite) on both architectures.
export MACOSX_DEPLOYMENT_TARGET=11.0

UNIVERSAL=0
DMG=0
for arg in "$@"; do
  case "$arg" in
    --universal) UNIVERSAL=1 ;;
    --dmg) DMG=1 ;;
    *) echo "Unknown option: $arg"; exit 2 ;;
  esac
done

command -v cargo >/dev/null || { echo "Rust is missing. Install it: brew install rustup && rustup default stable"; exit 1; }

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

# Build the binary for this Mac, and for the other architecture with --universal.
cargo build --release -p ourdb
BIN=target/release/ourdb
if [ "$UNIVERSAL" = 1 ]; then
  case "$(uname -m)" in
    arm64) OTHER=x86_64-apple-darwin ;;
    *) OTHER=aarch64-apple-darwin ;;
  esac
  cargo build --release -p ourdb --target "$OTHER"
  mkdir -p target/universal
  lipo -create -output target/universal/ourdb "$BIN" "target/$OTHER/release/ourdb"
  BIN=target/universal/ourdb
fi

APP=dist/OurDB.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/OurDB"
cp assets/OurDB.icns "$APP/Contents/Resources/OurDB.icns"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>OurDB</string>
  <key>CFBundleDisplayName</key><string>OurDB</string>
  <key>CFBundleIdentifier</key><string>local.ourdb.app</string>
  <key>CFBundleExecutable</key><string>OurDB</string>
  <key>CFBundleIconFile</key><string>OurDB</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc signature: needed to run on Apple Silicon. It is not a Developer ID
# signature, so a downloaded copy needs "Open Anyway" once (see the DMG note).
codesign --force --deep --sign - "$APP"
echo "Built $APP ($(du -sh "$APP" | cut -f1), $(lipo -archs "$APP/Contents/MacOS/OurDB"))"

if [ "$DMG" = 1 ]; then
  WORK=$(mktemp -d)
  trap 'hdiutil detach -quiet "$WORK/mnt" 2>/dev/null || true; rm -rf "$WORK"' EXIT
  mkdir -p "$WORK/stage" "$WORK/mnt"
  cp -R "$APP" "$WORK/stage/"
  ln -s /Applications "$WORK/stage/Applications"
  cat > "$WORK/stage/If OurDB does not open.txt" <<'NOTE'
OurDB is free and open source, but it is not notarized by Apple.

Install:
  1. Drag OurDB to the Applications folder.
  2. Open OurDB from Applications.

If macOS says it cannot check OurDB or that OurDB was not opened:
  1. Click Done (do not move it to the Trash).
  2. Open System Settings > Privacy & Security.
  3. Scroll down. Next to "OurDB was blocked", click Open Anyway.
  4. Confirm with your password or Touch ID.
macOS asks this only the first time.

Or run this one command in Terminal:
  xattr -dr com.apple.quarantine /Applications/OurDB.app

Source code: https://github.com/xuanngoc/ourDBs
NOTE
  # Make a writable image, add the disk icon, then compress it.
  hdiutil create -quiet -volname OurDB -srcfolder "$WORK/stage" -fs HFS+ -format UDRW "$WORK/rw.dmg"
  hdiutil attach -quiet -nobrowse -noautoopen -mountpoint "$WORK/mnt" "$WORK/rw.dmg"
  cp assets/OurDB.icns "$WORK/mnt/.VolumeIcon.icns"
  SetFile -c icnC "$WORK/mnt/.VolumeIcon.icns" 2>/dev/null || true
  SetFile -a C "$WORK/mnt" 2>/dev/null || true
  hdiutil detach -quiet "$WORK/mnt"
  rm -f dist/OurDB.dmg
  hdiutil convert -quiet "$WORK/rw.dmg" -format UDZO -imagekey zlib-level=9 -o dist/OurDB.dmg
  echo "Built dist/OurDB.dmg ($(du -sh dist/OurDB.dmg | cut -f1))"
fi
