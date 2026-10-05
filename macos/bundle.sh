#!/usr/bin/env bash
# Package the SwiftUI binary as a `.app`.
#
# # Why this exists
#
# `swift build` produces a bare executable. A bare executable is not something a person can
# launch: no Dock icon, no window activation, no way to double-click it. The 5 MB binary in
# `.build/release/` is a developer artefact and this turns it into an application.
set -euo pipefail

CONFIG="${1:-release}"
APP="build/Chaff.app"
BIN=".build/$CONFIG/Chaff"

[ -x "$BIN" ] || { echo "not built: $BIN — run 'make build' first"; exit 1; }

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/Chaff"

# The commit, so a build in the Dock can be identified the same way the Tauri one can.
SHA="$(git -C .. rev-parse --short HEAD 2>/dev/null || echo unknown)"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Chaff</string>
    <key>CFBundleDisplayName</key><string>Chaff</string>
    <key>CFBundleIdentifier</key><string>com.takasurazeem.chaff.native</string>
    <key>CFBundleExecutable</key><string>Chaff</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>$SHA</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <!-- A photo tool is a document-less app: no automatic window restoration, and it should
         not appear in the Dock as a background agent. -->
    <key>LSApplicationCategoryType</key><string>public.app-category.photography</string>
    <key>NSHumanReadableCopyright</key><string>Chaff contributors</string>
</dict>
</plist>
PLIST

# Ad-hoc signed so macOS will run it locally. **Not** a Developer ID signature: Gatekeeper
# will still refuse it from anywhere but this machine, which is issue #68.
codesign --force --sign - "$APP" 2>/dev/null || echo "  (ad-hoc signing skipped)"

echo "built: $APP ($(du -sh "$APP" | cut -f1))"
echo "commit: $SHA"
