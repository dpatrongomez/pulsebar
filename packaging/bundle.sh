#!/usr/bin/env bash
# Build PulseBar.app (universal when the x86_64 target is installed), sign with Hardened
# Runtime, optionally notarize, and produce a zip for the Homebrew cask.
#
#   packaging/bundle.sh                         # unsigned local bundle
#   SIGN_ID="Developer ID Application: …" packaging/bundle.sh
#   SIGN_ID=… NOTARY_PROFILE=pulsebar packaging/bundle.sh   # + notarize & staple
#     (create the profile once: xcrun notarytool store-credentials pulsebar …)
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT=dist
APP="$OUT/PulseBar.app"
rm -rf "$APP" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

targets=(aarch64-apple-darwin)
if rustup target list --installed | grep -q x86_64-apple-darwin; then targets+=(x86_64-apple-darwin); fi
bins=()
for t in "${targets[@]}"; do
  cargo build --release --target "$t"
  bins+=("target/$t/release/pulsebar")
done
if [ ${#bins[@]} -gt 1 ]; then
  lipo -create -output "$APP/Contents/MacOS/PulseBar" "${bins[@]}"
else
  echo "note: x86_64 target not installed (rustup target add x86_64-apple-darwin) — building arm64 only"
  cp "${bins[0]}" "$APP/Contents/MacOS/PulseBar"
fi
sed "s/__VERSION__/$VERSION/g" packaging/Info.plist > "$APP/Contents/Info.plist"
[ -f packaging/AppIcon.icns ] && cp packaging/AppIcon.icns "$APP/Contents/Resources/"

if [ -n "${SIGN_ID:-}" ]; then
  codesign --force --options runtime --timestamp --entitlements packaging/entitlements.plist --sign "$SIGN_ID" "$APP"
  codesign --verify --strict --verbose=2 "$APP"
else
  codesign --force --sign - "$APP"   # ad-hoc for local use
fi

ZIP="$OUT/PulseBar-$VERSION.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"

if [ -n "${NOTARY_PROFILE:-}" ]; then
  xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$APP"
  rm -f "$ZIP" && ditto -c -k --keepParent "$APP" "$ZIP"
fi

echo "built $APP ($(lipo -archs "$APP/Contents/MacOS/PulseBar"))"
echo "sha256 $(shasum -a 256 "$ZIP" | cut -d' ' -f1)  $ZIP"
