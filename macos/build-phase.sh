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

# Layered Icon Composer icon: Assets.car for macOS 26's appearances, AppIcon.icns for older systems.
xcrun actool --compile "$TARGET_BUILD_DIR/$UNLOCALIZED_RESOURCES_FOLDER_PATH" --platform macosx \
  --minimum-deployment-target 12.0 --app-icon AppIcon \
  --output-partial-info-plist "$DERIVED_FILE_DIR/icon.plist" "$root/assets/icon/AppIcon.icon" >/dev/null
