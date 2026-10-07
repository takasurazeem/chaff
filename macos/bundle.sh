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
    <key>LSMinimumSystemVersion</key><string>26.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <!-- A photo tool is a document-less app: no automatic window restoration, and it should
         not appear in the Dock as a background agent. -->
    <key>LSApplicationCategoryType</key><string>public.app-category.photography</string>
    <key>NSHumanReadableCopyright</key><string>Chaff contributors</string>
</dict>
</plist>
PLIST

# ---------------------------------------------------------------------------------------
# Signing, and exactly where the line is.
#
# Three tiers, and this machine can reach the second:
#
#   1. **Ad-hoc** (`--sign -`). Runs here. Gatekeeper refuses it anywhere else, and macOS
#      reports it as "damaged" rather than "unidentified developer" — which is a worse message
#      for the same problem.
#
#   2. **Apple Development.** Identifies the developer and runs on machines in the same team.
#      What this script uses when the certificate is present. **Still not distributable** — it
#      is for development, and Gatekeeper on a stranger's Mac refuses it.
#
#   3. **Developer ID Application + notarization.** The only thing that ships. Requires a paid
#      Apple Developer Program membership, and the certificate must be created by the account
#      holder — it cannot be generated here.
#
# So: this produces a build that is signed as well as it can be on this machine, and the last
# step needs something only the user can buy. See docs/PACKAGING.md.
# ---------------------------------------------------------------------------------------
IDENTITY="${CHAFF_SIGN_IDENTITY:-}"
if [ -z "$IDENTITY" ]; then
    # One `security` call, parsed in awk.
    #
    # The first version piped through `grep | head -1`, which is where it broke: `head` closes
    # the pipe, `grep` takes SIGPIPE, and **`pipefail` turns that into a failing command
    # substitution** — so the assignment silently produced nothing and the build fell back to
    # ad-hoc while the grep worked perfectly by hand. A one-line test that passes in a terminal
    # and fails in a script is usually this.
    #
    # `find-identity` lists Developer ID first when both exist, and the order is what the
    # priority here relies on.
    IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | awk '
        /"Developer ID Application: / { if (d == "") { d = $0 } }
        /"Apple Development: /        { if (a == "") { a = $0 } }
        END {
            line = (d != "") ? d : a
            if (line != "" && match(line, /"[^"]*"/)) {
                print substr(line, RSTART + 1, RLENGTH - 2)
            }
        }')"
fi

if [ -n "$IDENTITY" ]; then
    echo "  signing: $IDENTITY"
    # Hardened runtime is required for notarization, and it costs nothing to enable now — a
    # build that is notarized later should not also need its signing changed.
    codesign --force --options runtime --timestamp --sign "$IDENTITY" "$APP" 2>&1 | sed 's/^/  /' \
        || echo "  (signing failed — falling back to ad-hoc)"
else
    echo "  signing: ad-hoc (no Developer ID or Apple Development certificate found)"
fi
# Verify rather than assume. A signature that silently did not apply is worse than none,
# because the next step believes it happened.
codesign --verify --deep --strict "$APP" 2>&1 | sed 's/^/  /' && echo "  signature verifies"

echo "built: $APP ($(du -sh "$APP" | cut -f1))"
echo "commit: $SHA"
