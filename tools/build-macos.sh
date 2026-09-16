#!/bin/bash
set -euo pipefail
[[ "$(uname -s)" == Darwin ]] || { echo 'Run this script on macOS with Xcode Command Line Tools.' >&2; exit 1; }
repo="$(cd "$(dirname "$0")/.." && pwd -P)"
cd "$repo"
arch="$(uname -m)"
case "$arch" in
  arm64) target=aarch64-apple-darwin ;;
  x86_64) target=x86_64-apple-darwin ;;
  *) echo 'Unsupported macOS architecture' >&2; exit 1 ;;
esac
export MACOSX_DEPLOYMENT_TARGET=13.0
version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "xxtab"))')"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || exit 1
rustup target add "$target"
cargo build --release --locked --bin xxtab --target "$target"
mkdir -p "$repo/.tools" "$repo/dist/installers"
work="$(mktemp -d "$repo/.tools/macos-build.XXXXXX")"
trap '[[ "$work" == "$repo/.tools/macos-build."* ]] && rm -rf -- "$work"' EXIT
app="$work/image/xxtab.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$work/xxtab.iconset"
cp "target/$target/release/xxtab" "$app/Contents/MacOS/xxtab"
xcrun swiftc -O -parse-as-library -swift-version 5 -target "$arch-apple-macosx13.0" -framework AppKit macos/Xxtab.swift -o "$app/Contents/MacOS/xxtab-macos"
cp packaging/macos/Info.plist "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $version" "$app/Contents/Info.plist"
cp LICENSE "$app/Contents/Resources/LICENSE"
xcrun swift packaging/macos/make-icon.swift "$work/logo.png"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$work/logo.png" --out "$work/xxtab.iconset/icon_${size}x${size}.png" >/dev/null
  doubled=$((size * 2))
  sips -z "$doubled" "$doubled" "$work/logo.png" --out "$work/xxtab.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$work/xxtab.iconset" -o "$app/Contents/Resources/xxtab.icns"
identity="${MACOS_SIGNING_IDENTITY:--}"
sign_args=(--force --sign "$identity")
if [[ "$identity" != - ]]; then sign_args+=(--options runtime --timestamp); fi
codesign "${sign_args[@]}" "$app/Contents/MacOS/xxtab"
codesign "${sign_args[@]}" "$app"
codesign --verify --deep --strict "$app"
"$app/Contents/MacOS/xxtab-macos" --smoke-test
if [[ "${XXTAB_MACOS_LIFECYCLE_TEST:-0}" == 1 ]]; then
  python3 tests/macos_session.py "$app/Contents/MacOS/xxtab"
fi
ln -s /Applications "$work/image/Applications"
base="$repo/dist/installers/xxtab-$version-macos-$arch"
hdiutil create -ov -volname xxtab -srcfolder "$work/image" -format UDZO "$base.dmg"
if [[ -n "${MACOS_NOTARY_PROFILE:-}" ]]; then
  [[ "$identity" != - ]] || { echo 'Notarization requires a Developer ID signing identity.' >&2; exit 1; }
  xcrun notarytool submit "$base.dmg" --keychain-profile "$MACOS_NOTARY_PROFILE" --wait
  xcrun stapler staple "$base.dmg"
  xcrun stapler staple "$app"
fi
ditto -c -k --sequesterRsrc --keepParent "$app" "$base.app.zip"
for package in "$base.dmg" "$base.app.zip"; do
  (cd "$(dirname "$package")"; shasum -a 256 "$(basename "$package")") > "$package.sha256"
done
echo "Built: $base.dmg"
