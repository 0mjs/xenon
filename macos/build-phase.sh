#!/bin/zsh
# Xcode build phase: build the Rust binary and place it, with the icon, inside the app bundle.
# Xcode signs the bundle afterwards.
set -euo pipefail
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
root="$SRCROOT/.."
cd "$root"

cargo build --release --locked -p xenon

mkdir -p "$(dirname "$TARGET_BUILD_DIR/$EXECUTABLE_PATH")" "$TARGET_BUILD_DIR/$UNLOCALIZED_RESOURCES_FOLDER_PATH"
cp target/release/xenon "$TARGET_BUILD_DIR/$EXECUTABLE_PATH"

# DuckDB's library, Apple silicon only, signed like the app (Xcode re-signs it on export).
frameworks="$TARGET_BUILD_DIR/$FRAMEWORKS_FOLDER_PATH"
mkdir -p "$frameworks"
./scripts/verify-duckdb.sh target/release/deps/libduckdb.dylib
# The GPLv3, and the licences and notices of everything built in.
resources="$TARGET_BUILD_DIR/$UNLOCALIZED_RESOURCES_FOLDER_PATH"
mkdir -p "$resources"
cp LICENSE "$resources/LICENSE"
python3 scripts/third-party-notices.py "$resources/THIRD-PARTY-NOTICES.txt"
lipo target/release/deps/libduckdb.dylib -thin arm64 -output "$frameworks/libduckdb.dylib"
if [[ -n "${EXPANDED_CODE_SIGN_IDENTITY:-}" ]]; then
  codesign --force --options runtime --sign "$EXPANDED_CODE_SIGN_IDENTITY" "$frameworks/libduckdb.dylib"
fi

# Layered Icon Composer icon: Assets.car for macOS 26's appearances, AppIcon.icns for older systems.
xcrun actool --compile "$TARGET_BUILD_DIR/$UNLOCALIZED_RESOURCES_FOLDER_PATH" --platform macosx \
  --minimum-deployment-target 12.0 --app-icon AppIcon \
  --output-partial-info-plist "$DERIVED_FILE_DIR/icon.plist" "$root/assets/icon/AppIcon.icon" >/dev/null
