#!/usr/bin/env bash
# packaging/macos/package.sh — Build, sign, notarize, staple, and DMG SideHuddle.app
#
# Usage:
#   bash packaging/macos/package.sh [ARCH] [VERSION]
#
# Defaults:
#   ARCH    = arm64
#   VERSION = latest git tag (or "0.0.0")
#
# Required environment variables:
#   APPLE_CERTIFICATE_P12       base64-encoded .p12 Developer ID Application cert
#   APPLE_CERTIFICATE_PASSWORD  password that unlocks the .p12
#   APPLE_APP_PASSWORD          app-specific password from appleid.apple.com

set -euo pipefail

# ── Identity (non-secret — visible in the signed binary anyway) ───────────────
APPLE_DEVELOPER_ID="Developer ID Application: Kenneth Chau (S7LTAD7MEA)"
APPLE_TEAM_ID="S7LTAD7MEA"
APPLE_ID="ken@gizzar.com"
BUNDLE_ID="com.ms.side-huddle"

# ── Args / defaults ───────────────────────────────────────────────────────────
ARCH="${1:-arm64}"
VERSION="${2:-$(git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//' || echo "0.0.0")}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APP_DIR="$REPO_ROOT/dist/SideHuddle.app"
DMG_OUT="$REPO_ROOT/dist/SideHuddle-${VERSION}.dmg"
ENTITLEMENTS="$REPO_ROOT/tools/bundle/side-huddle.entitlements"
INFO_PLIST_TPL="$REPO_ROOT/tools/bundle/Info.plist"
ICNS="$REPO_ROOT/tools/bundle/SideHuddle.icns"
GO_LIB_DIR="$REPO_ROOT/bindings/go/lib/darwin_${ARCH}"

echo "==> SideHuddle packaging — version $VERSION / $ARCH"
echo "    Bundle ID   : $BUNDLE_ID"
echo "    Developer ID: $APPLE_DEVELOPER_ID"

# ── Certificate import ────────────────────────────────────────────────────────
echo ""
echo "==> Importing Developer ID certificate..."
echo "$APPLE_CERTIFICATE_P12" | base64 --decode > /tmp/sidehuddle-cert.p12

security create-keychain -p "" sidehuddle-build.keychain
security default-keychain -s sidehuddle-build.keychain
security unlock-keychain   -p "" sidehuddle-build.keychain
security import /tmp/sidehuddle-cert.p12              \
    -k sidehuddle-build.keychain                       \
    -P "$APPLE_CERTIFICATE_PASSWORD"                   \
    -T /usr/bin/codesign                               \
    -T /usr/bin/security
security set-key-partition-list                        \
    -S apple-tool:,apple:                              \
    -s -k "" sidehuddle-build.keychain
rm /tmp/sidehuddle-cert.p12

echo "    Certificate imported."

# ── Build ─────────────────────────────────────────────────────────────────────
echo ""
echo "==> Building SideHuddle $VERSION..."
cd "$REPO_ROOT"

cargo build --release -p side-huddle
cp target/release/libside_huddle.a "$GO_LIB_DIR/libside_huddle.a"

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
go build -a -o "$APP_DIR/Contents/MacOS/SideHuddle" ./cmd/demo

cp "$INFO_PLIST_TPL"  "$APP_DIR/Contents/Info.plist"
cp "$ICNS"            "$APP_DIR/Contents/Resources/SideHuddle.icns"
plutil -replace CFBundleIdentifier          -string "$BUNDLE_ID" "$APP_DIR/Contents/Info.plist"
plutil -replace CFBundleShortVersionString  -string "$VERSION"   "$APP_DIR/Contents/Info.plist"
plutil -replace CFBundleVersion             -string "$VERSION"   "$APP_DIR/Contents/Info.plist"

echo "    Binary built."

# ── Code sign ─────────────────────────────────────────────────────────────────
echo ""
echo "==> Signing with Developer ID (Hardened Runtime)..."
codesign --deep --force --options runtime \
    --entitlements "$ENTITLEMENTS"        \
    --sign         "$APPLE_DEVELOPER_ID"  \
    --timestamp                           \
    "$APP_DIR"
codesign --verify --deep --strict "$APP_DIR"
echo "    Signature OK."

# ── Notarize ──────────────────────────────────────────────────────────────────
echo ""
echo "==> Submitting to Apple notary service (may take a few minutes)..."
NOTARIZE_ZIP="/tmp/SideHuddle-notarize.zip"
ditto -c -k --keepParent "$APP_DIR" "$NOTARIZE_ZIP"
xcrun notarytool submit "$NOTARIZE_ZIP" \
    --apple-id "$APPLE_ID"             \
    --password "$APPLE_APP_PASSWORD"   \
    --team-id  "$APPLE_TEAM_ID"        \
    --wait                             \
    --timeout  10m
rm "$NOTARIZE_ZIP"

# ── Staple ────────────────────────────────────────────────────────────────────
echo ""
echo "==> Stapling notarization ticket..."
xcrun stapler staple   "$APP_DIR"
xcrun stapler validate "$APP_DIR"
echo "    Staple OK."

# ── DMG ───────────────────────────────────────────────────────────────────────
echo ""
echo "==> Creating DMG..."
which create-dmg >/dev/null 2>&1 || brew install create-dmg

rm -f "$DMG_OUT"
create-dmg                                      \
    --volname      "SideHuddle $VERSION"         \
    --volicon      "$ICNS"                       \
    --window-pos   200 120                       \
    --window-size  560 400                       \
    --icon-size    100                           \
    --icon         "SideHuddle.app" 140 200      \
    --hide-extension "SideHuddle.app"            \
    --app-drop-link 420 200                      \
    "$DMG_OUT"                                   \
    "$(dirname "$APP_DIR")"

codesign --sign "$APPLE_DEVELOPER_ID" --timestamp "$DMG_OUT"
echo "    DMG signed."

# ── Cleanup ───────────────────────────────────────────────────────────────────
security delete-keychain sidehuddle-build.keychain

echo ""
echo "✓ Done: $DMG_OUT"
