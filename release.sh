#!/bin/bash
set -euo pipefail

NOTARY_PROFILE="symbolsweep-notary"
REPO="$HOME/Desktop/SymbolSweep"
RELEASES_REPO="mvarley07/symbolsweep-releases"

cd "$REPO"

VERSION=$(python3 -c "import json; print(json.load(open('src-tauri/tauri.conf.json'))['version'])")
NOTES="${1:-SymbolSweep ${VERSION}}"

echo "==> 1/6  Installing deps + building signed .app + .dmg (Tauri signs during bundle)"
npm install
npm run tauri build

DMG_PATH=$(ls src-tauri/target/release/bundle/dmg/*.dmg | head -n1)
APP_PATH="src-tauri/target/release/bundle/macos/SymbolSweep.app"

echo "==> Verifying the app signature before submitting"
codesign --verify --deep --strict --verbose=2 "$APP_PATH"

echo "==> 2/6  Submitting DMG to Apple for notarization (waits — a few min)"
echo "         DMG: $DMG_PATH"
xcrun notarytool submit "$DMG_PATH" --keychain-profile "$NOTARY_PROFILE" --wait

echo "==> 3/6  Stapling the notarization ticket to the DMG"
xcrun stapler staple "$DMG_PATH"

echo "==> 4/6  Final gatekeeper check"
spctl -a -t open --context context:primary-signature -v "$DMG_PATH" || true

echo "==> 5/6  Writing latest.json updater manifest"
BUNDLE_DIR="src-tauri/target/release/bundle/macos"
TARBALL="$BUNDLE_DIR/SymbolSweep.app.tar.gz"
SIG_FILE="$BUNDLE_DIR/SymbolSweep.app.tar.gz.sig"

[ -f "$TARBALL" ]  || { echo "ERROR: updater tarball not found: $TARBALL" >&2; exit 1; }
[ -f "$SIG_FILE" ] || { echo "ERROR: updater signature not found: $SIG_FILE" >&2; exit 1; }

# The .sig file is already base64 minisign output — use it verbatim, never re-encode.
SIGNATURE=$(cat "$SIG_FILE")
case "$SIGNATURE" in
  dW50cnVzdGVk*) ;;
  *) echo "ERROR: signature does not start with dW50cnVzdGVk — wrong file or double-encoded" >&2; exit 1 ;;
esac

PUB_DATE=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
DOWNLOAD_URL="https://github.com/${RELEASES_REPO}/releases/download/v${VERSION}/SymbolSweep.app.tar.gz"

VERSION="$VERSION" NOTES="$NOTES" PUB_DATE="$PUB_DATE" SIGNATURE="$SIGNATURE" DOWNLOAD_URL="$DOWNLOAD_URL" \
python3 - <<'EOF'
import json, os
manifest = {
    "version": os.environ["VERSION"],
    "notes": os.environ["NOTES"],
    "pub_date": os.environ["PUB_DATE"],
    "platforms": {
        "darwin-aarch64": {
            "signature": os.environ["SIGNATURE"],
            "url": os.environ["DOWNLOAD_URL"],
        }
    },
}
with open("latest.json", "w") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
EOF
cat latest.json

echo "==> 6/6  Creating GitHub release v${VERSION} on ${RELEASES_REPO}"
gh release create "v${VERSION}" \
  --repo "$RELEASES_REPO" \
  --title "SymbolSweep ${VERSION}" \
  --notes "${NOTES}" \
  latest.json "$TARBALL" "$SIG_FILE" "$DMG_PATH"

echo ""
echo "DONE. Release: https://github.com/${RELEASES_REPO}/releases/tag/v${VERSION}"
echo "Distributable DMG:"
echo "  $DMG_PATH"
