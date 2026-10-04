#!/usr/bin/env bash
# Launch the Linux build and read its output. Build, launch, read logs — no UI driving.
#
# DISPLAY is not exported into an SSH session even when the desktop is running, so it is
# set here. X0 is the conventional first local display.
set -uo pipefail

CONTAINER="${CHAFF_CONTAINER:-chaff-build}"
SRC="${CHAFF_SRC:-$HOME/chaff}"
export DISPLAY="${DISPLAY:-:0}"
LOG=/tmp/chaff-run.log

echo "display: DISPLAY=$DISPLAY"
if [ ! -e /tmp/.X11-unix/X0 ] && [ ! -e /tmp/.X11-unix/X1 ]; then
  echo "no X socket in /tmp/.X11-unix — the desktop may not be running"
fi

cd "$SRC" || exit 1
rm -f "$LOG"

timeout 30 distrobox enter "$CONTAINER" -- bash -lc "cd '$SRC' && ./target/release/chaff" >"$LOG" 2>&1 &
RUNNER=$!
sleep 20

echo "--- process ---"
if pgrep -af 'release/chaff' | grep -v distrobox | head -3; then
  echo "running"
else
  echo "NOT RUNNING"
fi

echo "--- log ---"
tail -25 "$LOG" 2>/dev/null || echo "(no log)"

wait $RUNNER 2>/dev/null
echo "--- exit ---"
echo "runner finished"
