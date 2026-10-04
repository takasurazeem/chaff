# Fixture Strategy

Two tiers, because they answer different questions and conflating them makes both worse.

| | `fixtures/synthetic/` | `fixtures/corpus/` |
|---|---|---|
| **Built by** | `generate_synthetic.py` | `fetch_corpus.py` |
| **Content** | Procedural patterns | Real photographs and real camera RAW |
| **Size** | ~4 MB, committed | ~47 MB, gitignored |
| **Needs network** | No | Yes, once |
| **Reproducible** | Byte-identical, seeded | Seeded URLs, hash-verified |
| **Used by** | Unit tests | Integration and accuracy tests |
| **Answers** | "Is the algorithm correct?" | "Does it survive real files?" |

## Why unit tests do not use real photographs

A unit test that downloads is a test that fails on a plane, fails behind a corporate
proxy, and fails *differently tomorrow* when the CDN serves a different image.

More importantly, synthetic fixtures have **known properties**, and that is what makes
them able to find bugs. A real photo cannot tell you "this frame contains 13 px of
horizontal motion blur"; a fixture can. Every one of the four bugs found while building
the focus metric was caught by a fixture asserting a *relationship* between images whose
ground truth was declared in advance:

- motion blur that aliased back into alignment because the displacement was an exact
  multiple of the pattern period
- "aperiodic detail" that was really impulse noise, indistinguishable from sensor noise
- an ROI selector using Sobel, which is identically zero on a 1px checkerboard
- a directionally asymmetric pattern that made an isotropic frame measure as anisotropic

None of those are visible in a photograph of a beach.

## Why the corpus still matters, and what synthetic cannot substitute

1. **RAW decoding.** A file named `.CR3` proves nothing about LibRaw. Only a real CR2,
   NEF, ARW, RW2 or DNG exercises a real decoder.
2. **Decoder variance.** Real JPEGs from real cameras, with real chroma subsampling,
   real quantisation tables and real EXIF.
3. **Real sensor noise.** The synthetic noise is Gaussian. Real noise is shot noise plus
   read noise plus pattern noise, and it is correlated with signal.
4. **Real bokeh.** The synthetic bokeh is a Gaussian-blurred copy of the subject. Real
   bokeh has cat's-eye geometry and spherical aberration.
5. **Whether the thresholds actually hold.** This is the important one. The motion-blur
   anisotropy threshold has a 35× margin *on a synthetic grid*. A real picket fence, a
   horizon, or a curtain could narrow that. Only the corpus can tell us.
6. **Faces** (Phase 2). Synthetic fixtures contain no people and will not be given any.

## Provenance

`fixtures/corpus/MANIFEST.json` records, for every file: the licence, the exact source
URL, the byte count and the SHA-256. `fetch_corpus.py --verify` re-checks all of them.
Provenance is auditable rather than assumed.

- **RAW** — [raw.pixls.us](https://raw.pixls.us/), the archive darktable and RawTherapee
  use for their own regression testing. Its catalogue lists 2,016 files with a licence
  per row: 1,870 CC0 and 146 CC-BY-NC. **Only CC0 is fetched.** CC-BY-NC forbids
  commercial use, and a test corpus that cannot be redistributed with the project is a
  liability, not an asset.
- **JPEG** — [picsum.photos](https://picsum.photos/), serving Unsplash photographs under
  the Unsplash License. Seeded URLs, so the corpus is reproducible.

### A correction

An earlier version of the fetcher listed 14 RAW filenames taken from a third-party web
page that claimed to mirror raw.pixls.us. **All 14 returned 404, and the repository it
cited does not exist.** The names had been inferred from a partial read of a rendered
table rather than from an API. The current fetcher reads the authoritative catalogue
directly, refuses to guess a filename, and exits non-zero when a download fails instead
of printing `FAILED` and continuing — which is how a corpus of zero files went
unnoticed the first time.

## Usage

```bash
# generate the committed synthetic fixtures (no network)
python3 tools/fixtures/generate_synthetic.py

# fetch the corpus (network, ~47 MB)
python3 tools/fixtures/fetch_corpus.py --raw 12 --jpeg 50

# prove the corpus is intact
python3 tools/fixtures/fetch_corpus.py --verify
```

## The rule

**Nothing here touches a personal photograph.** Both tools write only into `fixtures/`.
Development, tests, benchmarks and demos run exclusively against these two tiers; the
user's own library is never read, scanned, hashed, indexed or moved by any development
activity. Manual validation of anything user-visible is performed by the user, on their
data, on their machine.
