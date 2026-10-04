#!/usr/bin/env bash
# Evidence that the window rendered. Reading window state is not driving the UI.
set -uo pipefail
CONTAINER="${CHAFF_CONTAINER:-chaff-build}"
SRC="${CHAFF_SRC:-$HOME/chaff}"
export DISPLAY="${DISPLAY:-:0}"

for tool in wmctrl xdotool; do
  command -v "$tool" >/dev/null && echo "$tool: present" || echo "$tool: absent"
done

cd "$SRC" || exit 1
timeout 40 distrobox enter "$CONTAINER" -- bash -lc "cd '$SRC' && ./target/release/chaff" >/tmp/chaff-window.log 2>&1 &
sleep 22

echo "--- process ---"
pgrep -af 'release/chaff' | grep -v distrobox | head -2 || echo "NOT RUNNING"

echo "--- windows on the display ---"
if command -v wmctrl >/dev/null; then
  wmctrl -l 2>&1 | head -8
elif command -v xdotool >/dev/null; then
  xdotool search --name "." getwindowname %@ 2>&1 | head -8
else
  echo "(no window tool; checking X client list instead)"
  xlsclients 2>&1 | head -8 || echo "(no xlsclients either)"
fi

wait 2>/dev/null
