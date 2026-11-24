#!/usr/bin/env bash
# Build dist/OurDB.app. With --dmg, also build dist/OurDB.dmg.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

command -v cargo >/dev/null || { echo "Rust is missing. Install it: brew install rustup && rustup default stable"; exit 1; }

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release -p ourdb

APP=dist/OurDB.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/ourdb "$APP/Contents/MacOS/OurDB"
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

# Ad-hoc signature: enough for a locally built app on Apple Silicon.
codesign --force --deep --sign - "$APP"
echo "Built $APP ($(du -sh "$APP" | cut -f1))"

if [ "${1:-}" = "--dmg" ]; then
  STAGE=$(mktemp -d)
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  rm -f dist/OurDB.dmg
  hdiutil create -quiet -volname OurDB -srcfolder "$STAGE" -ov -format UDZO dist/OurDB.dmg
  rm -rf "$STAGE"
  echo "Built dist/OurDB.dmg"
fi
