#!/usr/bin/env bash
# Does the Linux build actually open a window? Evidence only; nothing is driven.
set -uo pipefail
CONTAINER="${CHAFF_CONTAINER:-chaff-build}"
SRC="${CHAFF_SRC:-$HOME/chaff}"
export DISPLAY="${DISPLAY:-:0}"

pkill -f release/chaff 2>/dev/null
sleep 1

echo "=== what the binary links against ==="
ldd "$SRC/target/release/chaff" 2>/dev/null \
  | grep -oE 'libwebkit2gtk[^ ]*|libjavascriptcore[^ ]*|libsoup[^ ]*|libgtk-3[^ ]*' \
  | sort -u | head -5

echo
echo "=== launching for 25s ==="
( cd "$SRC" && timeout 25 distrobox enter "$CONTAINER" -- bash -lc "cd '$SRC' && exec ./target/release/chaff" ) >/tmp/verify.log 2>&1 &
sleep 15

echo "=== processes ==="
ps -eo pid,nlwp,etime,comm | grep -i chaff | grep -v grep | head -5

PID=$(pgrep -x chaff | head -1)
if [ -n "${PID:-}" ]; then
  echo
  echo "=== pid $PID holds: ==="
  ls -l "/proc/$PID/fd" 2>/dev/null | grep -icE 'X11|wayland' | sed 's/^/  display sockets: /'
  echo "  threads: $(ls /proc/$PID/task 2>/dev/null | wc -l)"
fi

echo
echo "=== log (EGL noise removed) ==="
grep -vE 'libEGL|pci id|Gtk-Message' /tmp/verify.log | head -10

wait 2>/dev/null
echo "=== done ==="
