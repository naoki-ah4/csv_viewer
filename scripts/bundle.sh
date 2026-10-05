#!/bin/bash
# macOS の .app バンドルを作る。
#
#   scripts/bundle.sh           # target/bundle/CSV Viewer.app を作る
#   open "target/bundle/CSV Viewer.app" --args samples/utf8.csv
#
# アイコンは assets/icon.icns（作り方は scripts/make_icon.sh）。ad-hoc 署名（`-`）なので、手元で使う分には動くが、
# 配布用の署名や公証（notarization）はしていない。
#
# Finder の「このアプリケーションで開く」に .csv で出てくるようにするには、
# できあがった .app を /Applications か ~/Applications に置くのが確実。
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="CSV Viewer"
BUNDLE_ID="net.naoki-workspace.csv-viewer" # src/settings.rs の APP_ID と同じにする
BIN_NAME="csv_viewer"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"

cargo build --release

APP="target/bundle/${APP_NAME}.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/release/${BIN_NAME}" "$APP/Contents/MacOS/${BIN_NAME}"
cp assets/icon.icns "$APP/Contents/Resources/icon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleIconFile</key>
    <string>icon</string>
    <key>CFBundleExecutable</key>
    <string>${BIN_NAME}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key>
            <string>CSV</string>
            <key>CFBundleTypeRole</key>
            <string>Viewer</string>
            <!-- 既定のアプリを奪わず、「このアプリケーションで開く」の候補に出る。 -->
            <key>LSHandlerRank</key>
            <string>Alternate</string>
            <key>CFBundleTypeExtensions</key>
            <array>
                <string>csv</string>
            </array>
            <key>LSItemContentTypes</key>
            <array>
                <string>public.comma-separated-values-text</string>
            </array>
        </dict>
    </array>
</dict>
</plist>
PLIST

plutil -lint "$APP/Contents/Info.plist"
codesign --force --sign - "$APP"
codesign --verify --verbose=1 "$APP"
echo "Built: $APP (version ${VERSION})"
