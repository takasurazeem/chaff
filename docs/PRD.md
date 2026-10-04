# Product Requirements Document: Chaff

> *Chaff* is a working name — separating the wheat from the chaff. Rename freely.

**Version**: 1.0
**Date**: 2026-10-04
**Author**: Sarah (Product Owner, `product-requirements` skill) + DeepSeek Harness
**Quality Score**: 94/100
**Status**: Ready for user validation

---

## Executive Summary

A hobbyist photographer shoots RAW+JPEG pairs, comes home with 2,000–5,000 frames, and
spends hours doing the same three mechanical judgements by hand: *is this sharp?*, *is
this the same person as last time?*, *is this better than the eleven frames beside it?*
Existing tools (Aftershoot, Narrative Select, FilterPixel, Imagen, Photo Mechanic) solve
this, but they are subscription products that ship your photographs to someone else's
cloud, they do not let you point them at your own GPU, and none of them treat a RAW+JPEG
pair as one indivisible photograph when you delete it.

Chaff is a **local-first, cross-platform desktop culling workstation**. It indexes a
folder, scores every frame for technical quality, groups bursts and picks the keeper,
detects and clusters faces so you can name people once and have those names stick,
auto-tags content with a vision LLM, and — centrally — understands that `IMG_1234.CR3`
and `IMG_1234.JPG` are one photograph that moves and dies as a unit.

It runs on Windows, macOS and Linux from a single codebase. It probes the hardware it
finds itself on (including a LAN GPU box such as an RTX 3090) and **automatically selects
the largest vision model that hardware can actually run**, degrading through five tiers
without ever losing the ability to cull. It never deletes anything: reject means *flagged*,
and file removal is a separate, reversible, two-phase operation with a written manifest.

**Impact**: the mechanical 80% of culling should drop from hours to minutes, and the
irreversible-mistake rate should be zero, because every destructive action is reversible
until the user explicitly empties a trash folder.

---

## Problem Statement

**Current Situation**

- Culling is the largest single time cost after a shoot, and it is almost entirely
  mechanical: focus, exposure, eyes-open, burst redundancy, who is in the frame.
- RAW+JPEG dual-record mode (extremely common) doubles file count and creates a
  corruption hazard: delete one half and you are left with an orphaned RAW you cannot
  preview quickly, or an orphaned JPEG with no editing headroom.
- Face grouping in consumer tools is either absent or cloud-only. The photographer
  re-tags the same family members after every shoot.
- Blunt "AI culling" tools make confident mistakes. The dominant complaint in head-to-head
  reviews is not speed — it is *silently dropping a photo the photographer would have kept*,
  which costs more time to recover than culling manually would have cost.
- Nothing in this space lets a hobbyist use the GPU they already own.

**Proposed Solution**

