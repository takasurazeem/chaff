#!/usr/bin/env bash
# Measure the Linux GUI's startup and footprint (#55).
#
# # What this measures, and what it does not
#
# It launches the app and reads `/proc` — process start to the moment the main loop is
# running, resident memory, thread count, and whether the webview process exists. That is
# **reading output**, which is what a log gives you.
#
# It does **not** judge whether the interface is responsive. Frame timing under a real
# interaction needs a person at the keyboard, and a programmatic harness is not QA. What this
# can say is whether the process starts in a reasonable time and does not grow without bound,
# which is where a WebKitGTK problem usually shows first.
set -euo pipefail

BIN="${1:-target/release/chaff-app}"
[ -x "$BIN" ] || { echo "not built: $BIN"; exit 1; }

echo "binary:  $BIN ($(stat -c%s "$BIN" | numfmt --to=iec) bytes)"
echo "webkit:  $(ldd "$BIN" 2>/dev/null | grep -c webkit2gtk) webkit libs linked"

start=$(date +%s%N)
setsid "$BIN" >/tmp/chaff-perf.log 2>&1 &
pid=$!

# Wait for the process to be alive and have opened a display connection.
for _ in $(seq 1 100); do
  if [ -d "/proc/$pid" ] && [ -e "/proc/$pid/fd" ]; then
    if ls -l "/proc/$pid/fd" 2>/dev/null | grep -qE 'socket|X11|wayland'; then break; fi
  fi
  sleep 0.1
done
elapsed=$(( ($(date +%s%N) - start) / 1000000 ))

if [ ! -d "/proc/$pid" ]; then
  echo "FAILED to stay running; log:"; tail -5 /tmp/chaff-perf.log; exit 1
fi

rss=$(awk '/VmRSS/ {print $2}' "/proc/$pid/status" 2>/dev/null || echo 0)
threads=$(awk '/Threads/ {print $2}' "/proc/$pid/status" 2>/dev/null || echo 0)
sleep 2
webviews=$(pgrep -c -f "WebKitWebProcess" 2>/dev/null || echo 0)

# A webview process may take longer than the display connection to appear, and counting it
# immediately reports zero on a machine where it simply had not started yet. Measured after a
# pause instead, and reported as such.
echo "startup: ${elapsed}ms to a display connection"
echo "memory:  $((rss / 1024)) MB resident"
echo "threads: $threads"
echo "webview: $webviews WebKitWebProcess (after a 2s settle)"

sleep 3
rss2=$(awk '/VmRSS/ {print $2}' "/proc/$pid/status" 2>/dev/null || echo 0)
echo "memory after 3s idle: $((rss2 / 1024)) MB"

kill "$pid" 2>/dev/null || true
pkill -f WebKitWebProcess 2>/dev/null || true

# A rough budget, stated rather than implied. Software rendering is the floor here: the
# container has no GPU passthrough, so a number that looks slow may be the environment
# rather than the application.
echo
if [ "$elapsed" -lt 10000 ]; then echo "  startup within 10s"; else echo "  startup OVER 10s"; fi
if [ "$((rss2 / 1024))" -lt 900 ]; then echo "  memory under 900 MB"; else echo "  memory OVER 900 MB"; fi
