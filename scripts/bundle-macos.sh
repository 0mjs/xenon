#!/bin/zsh
# Build Xenon.app (release) with its icon: target/release/Xenon.app
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p xenon

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

version=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="xenon"))')
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
</dict>
</plist>
PLIST

echo "built $app"