A single native desktop application that owns the culling loop end to end, keeps all
pixels on the machine (or the user's own LAN), and treats the RAW+JPEG pair as the atomic
unit of work.

**Business Impact**

This is a personal-use tool, so impact is measured in hours recovered and in trust, not
revenue. Trust is the binding constraint: a tool that is 95% right but silently wrong 5%
of the time is worth less than a manual pass, because the user must re-verify everything
to find the 5%. Every requirement below that could trade accuracy for speed resolves in
favour of accuracy, reversibility, and explainability.

---

## Success Metrics

**Primary KPIs**

| # | Metric | Target | Measurement |
|---|--------|--------|-------------|
| K1 | Median active culling time per 1,000 frames | ≤ 12 minutes (from ~90 min manual) | Instrumented in-app timer: first frame shown → cull session ended, reported locally only |
| K2 | **False-reject rate** — keeper frames placed in the Reject band | ≤ 1 in 1,000 | Divergence audit: a random 2% sample of rejects is re-surfaced for user review each session; disagreement recorded to a local counter |
| K3 | Face-cluster precision on ≥ 30-photo identities | ≥ 97% (no foreign face inside a named person's cluster) | User-corrected cluster edits are logged; foreign-face corrections ÷ total assignments |
| K4 | Sharpness classifier agreement with user judgement | ≥ 92% on a 500-frame hand-labelled fixture set | Offline eval against a labelled fixture corpus (synthetic + user-labelled later) |
| K5 | Index throughput, 10,000 RAW+JPEG pairs on SSD | ≤ 6 minutes to browsable grid | Cold-cache benchmark, 8 worker threads, logged to a benchmark JSON artifact |
| K6 | Grid scroll performance at 50,000 items | ≥ 58 fps sustained, no frame > 33 ms | `requestAnimationFrame` delta histogram captured in a dev build |
| K7 | Unrecoverable file loss | **Zero, ever** | Every delete writes a pre-move manifest entry with content hashes; a test asserts restore-from-trash succeeds for 100% of recorded operations |

**Validation**: K2–K4 are validated offline against a labelled fixture corpus before release
(`benchmark` + `benchmark-optimization-loop` skills). K5–K6 are CI benchmarks on
macOS/Windows/Linux runners. K7 is a hard gate: the test suite fails the build if any
recorded delete cannot be restored byte-identically. K1 and K3 are validated by the user
in real sessions — no automated substitution is acceptable for those.

---

## User Personas

### Primary: Sameer — the hobbyist with a home GPU

- **Role**: Serious amateur photographer. Shoots family, travel, events, occasional paid
  work. 500–5,000 frames per session.
- **Goals**: Spend the evening after a shoot on two or three photographs he loves, not on
  four hours of triage. Have his family members recognised across shoots. Use the RTX 3090
  he already owns instead of paying a subscription.
- **Pain Points**: Manual culling fatigue; has previously deleted a RAW and been left with
  orphaned JPEGs; does not trust cloud tools with family photographs; existing AI culling
  silently drops keepers and is slow on his laptop.
- **Technical Level**: Advanced — comfortable with a home server, Docker, and editing
  config files. Willing to run a model server; unwilling to babysit it.

### Secondary: the same person on a laptop, away from home

- **Role**: Same user, no GPU available, spotty network, wants to cull on a plane.
- **Goals**: Face grouping, sharpness scoring and burst selection must still work.
- **Pain Points**: Any feature that hard-requires the 3090 makes the app useless here.
- **Technical Level**: Advanced.

> **Design consequence**: hardware capability is a runtime property, not an install-time
> assumption. See *Hardware Tier Ladder*.

---

## User Stories & Acceptance Criteria

### Story 1: Index a shoot and browse it

**As** Sameer **I want to** point Chaff at a folder and see all my photographs in a fast
grid **so that** I can start culling without a separate import step.

**Acceptance Criteria**
- [ ] Given a folder, Chaff discovers RAW, JPEG, HEIC, TIFF, PNG, XMP sidecars and common
      video containers, recursively, without modifying any file.
- [ ] RAW+JPEG pairs sharing a stem are presented as **one grid item** carrying a RAW/JPEG
      badge; the count of *photographs* (not files) is what the UI reports.
- [ ] Grid scrolls at ≥ 58 fps with 50,000 items.
- [ ] Indexing is resumable: interrupting and re-running continues rather than restarts.
- [ ] A file that vanishes mid-index is skipped with a non-fatal warning, never a crash.
- [ ] Chaff refuses, with a clear message, to index a path it cannot write its catalog
      beside **only if** the user has configured a sidecar catalog; an app-data catalog
      always works as fallback.

### Story 2: Hardware-aware model selection

**As** Sameer **I want** Chaff to work out what my machine can actually do and pick
models accordingly **so that** I never pick a model that OOMs, and never lose features
because I am on the laptop.

**Acceptance Criteria**
- [ ] On first launch Chaff probes: GPU vendor/model/VRAM, driver and CUDA/ROCm/Metal
      availability, CPU core count and SIMD features, total and available RAM, free disk,
      and any user-configured LAN model endpoints.
- [ ] Chaff writes a **Capability Report** the user can read (plain text), naming the
      detected hardware and the tier chosen, with the reason.
- [ ] Tier selection follows the *Hardware Tier Ladder* (see Functional Requirements) and
      never requires the user to name a model.
- [ ] The user can override the tier; an override persists and is visibly marked as manual.
- [ ] Model downloads require explicit confirmation showing size and license. A "never
      download models" mode exists and is honoured.
- [ ] On a CUDA out-of-memory error, Chaff automatically steps down one tier, logs it, and
      retries the failed batch once — it does not crash or wedge.
- [ ] When a configured LAN endpoint transitions from healthy → unhealthy mid-session,
      in-flight VLM work is **queued**, not failed, and resumes when the endpoint returns.

### Story 3: Automatic technical quality scoring

**As** Sameer **I want** every frame scored for focus, exposure and noise **so that** the
obvious technical failures are already sorted when I sit down.

**Acceptance Criteria**
- [ ] Focus is evaluated on the **subject region** (union of detected faces, else a
      saliency-derived region, else a centre crop), never on the whole frame alone, so that
      intentional shallow depth of field is not scored as blur.
- [ ] Focus, exposure and noise metrics are **normalised relative to the shoot**: a frame's
      focus score is its rank within its own shoot's distribution, not an absolute
      threshold. (Absolute Laplacian variance is not comparable across lenses, apertures
      and ISOs; this is the single most important accuracy requirement in this document.)
- [ ] Each image carries a human-readable **"why"**: e.g. `soft (subject focus p12 in shoot)`,
      `highlights clipped 3.1%`, `ISO 12800 noise`.
- [ ] The user can re-weight the scoring dimensions; weights persist per genre preset.
- [ ] Scoring is deterministic: the same input files and the same weights produce the same
      scores. (Guards against unreproducible "why did it change?" bugs.)

### Story 4: Burst grouping and keeper selection

**As** Sameer **I want** near-identical frames collapsed to the best one **so that** I am
not scrolling through twelve versions of the same blink.

**Acceptance Criteria**
- [ ] Bursts are grouped by camera body + capture-time gap (default ≤ 2 s) **and** by
      perceptual hash proximity, so bursts survive a clock change and catch separated
      near-duplicates.
- [ ] Within a burst the highest composite scorer is marked **Keeper**; the rest are marked
      **Redundant** but remain visible and restorable with one keystroke.
- [ ] Safety valve: burst keepers default to **2**, not 1, until the user changes it.
- [ ] The burst view lets the user compare frames side by side and promote any frame.
- [ ] A promoted frame is never later auto-demoted by a re-score.

### Story 5: Face detection, naming, and similar-face grouping

**As** Sameer **I want** faces found, grouped by person, and to name each group once
**so that** I can find every photo of my daughter across four years of shoots.

**Acceptance Criteria**
- [ ] Every detected face is stored with its bounding box, a quality score, and a 512-d
      embedding — never the cropped pixels alone, so grouping is a vector operation.
- [ ] Similar faces are clustered automatically; clusters appear as unnamed person cards
      ordered by face count.
- [ ] Naming a cluster applies to every face in it and is stored as a durable identity
      that survives re-clustering.
- [ ] Assigning one face to a person also merges/relabels that face's cluster, with an undo.
- [ ] New photographs are assigned to existing named identities **incrementally** by
      cosine similarity against identity centroids; a full re-cluster is a separate,
      explicit, schedulable operation (full HDBSCAN degrades badly past ~50k embeddings).
- [ ] Ambiguous faces (top-2 identity similarity within a margin) are surfaced as
      **"needs review"** rather than silently assigned.
- [ ] False positive protection: a face below a configurable quality threshold (blur, size,
      occlusion, extreme profile) is detected but never allowed to create an identity.
- [ ] Face data is never written outside the local catalog.

### Story 6: Automatic content tagging

**As** Sameer **I want** photographs tagged by content **so that** I can search
"beach", "birthday cake", "stage" later.

**Acceptance Criteria**
- [ ] A vision LLM produces a constrained set of tags from a **closed, user-editable
      vocabulary**; free-form hallucinated tags are rejected.
- [ ] Output is schema-constrained (grammar/JSON schema) at temperature 0, so the same
      image yields the same tags.
- [ ] Tags carry a confidence and the model that produced them; changing tiers does not
      silently invalidate existing tags (re-tagging is an explicit action with a diff
      preview).
- [ ] Batch tagging shows throughput and an ETA, is pausable, and resumes after a restart.
- [ ] At the remote tier, ≥ 1,000 frames complete in ≤ 8 minutes on an RTX 3090.
- [ ] Tagging runs at **reduced resolution** and never sends full-resolution originals
      anywhere, including to a LAN endpoint.

### Story 7: Deleting a photograph deletes its pair, with a warning

**As** Sameer **I want** removing a JPEG to also remove its RAW (and vice versa) after a
warning **so that** I never end up with orphaned halves again.

**Acceptance Criteria**
- [ ] Every delete action resolves the **pair group** first and shows exactly what will
      move: full paths, file count, total size, and which half triggered it.
- [ ] The warning explicitly names the counterpart files and states the destination trash
      folder.
- [ ] A configurable **"unpaired partner" policy** governs RAW-only or JPEG-only orphans:
      *warn* (default), *always pair*, *never pair*.
- [ ] Sidecars (`.xmp`, `.acr`, `.aap`) travel with their parent image.
- [ ] If the two halves of a pair live on **different volumes**, Chaff says so and requires
      explicit confirmation, because the move cannot be atomic.
- [ ] If the counterpart file is missing, Chaff reports "unpaired — nothing to pair with"
      rather than silently proceeding.
- [ ] Chaff re-hashes every file immediately before moving it and **aborts the whole
      operation** if a hash differs from the indexed value (the file changed under us).

### Story 8: Two-phase, reversible deletion

**As** Sameer **I want** deleted photographs to land in a dated trash folder with a written
record **so that** a mistake is recoverable a week later without a third-party tool.

**Acceptance Criteria**
- [ ] Phase 1 moves files to `<library-root>/.cull-trash/<YYYY-MM-DD>/<original relative
      path>` using an atomic rename on the same volume; nothing is unlinked.
- [ ] Every move appends a JSONL manifest entry: operation id, timestamp, source path,
      destination path, byte size, content hash, reason, and the pair group id.
- [ ] Phase 2 ("Empty Trash") is the **only** code path that unlinks a file; it requires an
      explicit confirmation naming the count and total size, and writes a purge receipt.
- [ ] "Restore" verifies the hash before moving a file back and reports any mismatch
      instead of overwriting.
- [ ] The trash folder is excluded from indexing and from thumbnail generation.
- [ ] Chaff refuses to operate on: a filesystem root, a path whose real path escapes the
      library root via symlink, a read-only mount, a detected camera-card layout
      (`DCIM/` with `MISC/`), or a library with a live Lightroom catalog lock present.
- [ ] All destructive operations are single-threaded and mutually exclusive with indexing.

### Story 9: Fast, keyboard-driven review

**As** Sameer **I want** to cull at speed with the keyboard **so that** my hands never
leave the home row.

**Acceptance Criteria**
- [ ] Full keyboard culling: keep / reject / flag / next / previous / compare / zoom /
      undo, all remappable.
- [ ] Undo reverses the last *n* actions in session, including flag changes and moves.
- [ ] A compare mode shows 2–4 frames synchronised on zoom and pan.
- [ ] Zoom-to-face on double-click; the zoom always lands on the sharpest detected face.
- [ ] Every action is reflected in the DB within one frame of the keypress (optimistic
      local write, no round-trip stall).
- [ ] Thumbnails paint within 100 ms on a cache hit and 250 ms on a cold decode.
- [ ] The app remains fully usable while indexing and while VLM tagging run in the
      background; progress is shown but never blocks input.

### Story 10: Interop with an existing editor

**As** Sameer **I want** my culling decisions to be readable by Lightroom/darktable
**so that** Chaff complements my editor instead of becoming another silo.

**Acceptance Criteria**
- [ ] Ratings and reject flags are written to XMP sidecars named after the **RAW** file, so
      both halves of a pair inherit the decision (one sidecar per photograph, per the
      established RAW+JPEG convention).
- [ ] Writing sidecars is opt-in and previewable — a "what will be written" diff before
      first write.
- [ ] Chaff never rewrites an existing sidecar's fields it does not own; it merges.
- [ ] If a sidecar is newer than the catalog entry, Chaff surfaces a conflict instead of
      overwriting.

---

## Functional Requirements

### F1 — Library indexing and pairing

**Description**: Build and maintain a content-addressed catalog of a folder tree.

**Pair resolution algorithm**
1. Enumerate files; classify by extension into `RAW`, `RASTER`, `SIDECAR`, `VIDEO`, `OTHER`.
2. Group key: `(canonical_dir, lowercase_stem_after_normalisation)`. Normalisation strips
   trailing `-1`/` (1)` duplicate markers *only when the counterpart exists with the bare
   stem*, and never strips the numeric body of the filename.
3. A group with one RAW + one RASTER is a **pair**. Two RAWs with the same stem → mark
   **ambiguous**, surface to the user, never auto-resolve.
4. Orphans (RAW with no raster, raster with no RAW) are flagged and governed by the
   *unpaired partner* policy.
5. Sidecars attach to their stem's group.
6. RAW extensions: CR2 CR3 NEF NRW ARW SRF SR2 RAF ORF RW2 DNG PEF RWL SRW 3FR IIQ X3F MRW
   ERF KDC DCR MOS. Raster: JPG JPEG PNG HEIC HEIF TIF TIFF AVIF WEBP. Sidecar: XMP ACR AAP.

**Edge cases**: files with no extension (skipped, warned); uppercase/lowercase stem
mismatch (treated as a match, noted in the manifest); a stem colliding across two
directories (separate groups — directory is part of the key).

**Error handling**: unreadable file → recorded as `unreadable` with the OS error, retried
once on next index; corrupt RAW header → indexed as a RAW with no preview, flagged.

### F2 — Thumbnail and preview pipeline

- Extract the **embedded camera JPEG** from the RAW where present (fast path — a RAW
  preview exists precisely so nobody has to demosaic for a grid).
- Fall back to a full RAW decode via LibRaw only when no embedded preview exists.
- Write thumbnails as content-addressed files inside the app data dir, keyed by
  `blake3(file content)`, so a re-name or re-move does not invalidate them, and a changed
  file self-invalidates. (This is the `content-hash-cache-pattern` skill applied literally.)
- Generate three sizes: 256 px (grid), 1024 px (loupe), 2048 px (zoom). Never hold a
  full-resolution RGB buffer for grid purposes.
- Serve image bytes to the webview through Tauri's **asset protocol**, never as base64
  over the JSON IPC bridge.

### F3 — Technical quality scoring

See *The Scoring Model* below. Deterministic, subject-weighted, shoot-normalised,
explainable.

### F4 — Burst and duplicate detection

Perceptual hash (64-bit DCT pHash) plus capture-time adjacency plus face-embedding
similarity. Handles: clock changes (hash only), near-duplicates across sessions (hash +
time-of-day-independent embedding match), and multi-frame brackets (detected by EXIF
exposure-bracket tag; a bracket is **not** a burst and must not be culled to one frame).

### F5 — Face pipeline

Detect → align → embed → quality-filter → cluster → name → incrementally assign.
Pluggable `FaceEngine` trait with at least two implementations (see ADR-0002).

### F6 — Content tagging

Constrained-vocabulary VLM tagging over an OpenAI-compatible HTTP endpoint, with a local
ONNX zero-shot fallback for the CPU-only tier.

### F7 — Review UI

Virtualized grid, filmstrip, loupe, compare, keyboard culling, filters by score band /
person / tag / date / camera / lens, and a "why this score" panel.

### F8 — Paired delete and trash

Two-phase, manifest-backed, hash-verified. See *Safety & Data-Integrity Model*.

### F9 — Hardware probe and model tiering

See *Hardware Tier Ladder*.

### F10 — XMP interop

Opt-in, merge-not-clobber, one sidecar per photograph.

### Out of Scope

- RAW development / demosaicing controls / colour grading — Chaff culls, it does not develop.
- Tethering, cloud sync, mobile apps (desktop only: Windows, macOS, Linux).
- Video culling beyond extracting a representative first frame (deferred).
- Multi-user, collaboration, shared catalogs.
- Replacing or writing a Lightroom catalog (`.lrcat` is never touched).
- Any facial *recognition* against external databases. Chaff groups and labels faces the
  user already possesses; it does not identify strangers.

---

## The Scoring Model

This is the technical heart of the product and the part most likely to be built badly.
Four properties are non-negotiable: **subject-weighted**, **shoot-normalised**,
**explainable**, **deterministic**.

### Stage 1 — Measure

| Signal | Method | Notes |
|---|---|---|
| Focus | Variance of Laplacian on the subject ROI, computed at two scales; take max | Cheap, proven, and the standard baseline |
| Focus reliability | Coefficient of variation of the Laplacian response across ROI sub-tiles | A genuinely sharp frame is sharp *everywhere on the subject*; a single lucky edge is not |
| Motion vs defocus | Ratio of directional Sobel energy (H vs V) | Anisotropy > ~1.6 indicates motion blur with a direction, which is a different failure from defocus and deserves a different label |
| Local contrast | Mean gradient magnitude | Used to **normalise** focus so flat, low-contrast scenes are not punished |
| Exposure | Clipped-highlight and clipped-shadow pixel fractions | From RAW black/white points when available (`rawprepare`-style levels), else 8-bit |
| Noise | Median absolute deviation of high-pass residual in low-gradient patches, ISO-normalised | |
| Face quality | Landmark-based: eye aspect ratio, head yaw/pitch, face-box size, blur within the eye region | |
| Aesthetic | Small no-reference model (NIMA/MUSIQ-class ONNX) or CLIP-IQA prompt-pair scoring | CLIP-IQA is nearly free given CLIP is already loaded for tagging |

### Stage 2 — Normalise within the shoot

```
focus_norm(i)   = percentile_rank( focus_raw(i),  shoot_focus_distribution )
exposure_norm(i)= percentile_rank( exposure_raw(i), shoot_exposure_distribution )
noise_norm(i)   = percentile_rank( noise_raw(i),   shoot_noise_distribution )
```

Absolute thresholds do not survive contact with reality: `f/1.4 at ISO 100` and
`f/8 at ISO 6400` produce wildly different Laplacian variance for equally good photographs.
Ranking inside the shoot — the same lens, body, lighting and subject — is the only
comparison that is actually meaningful. A silent-shutter burst of a sleeping child at
ISO 12800 must not be wholesale-rejected because it is noisy; the *noisiest* frame in a
noisy shoot is still a normal frame for that shoot.

Guard: a shoot with fewer than 8 frames falls back to absolute thresholds, clearly
labelled as such in the UI, because a percentile rank over 3 frames is noise.

### Stage 3 — Composite

```
S = w_f·focus_norm + w_x·exposure_norm + w_n·(100 − noise_norm) + w_c·composition
  + w_a·aesthetic + w_e·expression + w_r·eyes_open
    
Σw = 100, all weights user-visible and adjustable
```

Genre presets override the weights: *portrait* (expression + eyes-open dominate),
*landscape* (composition + focus), *wildlife* (focus + motion), *event* (expression +
burst diversity), *street* (composition + aesthetic).

### Stage 4 — Band, never binary

| Band | Meaning | Default rule |
|---|---|---|
| **Keep** | Confidently good, or best-in-burst | top band by composite, or designated burst keeper |
| **Review** | Ambiguous — the human should look | middle band |
| **Reject** | Technically failed or redundant | bottom band, or redundant-within-burst |

**Reject never means delete.** Reject is a *flag*. Removal is a separate user action
(Story 7/8). This separation is what makes the tool trustworthy enough to use.

### Stage 5 — Explain

Every score carries a reason string built from the largest contributors to and detractors
from the composite. No unexplained scores are permitted in the UI.

---

## Safety & Data-Integrity Model

Ten rules, each of which is a test:

1. **Nothing is ever unlinked except by the purge path.** Trash-move and unlink are separate
   functions and only one of them can be called by the UI.
2. **Re-hash before move.** A file whose content differs from its index entry aborts the
   operation.
3. **Pairs move together or not at all.** Multi-file operations are transactional: stage all
   moves, verify all, then commit; roll back on any failure.
4. **Refuse dangerous roots.** Filesystem roots, camera cards, read-only mounts, and paths
   escaping the library root via symlink are all refused with an explanation.
5. **Refuse while an editor holds the library.** Detect Lightroom/darktable lock artifacts
   and refuse, with instructions.
6. **Cross-volume pairs warn loudly.** A copy+delete is not atomic; that is a different
   risk class and the user must say yes.
7. **The manifest is append-only.** JSONL, fsynced before any unlink is ever possible.
8. **Restore verifies before it writes.** Mismatch is reported, never overwritten.
9. **Purge is explicit and receipted.** Count, size, and destination named; a receipt is
   written.
10. **No automatic destructive action, ever.** Not on a timer, not on import, not
    "smart cleanup". Only the user removes files.

---

## Hardware Tier Ladder

Probed at startup, re-evaluated on endpoint health change, GPU OOM, or thermal event.

| Tier | Condition | Face engine | Vision tagging | Notes |
|---|---|---|---|---|
| **0 — Remote GPU** | A configured LAN endpoint answers healthy | Local ONNX | Remote, largest model | 9930-class box, e.g. `192.168.1.150`. Queue-on-outage. |
| **1 — Local large** | ≥ 20 GB VRAM | Local ONNX (GPU EP) | 27B-class dense VLM, Q4_K_M | ~17 GB at Q4; RTX 3090/4090 class handles 27–32B comfortably |
| **2 — Local mid** | 12–19 GB VRAM | Local ONNX (GPU EP) | 7–8B VLM, Q4 | |
| **3 — Local small** | 6–11 GB VRAM | Local ONNX (GPU EP) | 4B-class VLM, int4, or 2.2B edge model | |
| **4 — CPU only** | No usable GPU | Local ONNX (CPU EP) | **None** — zero-shot CLIP tags only; VLM work queued for a better tier | Faces, sharpness, bursts all still work |

Model candidates per tier are **configuration, not code** — a versioned JSON catalogue the
app reads, so models can be added without a release.

The probe records and reports: GPU name and VRAM, execution providers actually available
(CUDA / TensorRT / ROCm / DirectML / CoreML / Metal / CPU), driver version, CPU core count
and SIMD features, total/available RAM, free disk, and LAN endpoints reachable. The
Capability Report is plain text and one click away, because "why did it pick that model"
must never be a mystery.

---

## Technical Constraints

### Performance

- Index 10,000 pairs ≤ 6 min (SSD, warm metadata cache, 8 workers).
- Focus/exposure/noise ≥ 60 img/s on 8 performance cores; ≥ 25 img/s on 4.
- Faces ≥ 30 img/s CPU; ≥ 150 img/s on a 3090-class GPU.
- VLM tagging ≥ 1,000 frames in ≤ 8 min at tier 0.
- Grid: ≥ 58 fps at 50,000 items; thumbnail ≤ 100 ms warm / 250 ms cold.
- **Memory**: app idle RSS ≤ 250 MB with a 50k catalog and no images pinned; thumbnail
  cache hard-capped (default 512 MB) with LRU eviction — the cache must never be unbounded.
- Cold start to interactive ≤ 1.5 s.

### Security & Privacy

- **No telemetry, no analytics, no crash reporting to any third party.** Ever.
- Egress is enforced through a single chokepoint with an allowlist: `localhost`,
  the user's explicitly configured LAN endpoints, and the model-download hosts the user
  approved. Any other outbound connection is a bug and is treated as a security defect.
- The local IPC surface binds to loopback only and is token-authenticated, so no other
  process or page on the machine or LAN can drive the file-deletion API.
- Sidecars and the catalog are the only files Chaff writes inside the library, both opt-in.
- Face embeddings and identities never leave the catalog.
- Images sent to a LAN endpoint are downscaled and stripped of EXIF GPS.

### Integration

- **VLM/LM server**: OpenAI-compatible `/v1/chat/completions` over HTTP on loopback or LAN
  (llama.cpp `llama-server`, LM Studio, Ollama, or vLLM). Note: for current Qwen-generation
  vision models, llama.cpp / LM Studio is required — Ollama does not wire up the separate
  vision `mmproj` sidecar for them.
- **ONNX Runtime**: CUDA/TensorRT on Linux/NVIDIA, DirectML on Windows, CoreML on macOS,
  CPU everywhere.
- **XMP**: sidecar named after the RAW; merge, never clobber.
- **RAW**: LibRaw. **Explicitly not** a Lightroom catalog integration — `.lrcat` is read-only
  at most, and only if the user opts in.

### Technology Stack

Decided in [ADR-0001](adr/0001-cross-platform-ui-stack.md): **Tauri v2 + React 19 +
TypeScript + Vite + Tailwind**, Rust core, ONNX Runtime for local models, HTTP for remote
models. Measured basis: 57 MB installer, 311 ms cold start, 109 MB idle RSS versus
Electron's 323 MB / 273 ms / 128 MB on the same benchmark app.

### Compatibility

Windows 10 1809+ / 11 (WebView2), macOS 12+ (WKWebView), Ubuntu 22.04+, Fedora 38+,
Debian 12+ (WebKitGTK 4.1). Permissive-licensed packaging: MSI/NSIS, `.dmg`/notarised,
AppImage + `.deb` + `.rpm`.

---

## MVP Scope & Phasing

### Phase 1 — MVP (the culling loop, no AI cloud, no remote)

1. Indexing + RAW/JPEG pair resolution + SQLite catalog (F1)
2. Embedded-JPEG thumbnail pipeline with content-addressed cache (F2)
3. Subject-weighted, shoot-normalised, explainable technical scoring (F3)
4. Burst grouping + keeper selection (F4)
5. Virtualized review UI with keyboard culling + compare + undo (F7)
6. Two-phase paired delete with manifest and restore (F8)
7. Hardware probe + Capability Report, tiers 1–4 (F9)

**MVP definition**: the user can point Chaff at a shoot, cull it with the keyboard in a
fraction of the manual time, and trust that nothing is lost. That is the whole value
proposition; everything else is amplification.

### Phase 2 — Faces

Face detect/embed/cluster, naming, incremental assignment, person filters, quality-gated
cluster creation (F5), and face-driven zoom.

### Phase 3 — Vision tagging and remote tier

VLM tagging with closed vocabulary and schema-constrained output, tier 0 remote serving,
queue-on-outage, tag search (F6).

### Phase 4 — Interop and polish

XMP write-back and merge, raw-pyramid/format edge cases, Linux WebKitGTK performance pass,
packaging and signing for all three OSes, accessibility audit.

### Future Considerations

- Aesthetic model fine-tuned on the user's own keep/reject history (the training signal
  falls out of normal use for free).
- Duplicate-across-drives detection.
- Optional headless daemon so the 3090 box can index a library overnight.
- Plugin surface for custom scoring modules.

---

## Risk Assessment

| Risk | Prob. | Impact | Mitigation |
|---|---|---|---|
| **False rejects erode trust and the user abandons the tool** | High | High | Three bands not binary; rejects flag-only, never delete; keep-2-in-burst default; explainable reasons; 2% divergence audit per session; K2 gate at 1/1000 |
| **Sharpness metric marks intentional bokeh/panning as blur** | High | High | Subject ROI only; shoot-relative normalisation; local-contrast normalisation; directional motion detection; the single highest-priority accuracy work in Phase 1 |
| **VLM hallucinated tags poison search** | Medium | Medium | Closed vocabulary; grammar/JSON-schema-constrained decoding; temperature 0; confidence + model provenance stored; re-tag is explicit with a diff |
| **Model licensing blocks distribution** | Medium | Medium | Confirmed by source: InsightFace `buffalo_l`/`antelopev2` are *non-commercial research only*, so they can never be bundled. `FaceEngine` trait; default to Apache-2.0 OpenCV YuNet + SFace; InsightFace offered as an opt-in, never-bundled, clearly-labelled personal-use upgrade. LibRaw ships dynamically linked under LGPL-2.1 |
| **HDBSCAN degrades past ~50k embeddings** | Medium | Medium | Incremental centroid assignment for daily work; full re-cluster only on explicit request, with progress and cancellation |
| **Cross-volume delete is not atomic** | Medium | High | Detect and warn; require explicit confirmation; per-volume trash folders preferred |
| **WebKitGTK performs poorly on large grids on Linux** | Medium | Medium | Synthetic 50k benchmark in CI on all three OSes; avoid CSS filters in the grid; off-main-thread decode; `content-visibility`; per-OS perf budget |
| **Hardware variance makes tier selection wrong** | Medium | Medium | Capability Report for transparency; auto step-down on OOM; user override that persists |
| **IPC bottleneck when streaming thumbnails** | Medium | Medium | Asset protocol for bytes, JSON IPC only for control; never base64 images over IPC |
| **The library changes under Chaff (editor writing sidecars)** | Medium | High | Re-hash before move; abort on mismatch; file watcher reconciles; conflict surfaced not resolved silently |
| **Scope creep into RAW development** | Medium | Medium | Out-of-scope list is explicit and treated as a contract |
| **Deleting the wrong thing through a UI bug** | Low | Catastrophic | Single destructive chokepoint; transaction-with-rollback; hash verification; refusal rules; every rule is a test |

---

## Dependencies & Blockers

**Dependencies**

- **LibRaw** (via Rust bindings) for RAW decode — triple-licensed LGPL-2.1 / CDDL-1.0 /
  LibRaw commercial, **verified**: "To use the LibRaw library in an application, you can
  choose the license that better suits your needs." LGPL-2.1 permits commercial use when
  the library is dynamically linked, which is how we ship it
  ([LibRaw licensing](https://www.libraw.org/node/2228)).
- **ONNX Runtime** for local inference — MIT; execution providers vary.
- **Face model packs** — `buffalo_l` and `antelopev2` are **non-commercial only, verified**:
  the upstream model zoo states "ALL models are available for non-commercial research
  purposes only" and the distributed package carries a
  `model-distribution-disclaimer-license`
  ([InsightFace model zoo](https://github.com/deepinsight/insightface/blob/master/model_zoo/README.md),
  [Hugging Face mirror](https://huggingface.co/deepghs/insightface/tree/main/buffalo_l)).
  This is why ADR-0002 defaults to Apache-2.0 OpenCV YuNet + SFace.
- **llama.cpp / LM Studio** on the GPU host for VLM serving — the user runs this; Chaff only
  speaks the OpenAI-compatible HTTP API to it.
- **A labelled fixture corpus** for K2–K4. Generated synthetically at build time for
  development; user-labelled later for real validation.
- **Tauri v2** toolchain and platform webview runtimes.

**Known Blockers**

- **None for Phase 1.** Phase 1 deliberately requires no GPU, no network, and no model
  downloads, which is what makes it buildable and testable immediately.
- Phase 3 requires a reachable model server on the user's LAN. As of this writing
  `192.168.1.150:8080/health` does not answer, so Phase 3 development proceeds against a
  stub endpoint and is validated by the user once their server is running.

---

## Appendix

### Glossary

- **Pair** — one RAW plus one raster file sharing a stem; one *photograph*, two files.
- **Burst** — consecutive frames of one moment; cull to a keeper.
- **Bracket** — deliberate exposure/ focus variation; **not** a burst, not culled to one.
- **Band** — Keep / Review / Reject. A flag, never a deletion.
- **Band vs Delete** — bands classify; trash removes. Only trash moves files, only purge
  unlinks them.
- **Tier** — a hardware capability class selecting model size (0 remote … 4 CPU-only).
- **ROI** — region of interest; the subject region a focus score is computed over.
- **Shoot-relative normalisation** — scoring a frame by rank within its own shoot.
- **Capability Report** — plain-text record of probed hardware and the chosen tier.

### References

Stack and framework evidence:
- [Tauri vs. Electron vs. Deno Desktop vs. Electrobun — measured comparison (July 2026)](https://betterstack.com/community/guides/scaling-nodejs/tauri-vs-electron-vs-deno-vs-electrobun/)
- [Tauri v2 sidecar / external binary documentation](https://v2.tauri.app/develop/sidecar/)

Vision model landscape:
- [Best Vision Models You Can Run Locally — per-GPU-tier guide (Feb 2026, updated Jul 2026)](https://insiderllm.com/guides/vision-models-locally/)
- [Best Local Vision-Language Models — Roboflow](https://blog.roboflow.com/local-vision-language-models/)
- [SmolVLM: Redefining small and efficient multimodal models](https://arxiv.org/html/2504.05299v1)

Faces and clustering:
- [InsightFace ArcFace — additive angular margin loss](https://www.insightface.ai/research/arcface)
- [Face clustering using hierarchical density-based methods](https://act-labs.github.io/posts/facenet-clustering/)
- [Incremental HDBSCAN implementation](https://github.com/femelo/incremental-hdbscan)

Sharpness and culling practice:
- [Autofocus using OpenCV: comparative study of focus measures](https://opencv.org/autofocus-using-opencv-a-comparative-study-of-focus-measures-for-sharpness-assessment/)
- [Camera focus in computer vision — Roboflow](https://blog.roboflow.com/computer-vision-camera-focus-guide/)
- [How to cull RAW+JPEG pairs as one frame](https://cullkit.com/blog/raw-jpeg-pair-culling-workflow)
- [Photo-Sync-Cleaner — bidirectional RAW/JPG orphan cleanup](https://github.com/luoqi2112/Photo-Sync-Cleaner)

Competitive landscape:
- [imagic vs Aftershoot vs FilterPixel vs Imagen vs Narrative Select](https://imagic.ink/blog/ai-photo-culling-software-comparison-2026)
- [Best photo culling software — 6 tools tested on 3,000 RAWs](https://filterpixel.com/best-ai-photo-culling-software)

RAW handling in Rust:
- [rsraw — Rust bindings for LibRaw](https://github.com/Hexilee/rsraw)
- [rawloader — pure-Rust RAW extraction](https://github.com/pedrocr/rawloader)

Licensing (verified in-session, load-bearing for distribution):
- [LibRaw licensing — LGPL-2.1 / CDDL-1.0 choice](https://www.libraw.org/node/2228)
- [InsightFace model zoo — "ALL models non-commercial research purposes only"](https://github.com/deepinsight/insightface/blob/master/model_zoo/README.md)

### Source reliability notes

Evidence in this document is graded, because the sources are not equal:

- **Measured benchmarks** (the Tauri/Electron table) — a controlled same-app comparison.
  Treat as strong for *relative* numbers, still one machine and one application.
- **Model guidance** (tier picks, throughput ranges, the Ollama `mmproj` caveat) — a
  practitioner guide plus a vendor survey. Treat as directional, **not** as a guarantee.
  Every throughput number is re-measured on the user's own hardware by the Capability
  Report before it is shown in the UI, and the PRD's KPI targets are the contract, not
  these figures.
- **Clustering degradation past ~50k embeddings** — community report, not a peer-reviewed
  measurement. It motivates the incremental-assignment design in Story 5, which is a good
  design regardless of the exact threshold. The real limit for this library must be
  measured against a synthetic 50k-embedding fixture before the Phase 2 design is frozen.

### Open Questions for the User

Each of these is marked with what it would actually change, so you can see which are
cheap to answer and which are load-bearing.

| # | Question | Changes | Cost to answer late |
|---|----------|---------|---------------------|
| 1 | **Product name** — is *Chaff* acceptable? | Cosmetic, but it is in package ids and directory names | Cheap now, annoying later |
| 2 | **Default genre preset** — which shoot type do you cull most? | Default scoring weights only | Trivial — it is a config value |
| 3 | **Keep-2-in-burst** — is two keepers right, or does it defeat the purpose? | Burst safety default | Trivial, but changes what you see on day one |
| 4 | **Sidecar writing** — write XMP from Phase 1, or leave the library untouched? | Whether Phase 1 can write inside your library at all | **Load-bearing.** Changes the Phase 1 scope and the refusal rules |
| 5 | **Trash location** — `.cull-trash/` in the library, or app-data? | **Reverses ADR-0004's core decision.** In-library is atomic; app-data is cross-volume and non-atomic for libraries on other drives | **Load-bearing.** Changes the safety guarantees |
| 6 | **Face engine licensing** — personal use only, or might you share this? | **Flips the default in ADR-0002** between Apache-2.0 OpenCV models and the more accurate non-commercial InsightFace pack | **Load-bearing for accuracy.** Changes the K3 target's feasibility |
| 7 | **Model server** — when `192.168.1.150:8080` returns, which one is it? | Which API shape and catalogue entries Phase 3 targets | Cheap — the abstraction absorbs it, but it blocks Phase 3 validation |

**Recommendation**: answer 4, 5 and 6 before Phase 1 starts. Answers 1, 2, 3, 7 can wait
until they come up.

### Phase 1 Effort Estimate

Order-of-magnitude, for planning only — not a commitment:

| Slice | Relative effort | Note |
|---|---|---|
| Indexing + pair resolution + catalog | Medium | Pairing edge cases (Unicode normalisation, case-insensitive volumes, ambiguous stems) are the hidden cost |
| Thumbnail pipeline | Medium | Embedded-JPEG extraction is the fast path; the LibRaw fallback is where the platform pain lives |
| Technical scoring | **High** | The highest-risk slice. Subject ROI + shoot normalisation + explainability is most of the value and most of the accuracy work |
| Burst grouping | Low | Well-understood once pHash and time adjacency exist |
| Review UI (grid, loupe, compare, keyboard) | High | The visible product; also where the 50k-frame budget is won or lost |
| Two-phase trash + manifest + restore | Medium | Small code, large test surface — this is the slice that must be exhaustively tested |
| Hardware probe + Capability Report | Low–Medium | The execution-provider matrix is the fiddly part |

The two High slices are where the schedule risk is concentrated, and they are independent
of each other, so they can proceed in parallel.

---

## Development Boundary (Non-Negotiable)

This project is developed against **synthetic fixtures only**. No development, test, demo,
screenshot, benchmark, or debugging activity reads, writes, moves, hashes, copies, or
otherwise touches any personal photograph or library. Fixtures are generated
programmatically at build time (procedural noise, gradients, checkerboards, synthetic
EXIF-written RAW/JPEG stem pairs) and live under `fixtures/`, which is gitignored.

All user-facing validation is performed by the user on their own data on their own machine.
The harness builds, installs, launches, and reads logs; it does not judge the UI.

---

*This PRD was created through research-driven requirements gathering with quality scoring
across business, functional, UX, and technical dimensions.*
