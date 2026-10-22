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

# DuckDB's library from its archive, checked before it's unzipped (the duckdb crate's own
# download would unzip it first, with a zip crate that has advisories against it).
eval "$(./scripts/ci-duckdb.sh)"
cargo build --release --locked -p xenon
version=$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="xenon"))')

app=target/release/Xenon.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/xenon "$app/Contents/MacOS/xenon"

# DuckDB's library, Apple silicon only (its download holds Intel code too, twice the size).
mkdir -p "$app/Contents/Frameworks"
./scripts/verify-duckdb.sh "$DUCKDB_LIB_DIR/libduckdb.dylib"
# The GPLv3, and the licences and notices of everything built in.
cp LICENSE "$app/Contents/Resources/LICENSE"
python3 scripts/third-party-notices.py "$app/Contents/Resources/THIRD-PARTY-NOTICES.txt"
lipo "$DUCKDB_LIB_DIR/libduckdb.dylib" -thin arm64 -output "$app/Contents/Frameworks/libduckdb.dylib"

# Icon: compile the layered Icon Composer icon. macOS 26 renders its light, dark, clear and
# tinted looks from Assets.car; AppIcon.icns is the fallback for older systems.
xcrun actool --compile "$PWD/$app/Contents/Resources" --platform macosx --minimum-deployment-target 12.0 \
  --app-icon AppIcon --output-partial-info-plist "$(mktemp -d)/icon.plist" "$PWD/assets/icon/AppIcon.icon" >/dev/null

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Xenon</string>
  <key>CFBundleDisplayName</key><string>Xenon</string>
  <key>CFBundleIdentifier</key><string>software.carbon.xenon</string>
  <key>CFBundleExecutable</key><string>xenon</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleIconName</key><string>AppIcon</string>
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
  codesign --force --options runtime --timestamp --sign "$identity" "$app/Contents/Frameworks/libduckdb.dylib"
  codesign --force --options runtime --timestamp --sign "$identity" "$app"
else
  echo "no Developer ID Application certificate found; signing ad hoc (Gatekeeper will warn on other Macs)"
  codesign --force --sign - "$app/Contents/Frameworks/libduckdb.dylib"
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
