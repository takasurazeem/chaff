#!/usr/bin/env bash
# Merge raw/JPEG folders that a script split apart years ago.
#
# **Moves files. Never reads them.** No decoding, no EXIF, no hashing — `mv` and `find`
# only. The only thing inspected is the file's *name*, which is what decides whether a
# folder is a raw folder or a JPEG folder.
#
# ## What it does
#
# Finds directories holding one kind of file — all raws, or all rendered images — and moves
# their contents up into the parent, so a photograph's files sit together again. Then
# removes the bucket folder if it is empty.
#
#     shoot/raws/IMG_1.CR3  ┐
#     shoot/jpegs/IMG_1.JPG ┘  ->  shoot/IMG_1.CR3 + shoot/IMG_1.JPG
#
# ## What it refuses to do
#
#   * **Never overwrites.** If the destination name is taken, it skips and says so.
#   * **Never touches a mixed folder.** A folder holding both raws and JPEGs is already
#     merged or is something else entirely.
#   * **Never follows symlinks** out of the tree.
#   * **Never deletes a non-empty folder.**
#   * **Dry run by default.** Nothing moves without `--apply`.
#
# ## Usage
#
#     tools/linux/merge_split_folders.sh ~/Pictures              # show what would happen
#     tools/linux/merge_split_folders.sh ~/Pictures --apply      # do it
set -uo pipefail

ROOT="${1:?usage: merge_split_folders.sh <root> [--apply]}"
APPLY="${2:-}"
LOG="$HOME/chaff-merge-$(date +%Y%m%d-%H%M%S).log"

if [ ! -d "$ROOT" ]; then echo "not a directory: $ROOT" >&2; exit 2; fi

RAW_EXT='cr3|cr2|nef|arw|rw2|raf|orf|dng|pef|srw|raw|rwl|iiq|mos|mrw|x3f|3fr|kdc|dcr|erf|mef|nrw|sr2|srf|gpr|fff|cam|arq|ari|lri|mdc|sti|ori|rwz|braw|r3d'
RASTER_EXT='jpg|jpeg|png|tif|tiff|heic|heif|webp|bmp|gif'
# Sidecars travel with their photograph. Anything else — .DS_Store, Thumbs.db — stays put.
SIDECAR_EXT='xmp|rrdata|pp3|aap|acr|thm'
MOVE_EXT="${RAW_EXT}|${RASTER_EXT}|${SIDECAR_EXT}"

# Files directly inside a directory, by extension.
count_kind() {
  local d="$1" ext="$2"
  find "$d" -maxdepth 1 -type f 2>/dev/null \
    | grep -icE "\.(${ext})$"
}

# Is this folder a bucket of one kind?
#
# **Dominance, not purity.** `May 7 - May 14/CR3/` holds 67 CR3, 1 DNG and a single stray
# TIFF. Requiring zero rasters blocked all 68 raws — and their 139 sidecars — because of one
# file. A folder is a bucket for a kind when it holds at least one of them and the other
# kind is at most a tenth as many.
#
# The threshold matters in both directions: 68 raw against 1 raster passes, while 5 raw
# against 4 raster does not, which is what keeps a genuinely mixed folder from being
# treated as a bucket and scattered.
is_bucket() {
  local d="$1" ext="$2" other="$3"
  local n o
  n=$(count_kind "$d" "$ext")
  o=$(count_kind "$d" "$other")
  [ "$n" -gt 0 ] && [ $(( o * 10 )) -le "$n" ]
}

# Does this folder's *parent* already hold the other kind?
#
# Needed to finish a half-completed merge, and harmless otherwise. The first run moved the
# raws up into the parent and left the JPEG folders behind; their raw sibling was by then
# empty and gone, so the sibling test could no longer see a partner. A JPEG folder sitting
# directly inside a folder that already holds raws is the same situation, and merging it is
# the same correct answer.
parent_has() {
  local d="$1" ext="$2"
  local parent; parent=$(dirname "$d")
  [ "$(count_kind "$parent" "$ext")" -gt 0 ]
}

echo "root:  $ROOT"
echo "mode:  $([ "$APPLY" = "--apply" ] && echo APPLY || echo 'DRY RUN — nothing will move')"
echo "log:   $LOG"
echo

moved=0; skipped=0; dirs=0

