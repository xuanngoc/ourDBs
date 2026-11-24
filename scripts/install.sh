#!/usr/bin/env bash
# Build OurDB.app and copy it to /Applications (or ~/Applications when
# /Applications is not writable).
set -euo pipefail
cd "$(dirname "$0")/.."
./scripts/bundle-macos.sh
DEST=/Applications
[ -w "$DEST" ] || { DEST="$HOME/Applications"; mkdir -p "$DEST"; }
rm -rf "$DEST/OurDB.app"
ditto dist/OurDB.app "$DEST/OurDB.app"
echo "Installed $DEST/OurDB.app. Open it from Launchpad or run: open \"$DEST/OurDB.app\""
