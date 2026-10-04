#!/usr/bin/env bash
#
# Fetch the royalty-free test corpus for Chaff.
#
# TWO SOURCES, TWO LICENCES, BOTH VERIFIABLE:
#
#   JPEG (default 50)  Lorem Picsum, https://picsum.photos — serves Unsplash photographs
#                      under the Unsplash License (free to use, including commercially,
#                      no permission needed). Seeded URLs, so a given seed always
#                      returns the same photograph and the corpus is reproducible.
#
#   RAW  (14 files)    Mirror of raw.pixls.us, the public archive that darktable and
#                      RawTherapee use for their own regression testing. Every file is
#                      CC0 (public domain dedication): use, modify, redistribute, sell.
#                      Served from a GitHub release with SHA-256 checksums, which this
#                      script verifies before accepting a file.
#
# WHY THE RAW SET IS ONLY 14 FILES:
#   There is no large royalty-free corpus of RAW *photographs*. RAW is a per-camera
#   proprietary container and stock libraries do not license it. 14 files covering 12
#   formats with a clean CC0 licence and published checksums is a better test corpus
#   than 50 files of uncertain provenance. For volume, the synthetic generator produces
#   unlimited deterministic RAW fixtures — see generate_synthetic.py.
#
# NOTHING HERE TOUCHES THE USER'S PHOTOGRAPHS. This script only downloads into
# fixtures/, which is gitignored and never shipped in the app bundle.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CORPUS_DIR="${REPO_ROOT}/fixtures/corpus"
RAW_DIR="${CORPUS_DIR}/raw"
JPEG_DIR="${CORPUS_DIR}/jpeg"

RAW_BASE="https://github.com/tellodaniel/revelraw.com/releases/download/sample-raw-files-v1"
CHECKSUMS_URL="${RAW_BASE}/SHA256SUMS.txt"

# One file per format, chosen upstream for format coverage.
RAW_FILES=(
  Sony-a7III.ARW
  Canon-EOS-R.CR3
  Canon-5D-Mark-IV.CR2
  Nikon-Z6.NEF
  Fujifilm-X-T4.RAF
  Olympus-E-M1-Mark-III.ORF
  Panasonic-Lumix-DC-G9.RW2
  Pentax-K-1.PEF
  iPhone-ProRAW.DNG
  Leica-Q2.DNG
  Samsung-NX1.SRW
  Sigma-fp.X3F
  Hasselblad-X1D.3FR
  Sony-A7R-II.ARW
)

JPEG_COUNT="${JPEG_COUNT:-50}"
JPEG_WIDTH="${JPEG_WIDTH:-1600}"
JPEG_HEIGHT="${JPEG_HEIGHT:-1067}"

fetch_raw=false
fetch_jpeg=false
verify_only=false

usage() {
  cat <<'EOF'
Usage: fetch_corpus.sh [--raw] [--jpeg] [--all] [--verify]

  --raw      download and checksum-verify the CC0 RAW samples
  --jpeg     download 50 seeded royalty-free JPEGs (set JPEG_COUNT to change)
  --all      both of the above  (default when no flag is given)
  --verify   re-verify checksums of already-downloaded files, download nothing
  -h         this message

Environment:
  JPEG_COUNT   number of JPEGs to fetch (default 50)
  JPEG_WIDTH   default 1600
  JPEG_HEIGHT  default 1067
EOF
}

if [ $# -eq 0 ]; then
  fetch_raw=true
  fetch_jpeg=true
fi

while [ $# -gt 0 ]; do
  case "$1" in
    --raw)    fetch_raw=true ;;
    --jpeg)   fetch_jpeg=true ;;
    --all)    fetch_raw=true; fetch_jpeg=true ;;
    --verify) verify_only=true; fetch_raw=true; fetch_jpeg=true ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

mkdir -p "$RAW_DIR" "$JPEG_DIR"

# ---------------------------------------------------------------------------
# RAW — CC0, checksum-verified
# ---------------------------------------------------------------------------
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

