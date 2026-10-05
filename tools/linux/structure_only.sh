#!/usr/bin/env bash
# Directory structure of a photo library.
#
# **Names and counts only.** This script never opens a file: no EXIF, no decoding, no
# hashing, no reads of any kind. `find -type d` lists directory entries and `find -type f`
# lists file entries; neither touches contents. That is the scope the user authorised.
set -uo pipefail

ROOT="${1:-$HOME/Pictures}"

echo "root: $ROOT"
echo

echo "=== shape ==="
printf '  directories: %s\n' "$(find "$ROOT" -type d 2>/dev/null | wc -l)"
printf '  files:       %s\n' "$(find "$ROOT" -type f 2>/dev/null | wc -l)"
printf '  max depth:   %s\n' "$(find "$ROOT" -type d 2>/dev/null | awk -F/ '{print NF}' | sort -rn | head -1)"
echo

echo "=== file extensions (from names only) ==="
find "$ROOT" -type f 2>/dev/null \
  | sed 's/.*\.//' | tr '[:upper:]' '[:lower:]' \
  | sort | uniq -c | sort -rn | head -14 \
  | awk '{printf "  %-8s %s\n", $2, $1}'
echo

echo "=== directory tree, directories only ==="
find "$ROOT" -type d 2>/dev/null | sed "s|$ROOT|.|" | sort | head -60
echo

echo "=== the top 30 directories by file count ==="
echo "    (this is what decides whether shoot-relative ranking has enough frames)"
find "$ROOT" -type d 2>/dev/null -print0 \
  | while IFS= read -r -d '' d; do
      n=$(find "$d" -maxdepth 1 -type f 2>/dev/null | wc -l)
      [ "$n" -gt 0 ] && printf '%6d  %s\n' "$n" "${d/#$ROOT/.}"
    done | sort -rn | head -30
echo

echo "=== how many directories hold 8 or more files ==="
echo "    (MIN_SHOOT_SIZE is 8; below it a folder falls back to library-wide ranking)"
find "$ROOT" -type d 2>/dev/null -print0 \
  | while IFS= read -r -d '' d; do
      n=$(find "$d" -maxdepth 1 -type f 2>/dev/null | wc -l)
      [ "$n" -ge 8 ] && echo x
    done | wc -l | sed 's/^/  /'
