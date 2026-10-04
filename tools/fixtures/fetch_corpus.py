#!/usr/bin/env python3
"""Fetch Chaff's royalty-free test corpus: real RAW files and real JPEGs.

Replaces an earlier shell script whose RAW half was fabricated — it listed 14
filenames I had inferred from a partial read of a web page, and **all 14 returned
404**. Guessed URLs that fail silently are worse than no fetcher, so this one reads
the authoritative catalogue, takes only entries whose licence it can verify, and
records what it actually got.

SOURCES
-------
RAW   raw.pixls.us — the public archive darktable and RawTherapee use for their own
      regression testing. Its JSON catalogue lists 2,016 files with a licence per
      row: 1,870 CC0 (public domain dedication) and 146 CC-BY-NC. **Only CC0 is
      fetched.** CC-BY-NC is excluded deliberately: it forbids commercial use, and a
      test corpus that cannot be redistributed with the project is a liability.

JPEG  Lorem Picsum (picsum.photos), which serves Unsplash photographs under the
      Unsplash License. URLs are seeded, so a given seed always returns the same
      photograph and the corpus is byte-reproducible rather than "whatever the CDN
      felt like today".

WHAT THIS IS NOT
----------------
This is an integration and accuracy corpus. It is NOT used by unit tests. Unit tests
run against `fixtures/synthetic/`, which is generated, tiny, committed, deterministic
and offline. A unit test that downloads is a unit test that fails on a plane, fails
behind a corporate proxy, and fails differently tomorrow.

The corpus is gitignored and lives in `fixtures/corpus/`.

Nothing here touches any personal photograph. This script only writes into fixtures/.

Usage:
    python3 tools/fixtures/fetch_corpus.py --raw 20 --jpeg 50
    python3 tools/fixtures/fetch_corpus.py --verify
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import asdict, dataclass, is_dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
CORPUS = REPO_ROOT / "fixtures" / "corpus"

REPOSITORY_URL = "https://raw.pixls.us/json/getrepository.php?set=all"
PICSUM = "https://picsum.photos/seed/{seed}/{w}/{h}"

USER_AGENT = "chaff-fixture-fetcher/1.0 (+https://github.com/takasurazeem/chaff)"


@dataclass
class Fetched:
    path: str
    source: str
    licence: str
    bytes: int
    sha256: str
    make: str = ""
    model: str = ""
    fmt: str = ""


def encode_url(url: str) -> str:
    """Percent-encode the path of a URL, leaving scheme/host/query intact.

    raw.pixls.us serves filenames containing spaces and colons —
    `RaspberryPi - RP_ov5647 - 3:2.raw` — and emits them raw inside its catalogue
    HTML. Passing that straight to urllib raises InvalidURL: "URL can't contain
    control characters". The catalogue is the authority on *which* file, so the fix
    belongs here rather than in the selection logic.
    """
    parts = urllib.parse.urlsplit(url)
    return urllib.parse.urlunsplit(
        (parts.scheme, parts.netloc, urllib.parse.quote(parts.path), parts.query, parts.fragment)
    )


def http_get(url: str, timeout: int = 120) -> bytes:
    req = urllib.request.Request(encode_url(url), headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


def sha256_bytes(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


# ---------------------------------------------------------------------------
# RAW — raw.pixls.us
# ---------------------------------------------------------------------------
def load_catalogue() -> list[list]:
    doc = json.loads(http_get(REPOSITORY_URL, timeout=60).decode("utf-8", "replace"))
    rows = doc["data"] if isinstance(doc, dict) and "data" in doc else doc
    if not isinstance(rows, list) or not rows:
        raise SystemExit("catalogue came back empty or in an unexpected shape")
    return rows


def is_cc0(row: list) -> bool:
    lic = str(row[5]).lower()
    return "publicdomain/zero" in lic or "public-domain" in lic or ">co<" in lic


def download_url(row: list) -> str | None:
    m = re.search(r"href='([^']*getfile\.php[^']*)'", str(row[7]))
    return m.group(1).replace("&amp;", "&") if m else None


def pick_diverse(rows: list[list], count: int, max_mb: float) -> list[list]:
    """Choose entries spread across manufacturers and file formats.

    Diversity matters more than count here: twenty Canons test one decoder path twenty
    times, whereas twenty entries across fifteen manufacturers test the format
    variations that actually break RAW parsers. Within a manufacturer, smaller files
    are preferred so the corpus stays a sane size on disk.
    """
    cc0 = [r for r in rows if is_cc0(r) and download_url(r)]
    eligible = []
    for r in cc0:
        try:
            mb = float(r[3])
        except (TypeError, ValueError):
            continue
        if mb <= max_mb:
            eligible.append((mb, r))

    eligible.sort(key=lambda t: t[0])

    def fmt_of(row: list) -> str:
        url = download_url(row) or ""
        return Path(urllib.parse.unquote(url)).suffix.lower().lstrip(".")

    # Pass 1: one per (make, format) pair — maximises distinct decoder paths.
    seen_pairs: set[tuple[str, str]] = set()
    # Pass 2: fill remaining slots one per make.
    seen_makes: dict[str, int] = {}
    chosen: list[list] = []

    for _, row in eligible:
        pair = (row[0], fmt_of(row))
        if pair not in seen_pairs:
            seen_pairs.add(pair)
            chosen.append(row)
            seen_makes[row[0]] = seen_makes.get(row[0], 0) + 1
        if len(chosen) >= count:
            return chosen

    for _, row in eligible:
        if row in chosen:
            continue
        if seen_makes.get(row[0], 0) < 2:
            chosen.append(row)
            seen_makes[row[0]] = seen_makes.get(row[0], 0) + 1
        if len(chosen) >= count:
            break

    return chosen[:count]


def fetch_raw(count: int, max_mb: float, force: bool) -> list[Fetched]:
    out_dir = CORPUS / "raw"
    out_dir.mkdir(parents=True, exist_ok=True)

    print(f"==> RAW corpus (CC0 only, from raw.pixls.us)")
    rows = load_catalogue()
    n_cc0 = sum(1 for r in rows if is_cc0(r))
    print(f"    catalogue: {len(rows)} entries, {n_cc0} CC0 (CC-BY-NC excluded on licence grounds)")

    picks = pick_diverse(rows, count, max_mb)
    if not picks:
        raise SystemExit("no eligible CC0 entries found under the size cap")

    results: list[Fetched] = []
    for i, row in enumerate(picks, 1):
        url = download_url(row)
        assert url is not None
        name = Path(urllib.parse.unquote(url)).name
        # Keep the camera identity in the filename: it is what makes a failing decode
        # test diagnosable months later.
        safe = re.sub(r"[^A-Za-z0-9._-]+", "_", f"{row[0]}-{row[1]}-{name}")
        dest = out_dir / safe

        if dest.exists() and not force and dest.stat().st_size > 0:
            data = dest.read_bytes()
            print(f"    have    {safe}  ({len(data):,} bytes)")
        else:
            try:
                data = http_get(url, timeout=300)
            except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError) as e:
                # Loud, not silent. The previous fetcher printed FAILED and carried on,
                # which is how a corpus of zero files went unnoticed.
                print(f"    FAILED  {safe}: {e}", file=sys.stderr)
                continue
            if not data:
                print(f"    FAILED  {safe}: empty response", file=sys.stderr)
                continue
            dest.write_bytes(data)
            print(f"    fetch   {safe}  ({len(data):,} bytes)")

        results.append(
            Fetched(
                path=str(dest.relative_to(REPO_ROOT)),
                source=url,
                licence="CC0-1.0",
                bytes=len(data),
                sha256=sha256_bytes(data),
                make=str(row[0]),
                model=str(row[1]),
                fmt=dest.suffix.lstrip("."),
            )
        )

    got = len(results)
    print(f"    {got}/{count} RAW files present")
    if got == 0:
        raise SystemExit("RAW fetch produced nothing — treating as failure, not as success")
    return results


# ---------------------------------------------------------------------------
# JPEG — Lorem Picsum
# ---------------------------------------------------------------------------
def fetch_jpeg(count: int, width: int, height: int, force: bool) -> list[Fetched]:
    out_dir = CORPUS / "jpeg"
    out_dir.mkdir(parents=True, exist_ok=True)
    print(f"==> JPEG corpus ({count} seeded royalty-free photographs)")

    results: list[Fetched] = []
    for i in range(1, count + 1):
        seed = f"chaff-{i:03d}"
        dest = out_dir / f"{seed}.jpg"

        if dest.exists() and not force and dest.stat().st_size > 0:
            data = dest.read_bytes()
        else:
            url = PICSUM.format(seed=seed, w=width, h=height)
            try:
                data = http_get(url, timeout=120)
            except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError) as e:
                print(f"    FAILED  {seed}: {e}", file=sys.stderr)
                continue
            dest.write_bytes(data)

        results.append(
            Fetched(
                path=str(dest.relative_to(REPO_ROOT)),
                source=PICSUM.format(seed=seed, w=width, h=height),
                licence="Unsplash License",
                bytes=len(data),
                sha256=sha256_bytes(data),
                make="Picsum",
                model=seed,
                fmt="jpg",
            )
        )
        if i % 10 == 0 or i == count:
            print(f"    {i}/{count}")

    print(f"    {len(results)}/{count} JPEGs present")
    return results


# ---------------------------------------------------------------------------
def write_manifest(all_files: list[Fetched], merge: bool = True) -> None:
    path = CORPUS / "MANIFEST.json"
    existing: list[dict] = []
    if merge and path.exists():
        try:
            existing = json.loads(path.read_text()).get("files", [])
        except json.JSONDecodeError:
            existing = []

    # Already-on-disk entries come back from JSON as plain dicts while freshly fetched
    # ones are dataclasses. Normalise both rather than assuming either shape.
    by_path: dict[str, dict] = {}
    for f in existing:
        if isinstance(f, dict) and "path" in f:
            by_path[f["path"]] = f
    for f in all_files:
        d = asdict(f) if is_dataclass(f) else dict(f)
        by_path[d["path"]] = d

    doc = {
        "note": (
            "Integration and accuracy corpus. NOT used by unit tests, which run against "
            "fixtures/synthetic/. Every entry records the licence and the exact source URL "
            "it came from, so provenance is auditable rather than assumed."
        ),
        "sources": {
            "raw": "https://raw.pixls.us/ (CC0 entries only; CC-BY-NC excluded)",
            "jpeg": "https://picsum.photos/ (Unsplash License)",
        },
        "size_caveat": (
            "The raw.pixls.us catalogue's size column describes the original upload, while "
            "getfile.php serves a normalised variant whose size can differ substantially "
            "(one RaspberryPi entry is catalogued at 0.35 MB and served at 6.3 MB). The "
            "byte counts recorded here are the actual sizes of the files on disk."
        ),
        "count": len(by_path),
        "files": sorted(by_path.values(), key=lambda f: f["path"]),
    }
    path.write_text(json.dumps(doc, indent=2) + "\n")
    print(f"\nwrote {path.relative_to(REPO_ROOT)}  ({doc['count']} files)")


def verify() -> int:
    path = CORPUS / "MANIFEST.json"
    if not path.exists():
        print("no manifest — nothing to verify", file=sys.stderr)
        return 1
    doc = json.loads(path.read_text())
    bad = 0
    for f in doc["files"]:
        p = REPO_ROOT / f["path"]
        if not p.exists():
            print(f"MISSING {f['path']}")
            bad += 1
            continue
        actual = sha256_bytes(p.read_bytes())
        if actual != f["sha256"]:
            print(f"CORRUPT {f['path']}")
            bad += 1
    print(f"verified {len(doc['files']) - bad}/{len(doc['files'])} files")
    return 1 if bad else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--raw", type=int, default=0, help="number of CC0 RAW files (0 = skip)")
    ap.add_argument("--jpeg", type=int, default=0, help="number of royalty-free JPEGs (0 = skip)")
    ap.add_argument("--max-raw-mb", type=float, default=45.0, help="per-file size cap for RAW")
    ap.add_argument("--width", type=int, default=1600)
    ap.add_argument("--height", type=int, default=1067)
    ap.add_argument("--force", action="store_true", help="re-download even if present")
    ap.add_argument("--verify", action="store_true", help="only re-verify hashes")
    args = ap.parse_args()

    if args.verify:
        return verify()

    if args.raw == 0 and args.jpeg == 0:
        args.raw, args.jpeg = 20, 50

    CORPUS.mkdir(parents=True, exist_ok=True)
    collected: list[Fetched] = []
    if args.raw:
        collected += fetch_raw(args.raw, args.max_raw_mb, args.force)
    if args.jpeg:
        collected += fetch_jpeg(args.jpeg, args.width, args.height, args.force)
    write_manifest(collected)

    raw_n = sum(1 for f in collected if f.fmt.lower() not in ("jpg", "jpeg"))
    jpg_n = len(collected) - raw_n
    print(f"\ncorpus: {raw_n} RAW, {jpg_n} JPEG")
    print("REMINDER: integration and accuracy tests only. Unit tests use fixtures/synthetic/.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