if [ "$fetch_raw" = true ]; then
  echo "==> RAW corpus (CC0, from raw.pixls.us via the RevelRaw mirror)"

  CHECKSUMS_FILE="$(mktemp)"
  if curl -fsSL -o "$CHECKSUMS_FILE" "$CHECKSUMS_URL"; then
    echo "    checksums: $(wc -l < "$CHECKSUMS_FILE" | tr -d ' ') entries"
  else
    echo "    WARNING: could not fetch SHA256SUMS.txt; downloads will be unverified" >&2
    : > "$CHECKSUMS_FILE"
  fi

  for name in "${RAW_FILES[@]}"; do
    dest="${RAW_DIR}/${name}"
    if [ -s "$dest" ]; then
      echo "    have    ${name}"
    elif [ "$verify_only" = true ]; then
      echo "    MISSING ${name}" >&2
      continue
    else
      printf '    fetch   %s ... ' "$name"
      if curl -fsSL -o "$dest" "${RAW_BASE}/${name}"; then
        printf '%s bytes\n' "$(wc -c < "$dest" | tr -d ' ')"
      else
        printf 'FAILED\n'
        rm -f "$dest"
        continue
      fi
    fi

    expected="$(awk -v n="$name" '$2 == n || $2 == "*"n {print $1; exit}' "$CHECKSUMS_FILE" || true)"
    if [ -n "$expected" ]; then
      actual="$(sha256_of "$dest")"
      if [ "$expected" = "$actual" ]; then
        echo "    ok      ${name} (sha256 verified)"
      else
        echo "    CORRUPT ${name}: expected ${expected}, got ${actual}" >&2
        rm -f "$dest"
        exit 1
      fi
    fi
  done
  rm -f "$CHECKSUMS_FILE"
fi

# ---------------------------------------------------------------------------
# JPEG — royalty-free, seeded and therefore reproducible
# ---------------------------------------------------------------------------
if [ "$fetch_jpeg" = true ]; then
  echo "==> JPEG corpus (${JPEG_COUNT} seeded royalty-free photographs)"

  if [ "$verify_only" = true ]; then
    present=$(find "$JPEG_DIR" -name '*.jpg' | wc -l | tr -d ' ')
    echo "    ${present}/${JPEG_COUNT} present"
  else
    for i in $(seq 1 "$JPEG_COUNT"); do
      # The seed makes the URL stable: the same seed always yields the same photograph,
      # so a failing test cannot be "fixed" by a different image arriving.
      seed="$(printf 'chaff-%03d' "$i")"
      dest="${JPEG_DIR}/${seed}.jpg"

      if [ -s "$dest" ]; then
        continue
      fi

      url="https://picsum.photos/seed/${seed}/${JPEG_WIDTH}/${JPEG_HEIGHT}"
      if curl -fsSL -o "$dest" "$url" && [ -s "$dest" ]; then
        printf '    %3d/%d  %s (%s bytes)\n' "$i" "$JPEG_COUNT" "$seed" "$(wc -c < "$dest" | tr -d ' ')"
      else
        echo "    FAILED ${seed} from ${url}" >&2
        rm -f "$dest"
      fi
    done
    present=$(find "$JPEG_DIR" -name '*.jpg' | wc -l | tr -d ' ')
    echo "    ${present}/${JPEG_COUNT} downloaded"
  fi
fi

# ---------------------------------------------------------------------------
echo
echo "corpus:"
echo "  raw:  $(find "$RAW_DIR" -type f 2>/dev/null | wc -l | tr -d ' ') files, $(du -sh "$RAW_DIR" 2>/dev/null | cut -f1)"
echo "  jpeg: $(find "$JPEG_DIR" -type f 2>/dev/null | wc -l | tr -d ' ') files, $(du -sh "$JPEG_DIR" 2>/dev/null | cut -f1)"
echo
echo "REMINDER: this corpus is for integration and accuracy tests only. Unit tests must"
echo "not depend on it — they use deterministic synthetic fixtures instead."
