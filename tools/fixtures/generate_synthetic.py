#!/usr/bin/env python3
"""Generate deterministic synthetic fixtures for Chaff's test suite.

WHY SYNTHETIC FIXTURES EXIST AT ALL
-----------------------------------
Unit tests must be fast, deterministic, and offline. A test that downloads a
photograph is a test that fails on a plane, fails in CI behind a proxy, and fails
differently tomorrow when the remote image changes. So:

  fixtures/synthetic/   generated here, tiny, committed. Unit + integration tests.
  fixtures/corpus/      downloaded by fetch_corpus.py, large, gitignored.
                        Accuracy and decode tests against real camera files.
                        Accuracy and decode tests against real camera files.

WHAT THESE ARE NOT
------------------
These are not photographs and contain no people. They are procedural patterns
designed to have *known, measurable* properties so a test can assert a
relationship ("this file is blurrier than that file") rather than a magic number.

That distinction matters for the scoring model: the correctness property we test
is ordering and discrimination, not "the focus score equals 412.7".

Everything is seeded, so re-running produces byte-identical output.

Usage:
    python3 tools/fixtures/generate_synthetic.py [--out fixtures/synthetic]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

SEED = 20261004
SIZE = (640, 480)


# --------------------------------------------------------------------------
# Pattern generators
# --------------------------------------------------------------------------
def _rng(offset: int) -> np.random.Generator:
    """A generator seeded deterministically per-fixture, independent of call order."""
    return np.random.default_rng(SEED + offset)


def sharp_texture(offset: int, size=SIZE) -> Image.Image:
    """High-frequency detail: edges at several scales. The 'in focus' reference."""
    w, h = size
    arr = np.zeros((h, w), dtype=np.uint8)
    rng = _rng(offset)

    # Coarse blocks give low-frequency structure (so the frame is not degenerate).
    for _ in range(24):
        x0 = int(rng.integers(0, w - 40))
        y0 = int(rng.integers(0, h - 40))
        bw = int(rng.integers(20, 60))
        bh = int(rng.integers(20, 60))
        arr[y0 : y0 + bh, x0 : x0 + bw] = int(rng.integers(40, 215))

    img = Image.fromarray(arr, mode="L").convert("RGB")
    draw = ImageDraw.Draw(img)

    # Fine detail: a grid of hairlines plus small high-contrast marks. This is the
    # energy the Laplacian responds to, and therefore what "sharp" actually means.
    #
    # Both grid directions are drawn in the same colour on purpose. An earlier version
    # used white vertical lines and black horizontal lines, which gave the pattern
    # unequal energy along x and y — so an *isotropic* sharp frame measured as strongly
    # anisotropic, and the motion-blur heuristic fired on frames that were not blurred at
    # all. A sharp fixture must be directionally neutral or it cannot calibrate a
    # directional metric.
    for x in range(0, w, 8):
        draw.line([(x, 0), (x, h)], fill=(255, 255, 255), width=1)
    for y in range(0, h, 8):
        draw.line([(0, y), (w, y)], fill=(255, 255, 255), width=1)
    for _ in range(120):
        x = int(rng.integers(2, w - 8))
        y = int(rng.integers(2, h - 8))
        draw.rectangle([x, y, x + 4, y + 4], fill=(255, 255, 255))

    # NOTE: no single-pixel speckle here, deliberately.
    #
    # An earlier version added 1px black/white speckle to break the 8px grid's
    # periodicity. That was a mistake: a 1px impulse is *physically indistinguishable*
    # from sensor noise by any local estimator, and the noise estimator correctly
    # classified it as such — it reported sigma ~134 on a clean frame and the noise
    # correction then zeroed out every sharp fixture.
    #
    # The aperiodicity needed to stop blur-displacement aliasing comes from the randomly
    # placed 4x4 marks above, whose positions have no period. Any further detail at the
    # 1px scale would be indistinguishable from noise, in the fixture and in a real
    # photograph alike.

    return img


def gaussian_blurred(offset: int, radius: float, size=SIZE) -> Image.Image:
    """Defocus. Uniformly soft across the whole frame, and isotropic by construction —
    which is exactly what separates it from motion blur in the metric."""
    return sharp_texture(offset, size).filter(ImageFilter.GaussianBlur(radius=radius))


def _box_blur_x(arr: np.ndarray, length: int) -> np.ndarray:
    """True uniform horizontal box blur via a running sum.

    Exact, and free of the resampling artefacts that a shift-and-average loop
    introduces: bilinear resampling is itself a low-pass filter, so a 9-tap shift loop
    blurs the image twice over and by an amount that depends on how the shift lands on
    the pixel grid.
    """
    h, w, c = arr.shape
    pad = length // 2
    padded = np.pad(arr, ((0, 0), (pad, pad), (0, 0)), mode="edge")
    cumsum = np.cumsum(padded, axis=1)
    cumsum = np.concatenate([np.zeros((h, 1, c)), cumsum], axis=1)
    out = (cumsum[:, length:] - cumsum[:, :-length]) / float(length)
    return out[:, :w, :]


def motion_blurred(offset: int, length: int = 13, size=SIZE) -> Image.Image:
    """Directional motion blur.

    A horizontal smear of 13px on an 8px-period grid, plus aperiodic speckle. The
    vertical edges are strongly attenuated while horizontal edges survive, which is what
    makes the result *anisotropic* — the property that distinguishes motion from defocus.
    Length 13 is deliberately not a multiple of the 8px grid period.
    """
    base = np.asarray(sharp_texture(offset, size), dtype=np.float64)
    blurred = _box_blur_x(base, length)
    return Image.fromarray(np.clip(blurred, 0, 255).astype(np.uint8), mode="RGB")


def exposure_shifted(offset: int, factor: float, size=SIZE) -> Image.Image:
    """Scale exposure. factor > 1 pushes highlights into clipping."""
    base = np.asarray(sharp_texture(offset, size), dtype=np.float64)
    return Image.fromarray(np.clip(base * factor, 0, 255).astype(np.uint8), mode="RGB")


def noisy(offset: int, sigma: float, size=SIZE) -> Image.Image:
    """Sensor noise: additive Gaussian. Note the underlying frame stays sharp, so a
    correct implementation must score this as *noisy but in focus*, not as blurry."""
    base = np.asarray(sharp_texture(offset, size), dtype=np.float64)
    rng = _rng(offset + 5000)
    noise = rng.normal(0.0, sigma, base.shape)
    return Image.fromarray(np.clip(base + noise, 0, 255).astype(np.uint8), mode="RGB")


def flat_low_contrast(offset: int, size=SIZE) -> Image.Image:
    """A genuinely sharp frame in a scene with almost no contrast.

    The adversarial case for naive blur detection: absolute Laplacian variance is
    very low here, so a whole-frame absolute threshold marks it blurry. It is not
    blurry, and this fixture exists to catch that bug.
    """
    w, h = size
    arr = np.full((h, w), 128, dtype=np.uint8)
    arr = np.clip(arr.astype(np.float64) + _rng(offset).normal(0, 3.0, (h, w)), 0, 255)
    img = Image.fromarray(arr.astype(np.uint8), mode="L").convert("RGB")
    draw = ImageDraw.Draw(img)
    for x in range(0, w, 16):
        draw.line([(x, 0), (x, h)], fill=(134, 134, 134), width=1)
    return img


def bokeh_portrait(offset: int, size=SIZE) -> Image.Image:
    """A sharp subject disc on a heavily blurred background.

    The other adversarial case: roughly two thirds of the frame has almost no
    high-frequency energy. Whole-frame scoring marks this blurry. Subject-region
    scoring marks it sharp, which is correct — and this fixture is how we prove the
    implementation does the latter.

    The background is blurred hard (radius 15) on purpose. At radius 9 the blurred
    texture retained enough energy that whole-frame and subject-ROI measurements were
    only ~25% apart, which is too small a margin to distinguish "the ROI works" from
    "the ROI happens to be in a slightly better place". A realistic shallow-DOF
    background carries essentially no detail, so the fixture should not either.
    """
    w, h = size
    background = gaussian_blurred(offset, radius=15.0, size=size)

    subject = sharp_texture(offset + 77, size=size)
    mask = Image.new("L", size, 0)
    draw = ImageDraw.Draw(mask)
    cx, cy, r = int(w * 0.42), int(h * 0.46), int(min(w, h) * 0.30)
    draw.ellipse([cx - r, cy - r, cx + r, cy + r], fill=255)
    mask = mask.filter(ImageFilter.GaussianBlur(radius=2))

    return Image.composite(subject, background, mask)


# --------------------------------------------------------------------------
# EXIF
# --------------------------------------------------------------------------
def exif_for(capture_time: str, model: str = "Chaff Test Body") -> Image.Exif:
    """Minimal but valid EXIF, enough to exercise capture-time and body grouping."""
    exif = Image.Exif()
    exif[0x010F] = "Chaff"            # Make
    exif[0x0110] = model              # Model
    exif[0x0132] = capture_time       # DateTime
    exif[0x8827] = 400                # ISOSpeedRatings
    exif[0x829D] = (28, 10)           # FNumber = f/2.8
    exif[0x829A] = (1, 250)           # ExposureTime = 1/250
    exif[0x920A] = (35, 1)            # FocalLength = 35mm
    return exif


# --------------------------------------------------------------------------
# Fixture plan
# --------------------------------------------------------------------------
# name -> (builder, description, properties tests may rely on)
FIXTURES = [
    ("sharp_a", lambda: sharp_texture(1), "high-frequency detail, in focus",
     {"focus": "high", "contrast": "normal"}),
    ("sharp_b", lambda: sharp_texture(2), "second independent sharp frame",
     {"focus": "high", "contrast": "normal"}),
    ("blur_defocus_mild", lambda: gaussian_blurred(3, 1.6), "mild defocus",
     {"focus": "low", "blur_type": "defocus"}),
    ("blur_defocus_heavy", lambda: gaussian_blurred(4, 4.0), "heavy defocus",
     {"focus": "very_low", "blur_type": "defocus"}),
    ("blur_motion", lambda: motion_blurred(5), "directional motion blur",
     {"focus": "low", "blur_type": "motion", "anisotropic": True}),
    ("exposure_over", lambda: exposure_shifted(6, 1.9), "highlights clipped",
     {"exposure": "clipped_high"}),
    ("exposure_under", lambda: exposure_shifted(7, 0.28), "shadows crushed",
     {"exposure": "clipped_low"}),
    ("noise_high_iso", lambda: noisy(8, 26.0), "heavy noise, still in focus",
     {"focus": "high", "noise": "high"}),
    ("flat_low_contrast", lambda: flat_low_contrast(9), "sharp but very low contrast",
     {"focus": "low_absolute_high_relative", "contrast": "low"}),
    ("bokeh_portrait", lambda: bokeh_portrait(10), "sharp subject, blurred background",
     {"focus_subject": "high", "focus_whole_frame": "low"}),
]


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    h.update(path.read_bytes())
    return h.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default="fixtures/synthetic", help="output directory")
    args = ap.parse_args()

    out = Path(args.out).resolve()
    images_dir = out / "images"
    pairtree_dir = out / "pairtree"
    images_dir.mkdir(parents=True, exist_ok=True)

    manifest: dict = {"seed": SEED, "size": list(SIZE), "images": [], "pairtree": []}

    print(f"generating synthetic images -> {images_dir}")
    for i, (name, build, desc, props) in enumerate(FIXTURES):
        img = build()
        path = images_dir / f"{name}.jpg"
        # Quality 95 keeps detail intact: JPEG artefacts would themselves look like
        # noise to a sharpness metric and would make the fixtures lie.
        img.save(path, "JPEG", quality=95, exif=exif_for(f"2026:10:04 12:{i:02d}:00"))
        manifest["images"].append({
            "name": name,
            "file": f"images/{name}.jpg",
            "description": desc,
            "expected": props,
            "sha256": sha256(path),
        })
        print(f"  {name:22s} {path.stat().st_size:>7d} bytes  {desc}")

    # ----------------------------------------------------------------------
    # Pairing tree — PATH LOGIC ONLY.
    #
    # The raw-extension files here are NOT valid RAW files and must never be used
    # to test decoding. They exist so the indexer can be pointed at a real
    # directory tree and produce known groups. Real RAW decoding is tested against
    # fixtures/corpus/raw/, which holds genuine camera files.
    # ----------------------------------------------------------------------
    print(f"generating pairing tree -> {pairtree_dir}")
    plan = [
        # (relative path, content)
        ("shoot_a/IMG_0001.CR3", b"NOT-A-REAL-RAW path fixture"),
        ("shoot_a/IMG_0001.JPG", "image:sharp_a"),
        ("shoot_a/IMG_0001.XMP", b"<xmp/>"),
        ("shoot_a/IMG_0002.CR3", b"NOT-A-REAL-RAW path fixture"),
        ("shoot_a/IMG_0002.JPG", "image:sharp_b"),
        ("shoot_a/IMG_0003.CR3", b"NOT-A-REAL-RAW path fixture"),          # orphan raw
        ("shoot_a/IMG_0004.JPG", "image:sharp_a"),                          # orphan jpeg
        ("shoot_a/IMG_0005.CR3", b"NOT-A-REAL-RAW path fixture"),          # ambiguous
        ("shoot_a/IMG_0005.NEF", b"NOT-A-REAL-RAW path fixture"),
        ("shoot_a/IMG_0005.JPG", "image:sharp_b"),
        ("shoot_a/IMG_0006.CR3", b"NOT-A-REAL-RAW path fixture"),
        ("shoot_a/IMG_0006.JPG", "image:sharp_a"),
        ("shoot_a/IMG_0006 (1).CR3", b"NOT-A-REAL-RAW path fixture"),      # suspected dup
        ("shoot_a/notes.txt", b"not an image"),
        ("shoot_a/IMG_0007.CR3", b"NOT-A-REAL-RAW path fixture"),          # video sibling
        ("shoot_a/IMG_0007.JPG", "image:sharp_b"),
        ("shoot_a/IMG_0007.MP4", b"NOT-A-VIDEO path fixture"),
        ("shoot_b/IMG_0001.CR3", b"NOT-A-REAL-RAW path fixture"),          # same stem, other dir
        ("shoot_b/IMG_0001.JPG", "image:sharp_a"),
        # Unicode normalisation: NFC vs NFD spellings of the same stem. On APFS these
        # may be coalesced; the generator records what it actually wrote so the test can
        # assert against reality rather than against an assumption.
        ("shoot_b/caf\u00e9.CR3", b"NOT-A-REAL-RAW path fixture"),         # NFC
        ("shoot_b/cafe\u0301.JPG", "image:sharp_b"),                        # NFD
    ]

    made = []
    for rel, content in plan:
        dest = pairtree_dir / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, str) and content.startswith("image:"):
            src = images_dir / f"{content.split(':', 1)[1]}.jpg"
            dest.write_bytes(src.read_bytes())
        else:
            dest.write_bytes(content)  # type: ignore[arg-type]
        made.append(rel)
    for rel in made:
        print(f"  {rel}")

    manifest["pairtree"] = made
    manifest["pairtree_note"] = (
        "Raw-extension files here are path fixtures with placeholder content. They are "
        "NOT valid RAW files and must never be used for decode tests. Use "
        "fixtures/corpus/raw/ for decoding."
    )

    manifest_path = out / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"\nwrote {manifest_path}")
    print(f"{len(manifest['images'])} images, {len(made)} pairtree files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
