#!/bin/zsh
# Build Xenon.app and Xenon-<version>.dmg (release) in target/release.
#
# Signing: uses the "Developer ID Application" identity from your keychain when
# there is one (or XENON_SIGN_IDENTITY), otherwise signs ad hoc, which runs on
# this Mac but makes Gatekeeper warn on others.
# Notarizing: set XENON_NOTARY_PROFILE to a profile made with
#   xcrun notarytool store-credentials <profile> --apple-id <email> --team-id <team>
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p xenon
version=$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="xenon"))')

app=target/release/Xenon.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/xenon "$app/Contents/MacOS/xenon"

# Icon: every size macOS asks for, from the 1024px master.
iconset=$(mktemp -d)/Xenon.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z $size $size assets/icon/xenon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) assets/icon/xenon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Xenon.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Xenon</string>
  <key>CFBundleDisplayName</key><string>Xenon</string>
  <key>CFBundleIdentifier</key><string>software.carbon.xenon</string>
  <key>CFBundleExecutable</key><string>xenon</string>
  <key>CFBundleIconFile</key><string>Xenon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHumanReadableCopyright</key><string>© Carbon Software Ltd</string>
</dict>
</plist>
PLIST

identity=${XENON_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1)}
if [[ -n "$identity" ]]; then
  echo "signing with: $identity"
  codesign --force --options runtime --timestamp --sign "$identity" "$app"
else
  echo "no Developer ID Application certificate found; signing ad hoc (Gatekeeper will warn on other Macs)"
  codesign --force --sign - "$app"
fi
codesign --verify --strict "$app"

# Disk image with the app and an Applications shortcut.
dmg=target/release/Xenon-${version}.dmg
staging=$(mktemp -d)
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
rm -f "$dmg"
hdiutil create -quiet -volname "Xenon ${version}" -srcfolder "$staging" -ov -format UDZO "$dmg"
[[ -n "$identity" ]] && codesign --force --timestamp --sign "$identity" "$dmg"

if [[ -n "${XENON_NOTARY_PROFILE:-}" && -n "$identity" ]]; then
  xcrun notarytool submit "$dmg" --keychain-profile "$XENON_NOTARY_PROFILE" --wait
  xcrun stapler staple "$dmg"
fi

echo "built $app"
echo "built $dmg"
