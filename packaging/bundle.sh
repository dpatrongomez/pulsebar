#!/usr/bin/env bash
# Build dist/PulseBar.app and an installable dist/PulseBar-<version>.dmg (drag to Applications).
#
#   packaging/bundle.sh            # app + dmg (same as `make dmg`)
#   packaging/bundle.sh app        # app only  (same as `make app`)
#
# Optional environment:
#   SIGN_ID="Developer ID Application: …"   sign with Hardened Runtime (default: ad-hoc)
#   NOTARY_PROFILE=pulsebar                  notarize + staple (needs SIGN_ID; create the profile
#                                            once with: xcrun notarytool store-credentials pulsebar …)
#
# The binary is universal (arm64 + x86_64) when both rustup targets are installed, otherwise it is
# built for this Mac's architecture only.
set -euo pipefail
cd "$(dirname "$0")/.."

what=${1:-dmg}
case "$what" in app|dmg) ;; *) echo "usage: $0 [app|dmg]" >&2; exit 2 ;; esac
if [ -n "${NOTARY_PROFILE:-}" ] && [ -z "${SIGN_ID:-}" ]; then
  echo "error: NOTARY_PROFILE requires SIGN_ID" >&2; exit 2
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT=dist
APP="$OUT/PulseBar.app"
DMG="$OUT/PulseBar-$VERSION.dmg"
BIN="$APP/Contents/MacOS/PulseBar"

# 1. Binary
installed=$(rustup target list --installed 2>/dev/null || true)
rm -rf "$APP" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
if grep -q aarch64-apple-darwin <<<"$installed" && grep -q x86_64-apple-darwin <<<"$installed"; then
  cargo build --release --target aarch64-apple-darwin --target x86_64-apple-darwin
  lipo -create -output "$BIN" target/{aarch64,x86_64}-apple-darwin/release/pulsebar
else
  echo "note: building for this Mac only (for universal: rustup target add aarch64-apple-darwin x86_64-apple-darwin)"
  cargo build --release
  cp target/release/pulsebar "$BIN"
fi

# 2. Bundle
sed "s/__VERSION__/$VERSION/g" packaging/Info.plist > "$APP/Contents/Info.plist"
[ -f packaging/AppIcon.icns ] && cp packaging/AppIcon.icns "$APP/Contents/Resources/"
# The OFL requires the licences to travel with the fonts embedded in the binary.
cp assets/fonts/*-OFL.txt "$APP/Contents/Resources/"

if [ -n "${SIGN_ID:-}" ]; then
  codesign --force --options runtime --timestamp --entitlements packaging/entitlements.plist --sign "$SIGN_ID" "$APP"
  codesign --verify --strict --verbose=2 "$APP"
else
  codesign --force --sign - "$APP"   # ad-hoc: runs on this Mac; other Macs need right-click → Open
fi
echo "built $APP ($(lipo -archs "$BIN"))"
[ "$what" = app ] && exit 0

# 3. Disk image: the app plus an /Applications shortcut to drag it onto
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp -R "$APP" "$stage/"
ln -s /Applications "$stage/Applications"
rm -f "$DMG"
hdiutil create -quiet -volname "PulseBar $VERSION" -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$DMG"

if [ -n "${SIGN_ID:-}" ]; then
  codesign --force --timestamp --sign "$SIGN_ID" "$DMG"
fi
if [ -n "${NOTARY_PROFILE:-}" ]; then
  # One submission covers both: the ticket lists the dmg and the app inside it.
  xcrun notarytool submit "$DMG" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$DMG"
  xcrun stapler staple "$APP"
fi

echo "built $DMG"
echo "sha256 $(shasum -a 256 "$DMG" | cut -d' ' -f1)"
