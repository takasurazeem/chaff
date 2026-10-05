#!/usr/bin/env bash
# Build, install, and **prove** what landed.
#
# Written after the user dragged a DMG, believed it had replaced the application, and was
# running a binary from seven minutes earlier. A build that is installed by hand and
# verified by eye is a build nobody can trust, and the failure is silent: the app runs, it
# just runs the old code.
#
# Undo: nothing. It replaces /Applications/Chaff.app, which is the point.
set -euo pipefail

cd "$(dirname "$0")/../.."
APP="target/release/bundle/macos/Chaff.app"
DEST="/Applications/Chaff.app"

echo "=== building ==="
pnpm tauri build 2>&1 | grep -E "Built application|^error" || true

if [ ! -x "$APP/Contents/MacOS/chaff" ]; then
  echo "no binary at $APP — the build failed" >&2
  exit 1
fi

echo
echo "=== installing ==="
pkill -f "Chaff.app" 2>/dev/null || true
sleep 1
rm -rf "$DEST"
cp -R "$APP" "$DEST"
xattr -dr com.apple.quarantine "$DEST" 2>/dev/null || true

echo
echo "=== verifying ==="
BUILT=$(shasum -a 256 "$APP/Contents/MacOS/chaff" | cut -d' ' -f1)
INSTALLED=$(shasum -a 256 "$DEST/Contents/MacOS/chaff" | cut -d' ' -f1)
printf '  built:     %s\n' "${BUILT:0:32}..."
printf '  installed: %s\n' "${INSTALLED:0:32}..."
if [ "$BUILT" != "$INSTALLED" ]; then
  echo "  MISMATCH — the installed binary is not the one just built" >&2
  exit 1
fi
echo "  match"

# The commit compiled into the binary, read back out of it.
#
# **This is the check that survives someone copying the wrong DMG**, which is exactly what
# happened: the user dragged an image, believed it had replaced the application, and was
# running a binary seven minutes older.
#
# Searches for the *actual* HEAD rather than a generic hex pattern. An earlier version
# grepped for anything hex-looking and confidently reported "0123456789" — a string from
# some dependency — while the real stamp went unread. A verification that can match the
# wrong thing is not a verification.
HEAD_SHA=$(git rev-parse --short HEAD 2>/dev/null || echo "")
if [ -n "$HEAD_SHA" ]; then
  # **Captured first, matched second — not piped.**
  #
  # `strings ... | grep -q` looks obvious and is wrong under `set -o pipefail`: `grep -q`
  # exits the instant it matches, which closes the pipe and gives `strings` a SIGPIPE
  # (141). The pipeline then reports 141, the `if` sees failure, and a successful match is
  # reported as NOT FOUND. A verification that lies is worse than no verification, and
  # this one lied twice before the cause was found.
  BIN_STRINGS=$(strings "$DEST/Contents/MacOS/chaff" 2>/dev/null || true)
  case "$BIN_STRINGS" in
    *"$HEAD_SHA"*) printf '  commit:    %s (found in the installed binary)\n' "$HEAD_SHA" ;;
    *)
      printf '  commit:    %s NOT FOUND — installed binary is from a different commit\n' "$HEAD_SHA" >&2
      exit 1
      ;;
  esac
fi

echo
echo "=== launching ==="
open -a "$DEST"
sleep 4
pgrep -fl "Chaff.app" | head -1 || echo "  (did not start)"
