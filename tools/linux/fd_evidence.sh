#!/usr/bin/env bash
# Definitive evidence that the window was created: the process holds a connection to the
# X server socket. Reading /proc is not driving the UI.
set -uo pipefail
CONTAINER="${CHAFF_CONTAINER:-chaff-build}"
SRC="${CHAFF_SRC:-$HOME/chaff}"
export DISPLAY="${DISPLAY:-:0}"

cd "$SRC" || exit 1
timeout 35 distrobox enter "$CONTAINER" -- bash -lc "cd '$SRC' && ./target/release/chaff" >/tmp/chaff-fd.log 2>&1 &
sleep 20

PID=$(pgrep -f 'release/chaff' | grep -v distrobox | head -1)
if [ -z "$PID" ]; then echo "NOT RUNNING"; exit 1; fi
echo "pid $PID is running"
echo
echo "--- X11 / wayland sockets it holds ---"
ls -l "/proc/$PID/fd" 2>/dev/null | grep -iE 'X11|wayland|\.X11-unix' || echo "(no X socket in fd table)"
echo
echo "--- shared libraries it actually linked ---"
grep -oE 'libwebkit2gtk[^ ]*|libjavascriptcore[^ ]*|libsoup[^ ]*|libgtk-3[^ ]*' "/proc/$PID/maps" 2>/dev/null | sort -u | head -6
echo
echo "--- threads (a webview spawns several) ---"
ls "/proc/$PID/task" 2>/dev/null | wc -l
wait 2>/dev/null
