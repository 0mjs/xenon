#!/bin/zsh
# Xcode build phase: build the Rust binary and place it, with the icon, inside the app bundle.
# Xcode signs the bundle afterwards.
set -euo pipefail
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
root="$SRCROOT/.."
cd "$root"

cargo build --release -p xenon

mkdir -p "$(dirname "$TARGET_BUILD_DIR/$EXECUTABLE_PATH")" "$TARGET_BUILD_DIR/$UNLOCALIZED_RESOURCES_FOLDER_PATH"
cp target/release/xenon "$TARGET_BUILD_DIR/$EXECUTABLE_PATH"

iconset="$DERIVED_FILE_DIR/Xenon.iconset"
rm -rf "$iconset" && mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z $size $size assets/icon/xenon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) assets/icon/xenon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$TARGET_BUILD_DIR/$UNLOCALIZED_RESOURCES_FOLDER_PATH/Xenon.icns"
