#!/bin/zsh
# Build a notarized Xenon release: target/xcode/Xenon-<version>.dmg
#
# Uses Xcode's signed-in account (Xcode → Settings → Accounts, team H8USX7F5CF): xcodebuild
# signs with a cloud-managed Developer ID certificate and submits to Apple's notary service,
# so no certificates or app-specific passwords are needed on this Mac.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
# A release is built from exactly what's committed (the commit it's tagged at),
# with the locked dependencies and the pinned toolchain (rust-toolchain.toml).
# Untracked files count: cargo would build an untracked build.rs into it.
if [[ -n "$(git status --porcelain)" ]]; then
  echo "commit, stash or remove your changes first: a release is built from a commit" >&2
  git status --short >&2
  exit 1
fi
tag=$(git describe --exact-match --tags HEAD 2>/dev/null || true)
if [[ "$tag" != "v$version" ]]; then
  echo "tag this commit v$version first (it's ${tag:-not tagged}): a release is built from its tag" >&2
  exit 1
fi
if [[ "$(git ls-remote origin "refs/tags/v$version" | cut -f1)" != "$(git rev-parse "v$version")" ]]; then
  echo "push the tag first: origin's v$version isn't this one" >&2
  exit 1
fi
echo "commit $(git rev-parse --short HEAD), $(rustc --version)"
out=target/xcode
archive=$out/Xenon.xcarchive
mkdir -p "$out"
rm -rf "$archive" "$out/notarized"

echo "archiving Xenon $version…"
xcodebuild archive -project macos/Xenon.xcodeproj -scheme Xenon -configuration Release \
  -archivePath "$archive" -derivedDataPath "$out/derived" -allowProvisioningUpdates \
  MARKETING_VERSION="$version" CURRENT_PROJECT_VERSION="$version" > "$out/archive.log" 2>&1 \
  || { tail -20 "$out/archive.log"; exit 1; }

echo "signing with Developer ID and uploading for notarization…"
xcodebuild -exportArchive -archivePath "$archive" -exportOptionsPlist macos/ExportOptions.plist \
  -exportPath "$out/upload" -allowProvisioningUpdates > "$out/export.log" 2>&1 \
  || { tail -20 "$out/export.log"; exit 1; }

echo "waiting for Apple's notary service…"
for attempt in {1..60}; do
  if xcodebuild -exportNotarizedApp -archivePath "$archive" -exportPath "$out/notarized" \
      > "$out/notarized.log" 2>&1; then
    break
  fi
  if ! grep -q "processing" "$out/notarized.log"; then
    tail -20 "$out/notarized.log"
    exit 1
  fi
  (( attempt == 60 )) && { echo "still not notarized after 30 minutes"; exit 1; }
  sleep 30
done

app=$out/notarized/Xenon.app
xcrun stapler validate "$app"
spctl --assess --type execute --verbose "$app"

dmg=$out/Xenon-${version}.dmg
staging=$(mktemp -d)
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
rm -f "$dmg"
hdiutil create -quiet -volname "Xenon ${version}" -srcfolder "$staging" -ov -format UDZO "$dmg"
echo "built $dmg"
# For the release notes: what to check the download against.
echo "sha256 $(shasum -a 256 "$dmg" | cut -d' ' -f1)  Xenon-${version}.dmg"
echo "signed by team H8USX7F5CF (codesign -dv Xenon.app)"
