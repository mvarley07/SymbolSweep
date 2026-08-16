#!/bin/bash
set -euo pipefail

NOTARY_PROFILE="symbolsweep-notary"
REPO="$HOME/Desktop/SymbolSweep"

cd "$REPO"

echo "==> 1/4  Installing deps + building signed .app + .dmg (Tauri signs during bundle)"
npm install
npm run tauri build

DMG_PATH=$(ls src-tauri/target/release/bundle/dmg/*.dmg | head -n1)
APP_PATH="src-tauri/target/release/bundle/macos/SymbolSweep.app"

echo "==> Verifying the app signature before submitting"
codesign --verify --deep --strict --verbose=2 "$APP_PATH"

echo "==> 2/4  Submitting DMG to Apple for notarization (waits — a few min)"
echo "         DMG: $DMG_PATH"
xcrun notarytool submit "$DMG_PATH" --keychain-profile "$NOTARY_PROFILE" --wait

echo "==> 3/4  Stapling the notarization ticket to the DMG"
xcrun stapler staple "$DMG_PATH"

echo "==> 4/4  Final gatekeeper check"
spctl -a -t open --context context:primary-signature -v "$DMG_PATH" || true

echo ""
echo "DONE. Distributable DMG:"
echo "  $DMG_PATH"