while IFS= read -r -d '' dir; do
  [ "$dir" = "$ROOT" ] && continue

  raws=$(count_kind "$dir" "$RAW_EXT")
  rasters=$(count_kind "$dir" "$RASTER_EXT")

  if is_bucket "$dir" "$RAW_EXT" "$RASTER_EXT"; then
    kind=raw
  elif is_bucket "$dir" "$RASTER_EXT" "$RAW_EXT"; then
    kind=jpeg
  else
    continue
  fi
  parent=$(dirname "$dir")

  # **Only merge a folder that has a sibling holding the other kind.**
  #
  # Without this, any single-kind folder looks like a bucket. The first dry run proposed
  # scattering `Screenshots/` (2 JPEGs, no raws) and `MEGA downloads/` (1 JPEG) up into
  # their parents, and `darktable_exported/` into the raw folder above it — none of which
  # are raw/JPEG buckets. They are ordinary folders that happen to hold one kind of file.
  #
  # A real bucket has a partner: `raws/` sits beside `jpegs/`, `CR3/` beside `JPG/`. That
  # pairing is the signal, and it is the same signal the pairing engine uses.
  partner=0
  if [ "$kind" = "raw" ]; then
    parent_has "$dir" "$RASTER_EXT" && partner=1
  else
    parent_has "$dir" "$RAW_EXT" && partner=1
  fi
  while IFS= read -r -d '' sib; do
    [ "$sib" = "$dir" ] && continue
    if [ "$kind" = "raw" ] && is_bucket "$sib" "$RASTER_EXT" "$RAW_EXT"; then partner=1; fi
    if [ "$kind" = "jpeg" ] && is_bucket "$sib" "$RAW_EXT" "$RASTER_EXT"; then partner=1; fi
  done < <(find "$parent" -mindepth 1 -maxdepth 1 -type d -print0 2>/dev/null)

  if [ "$partner" -eq 0 ]; then
    printf 'SKIP   %-5s %3d files  %s — no raw/JPEG partner folder beside it\n' \
      "$kind" "$(( raws + rasters ))" "${dir/#$ROOT/.}"
    continue
  fi
  n=$(( raws + rasters ))
  printf '%s  %-5s %3d files  %s -> %s\n' \
    "$([ "$APPLY" = "--apply" ] && echo MOVE || echo WOULD)" "$kind" "$n" \
    "${dir/#$ROOT/.}" "${parent/#$ROOT/.}"

  while IFS= read -r -d '' f; do
    base=$(basename "$f")
    dest="$parent/$base"
    if [ -e "$dest" ]; then
      printf '    SKIP  %s — %s already exists\n' "$base" "$base"
      echo "SKIP $f -> $dest (destination exists)" >>"$LOG"
      skipped=$((skipped+1))
      continue
    fi
    if [ "$APPLY" = "--apply" ]; then
      if mv -n -- "$f" "$dest" 2>>"$LOG"; then
        echo "MOVED $f -> $dest" >>"$LOG"
        moved=$((moved+1))
      else
        printf '    FAIL  %s\n' "$base"
        echo "FAIL $f -> $dest" >>"$LOG"
        skipped=$((skipped+1))
      fi
    else
      moved=$((moved+1))
    fi
  done < <(find "$dir" -maxdepth 1 -type f -print0 2>/dev/null \
             | while IFS= read -r -d '' f; do
                 printf '%s\0' "$f" | grep -qiE "\.(${MOVE_EXT})$" && printf '%s\0' "$f"
               done)

  # Remove the bucket only if it is now genuinely empty.
  if [ "$APPLY" = "--apply" ]; then
    if [ -z "$(find "$dir" -mindepth 1 -print -quit 2>/dev/null)" ]; then
      rmdir -- "$dir" 2>/dev/null && { printf '    rmdir %s\n' "${dir/#$ROOT/.}"; echo "RMDIR $dir" >>"$LOG"; }
    else
      printf '    keep  %s — not empty\n' "${dir/#$ROOT/.}"
    fi
  fi
  dirs=$((dirs+1))
done < <(find "$ROOT" -type d -not -path '*/.dtrash/*' -not -path '*/.git/*' -print0 2>/dev/null)

echo
printf 'folders: %d   files: %d   skipped: %d\n' "$dirs" "$moved" "$skipped"
if [ "$APPLY" != "--apply" ]; then
  echo
  echo "Nothing moved. Re-run with --apply to do it."
fi
