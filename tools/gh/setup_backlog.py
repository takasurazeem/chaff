#!/usr/bin/env python3
"""Create the Chaff backlog: labels, milestones, issues, and a GitHub Project board.

Idempotent. Re-running creates only what is missing, so it is safe to run again
after editing the task list below.

Why a script rather than clicking: the backlog is a design artefact derived from
docs/PRD.md. Keeping it in version control means the plan and the code move together,
and a reviewer can see exactly what was proposed.

Usage:
    python3 tools/gh/setup_backlog.py --repo takasurazeem/chaff [--dry-run]
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass, field

# ---------------------------------------------------------------------------
# Labels
# ---------------------------------------------------------------------------
LABELS: dict[str, tuple[str, str]] = {
    "phase-1":      ("0E8A16", "Culling loop MVP — the whole value proposition"),
    "phase-2":      ("1D76DB", "Faces: detect, cluster, name"),
    "phase-3":      ("5319E7", "Vision-LLM content tagging"),
    "phase-4":      ("B60205", "Interop, packaging, performance passes"),
    "area-core":    ("C2E0C6", "Rust core: indexing, catalog, decode"),
    "area-ui":      ("BFD4F2", "React UI and interaction"),
    "area-safety":  ("D93F0B", "Data-integrity and destructive-operation safety"),
    "area-ml":      ("FBCA04", "Models, inference, evals"),
    "area-infra":   ("C5DEF5", "CI, tooling, egress, hardware probe"),
    "area-deploy":  ("F9D0C4", "Builds, installs, deployment"),
    "area-docs":    ("D4C5F9", "Documentation"),
    "test":         ("0E8A16", "Test coverage or a test gate"),
    "perf":         ("FBCA04", "Performance work against a stated budget"),
    "blocked":      ("000000", "Cannot proceed without an external answer"),
}

# ---------------------------------------------------------------------------
# Milestones
# ---------------------------------------------------------------------------
MILESTONES: dict[str, str] = {
    "Phase 1 — Culling loop MVP":
        "Indexing, pairing, thumbnails, technical scoring, bursts, review UI, "
        "two-phase paired delete, hardware probe.",
    "Phase 2 — Faces":
        "Face detection, embedding, clustering, naming, incremental assignment.",
    "Phase 3 — Vision tagging":
        "Constrained-vocabulary VLM tagging, remote tier, queue on outage.",
    "Phase 4 — Interop & packaging":
        "XMP write-back, Linux performance pass, signing, packaging.",
}


@dataclass
class Task:
    title: str
    labels: list[str]
    milestone: str
    body: str
    phase: str = ""
    area: str = ""
    size: str = ""
    extra: dict = field(default_factory=dict)


def t(title, labels, milestone, area, size, body, phase=""):
    if not phase:
        phase = milestone.split(" ")[0] + " " + milestone.split(" ")[1].rstrip("—").strip()
    return Task(title=title, labels=labels, milestone=milestone, body=body,
                phase=phase, area=area, size=size)


P1 = "Phase 1 — Culling loop MVP"
P2 = "Phase 2 — Faces"
P3 = "Phase 3 — Vision tagging"
P4 = "Phase 4 — Interop & packaging"

TASKS: list[Task] = [
    # ---------------- Indexing & catalog ----------------
    t("SQLite catalog: schema, WAL mode, forward-only migrations",
      ["phase-1", "area-core", "test"], P1, "area-core", "M",
      "Catalog schema for photographs, files, groups, scores and tags; WAL mode for "
      "concurrent read while indexing; forward-only migrations that back up the catalog "
      "before touching a user's library.\n\nPRD: F1. Skill: `database-migrations`."),

    t("Recursive directory indexer, resumable, non-destructive",
      ["phase-1", "area-core", "test"], P1, "area-core", "M",
      "Walk a folder tree, classify files, feed pair resolution. Must never modify a "
      "file. Interruptible and resumable. A file that vanishes mid-index is a warning, "
      "not a crash.\n\nPRD: F1, Story 1."),

    t("Pair-resolution integration test against a real directory tree",
      ["phase-1", "area-core", "test"], P1, "area-core", "S",
      "Unit tests for `pair.rs` already pass against fabricated paths (22 tests). This "
      "adds an end-to-end run over a real tree from the synthetic fixture generator, "
      "proving the walker and the resolver agree.\n\nFixture: `fixtures/synthetic/pairtree/`."),

    t("EXIF extraction into the catalog",
      ["phase-1", "area-core"], P1, "area-core", "S",
      "Capture time, body, lens, ISO, aperture, focal length, orientation, and exposure "
      "bracket tag. Capture time and body drive burst grouping; the bracket tag is how "
      "brackets are excluded from bursts.\n\nPRD: F4."),

    t("File watcher: reconcile external changes without clobbering",
      ["phase-1", "area-core", "area-safety"], P1, "area-core", "M",
      "Detect files added, removed or modified outside Chaff (an editor writing sidecars, "
      "a sync client replacing files). Surface conflicts; never resolve them silently.\n\n"
      "PRD: Safety rule 9, risk 'library changes under Chaff'."),

    # ---------------- Thumbnails ----------------
    t("Extract the embedded camera JPEG from RAW as the fast preview path",
      ["phase-1", "area-core", "perf"], P1, "area-core", "M",
      "A RAW file already contains a camera-rendered JPEG. Extracting it avoids "
      "demosaicing an entire sensor readout just to draw a grid cell.\n\nPRD: F2. "
      "Validated against real files from `fixtures/corpus/raw/`."),

    t("LibRaw full-decode fallback when no embedded preview exists",
      ["phase-1", "area-core", "perf"], P1, "area-core", "M",
      "Some bodies and some formats have no usable embedded preview. Fall back to a real "
      "decode via LibRaw (`rsraw`). Link dynamically: LibRaw is LGPL-2.1/CDDL and dynamic "
      "linking is what keeps the licence clean.\n\nADR-0002. PRD: F2, dependencies."),

    t("Content-addressed thumbnail cache with a hard LRU byte cap",
      ["phase-1", "area-core", "area-safety", "perf", "test"], P1, "area-core", "M",
      "Thumbnails keyed by blake3 of the source file: survives renames and re-moves, "
      "self-invalidates when content changes. **Hard 512 MB cap with LRU eviction** — an "
      "unbounded cache is the actual memory failure mode, not the UI framework.\n\n"
      "PRD: F2, K6, memory requirement. Skill: `content-hash-cache-pattern`."),

    t("Stream thumbnails over Tauri's asset protocol, never base64 over IPC",
      ["phase-1", "area-core", "perf"], P1, "area-core", "S",
      "Image bytes must not cross the JSON IPC bridge. This is the single largest "
      "avoidable throughput mistake in a Tauri app and it is cheap to get right up "
      "front.\n\nPRD: F2, technical constraints."),

    # ---------------- Scoring ----------------
    t("Focus metric: subject ROI, multi-scale, local-contrast normalised",
      ["phase-1", "area-core", "test"], P1, "area-core", "L",
      "**The highest-risk slice in the MVP.** Variance of Laplacian restricted to the "
      "subject region (faces, else saliency, else centre crop), computed at two scales, "
      "normalised by local contrast so flat scenes are not punished.\n\n"
      "Adversarial fixtures already generated: `bokeh_portrait` (whole-frame says blurry, "
      "subject-ROI says sharp) and `flat_low_contrast` (low absolute variance, not "
      "blurry).\n\nPRD: The Scoring Model, Stage 1."),

    t("Motion-vs-defocus discrimination via directional edge anisotropy",
      ["phase-1", "area-core", "test"], P1, "area-core", "M",
      "Ratio of horizontal to vertical Sobel energy. Anisotropy above ~1.6 means motion "
      "blur with a direction, which is a different failure from defocus and deserves a "
      "different label in the UI.\n\nFixture: `blur_motion` vs `blur_defocus_*`.\n\n"
      "PRD: The Scoring Model, Stage 1."),

    t("Exposure clipping metrics from RAW black/white points",
      ["phase-1", "area-core", "test"], P1, "area-core", "M",
      "Clipped-highlight and clipped-shadow fractions, measured against the raw black "
      "level and white point rather than 8-bit JPEG values, so the metric reflects what "
      "the sensor actually captured.\n\nFixtures: `exposure_over`, `exposure_under`. "
      "PRD: The Scoring Model."),

    t("Noise estimation, ISO-normalised",
      ["phase-1", "area-core", "test"], P1, "area-core", "M",
      "Median absolute deviation of a high-pass residual in low-gradient patches, "
      "normalised by ISO. Must score `noise_high_iso` as noisy-but-in-focus, not "
      "blurry.\n\nFixture: `noise_high_iso`. PRD: The Scoring Model."),

    t("Shoot-relative percentile normalisation with small-shoot fallback",
      ["phase-1", "area-core", "test"], P1, "area-core", "M",
      "Rank each frame's raw metrics within its own shoot. Absolute Laplacian variance is "
      "not comparable across lenses, apertures and ISOs — this is the single most "
      "important accuracy requirement in the PRD, and the reason a silent ISO-12800 burst "
      "is not wholesale-rejected.\n\nFallback to absolute thresholds below 8 frames, "
      "clearly labelled in the UI.\n\nPRD: The Scoring Model, Stage 2."),

    t("Composite scoring with genre presets and user-tunable weights",
      ["phase-1", "area-core", "test"], P1, "area-core", "M",
      "Weighted composite over focus, exposure, noise, composition, aesthetic, "
      "expression, eyes-open. Presets for portrait / landscape / wildlife / event / "
      "street. Must be **deterministic**: same inputs and weights give the same scores.\n\n"
      "PRD: The Scoring Model, Stage 3."),

    t("Explainability: per-image reason strings for every score",
      ["phase-1", "area-core", "area-ui"], P1, "area-core", "S",
      "Every score carries a human-readable 'why', built from its largest contributors "
      "and detractors. No unexplained scores in the UI. This is a trust feature, not "
      "polish.\n\nPRD: The Scoring Model, Stage 5, Story 3."),

    # ---------------- Bursts ----------------
    t("Burst grouping by capture-time adjacency and perceptual hash",
      ["phase-1", "area-core", "test"], P1, "area-core", "M",
      "Group by camera body plus capture-time gap (default ≤2 s) *and* by DCT pHash "
      "proximity, so bursts survive a clock change and catch near-duplicates that are "
      "separated in time.\n\nPRD: F4, Story 4."),

    t("Exclude exposure brackets from burst grouping",
      ["phase-1", "area-core", "area-safety", "test"], P1, "area-core", "S",
      "A bracket is deliberate exposure or focus variation, not redundancy. Culling a "
      "bracket down to one frame destroys the intended result. Detect via the EXIF "
      "bracket tag and never collapse.\n\nPRD: F4, glossary."),

    t("Burst keeper selection with keep-2 safety default",
      ["phase-1", "area-core", "area-safety"], P1, "area-core", "S",
      "Highest composite is the keeper; safety default is **two** keepers per burst, not "
      "one, until the user changes it. Manual promotion is sticky: a promoted frame is "
      "never auto-demoted by a later re-score.\n\nPRD: Story 4."),

    # ---------------- UI ----------------
    t("Virtualized grid with a 50k-item frame-time budget",
      ["phase-1", "area-ui", "perf", "test"], P1, "area-ui", "L",
      "TanStack Virtual, only visible rows mounted, small overscan. Budget: ≥58 fps at "
      "50,000 items with no frame over 33 ms. Avoid CSS filters in the grid; WebKitGTK on "
      "Linux is the weak link.\n\nPRD: Story 1, K6. Skill: `react-performance`."),

    t("Loupe and synchronised compare mode (2–4 frames)",
      ["phase-1", "area-ui"], P1, "area-ui", "M",
      "Full-size view with zoom-to-face on double-click, landing on the sharpest detected "
      "face. Compare mode synchronises zoom and pan across 2–4 frames — the feature "
      "photographers actually choose tools for.\n\nPRD: Story 9."),

    t("Keyboard culling with remappable bindings",
      ["phase-1", "area-ui", "test"], P1, "area-ui", "M",
      "Keep / reject / flag / next / previous / compare / zoom / undo, all remappable. "
      "Actions reflected in the DB within one frame of the keypress via optimistic local "
      "write.\n\nPRD: Story 9."),

    t("Session undo stack covering flags and moves",
      ["phase-1", "area-ui", "area-safety"], P1, "area-ui", "M",
      "Undo the last n actions in session, including flags and file moves. The UI half of "
      "the reversibility promise; the manifest is the durable half.\n\nPRD: Story 9, "
      "ADR-0004."),

    t("Filters: band, date, camera, lens, tag, person",
      ["phase-1", "area-ui"], P1, "area-ui", "M",
      "Filter the grid down to a working set. Person filter arrives with Phase 2 but the "
      "filter surface is built now so it is not retrofitted.\n\nPRD: F7."),

    t("Score explanation panel",
      ["phase-1", "area-ui"], P1, "area-ui", "S",
      "Show the reason strings behind a frame's band, with the weights that produced "
      "them, and let the user re-weight from here.\n\nPRD: Story 3, F7."),

    # ---------------- Safety ----------------
    t("Two-phase trash: move with fsynced JSONL manifest",
      ["phase-1", "area-safety", "area-core", "test"], P1, "area-safety", "L",
      "Move to `.cull-trash/<date>/<original relative path>` by atomic rename on the same "
      "volume. Append and **fsync** a manifest entry — operation id, timestamp, source, "
      "destination, byte size, content hash, reason, pair-group id — before the move.\n\n"
      "PRD: F8, Safety rules 1–9. ADR-0004."),

    t("Restore from trash with hash verification",
      ["phase-1", "area-safety", "area-core", "test"], P1, "area-safety", "M",
      "Move back and verify the hash before writing. A mismatch is reported, never "
      "overwritten. Recovery must need no third-party tool.\n\nPRD: Story 8."),

    t("Paired-delete resolution and the explicit warning UI",
      ["phase-1", "area-safety", "area-ui"], P1, "area-safety", "M",
      "Resolving a delete to its pair group and showing exactly what will move: full "
      "paths, file count, total size, and which half triggered it. Cross-volume pairs get "
      "a stronger warning because the move cannot be atomic.\n\nPRD: Story 7."),

    t("Refusal rules for dangerous paths",
      ["phase-1", "area-safety", "test"], P1, "area-safety", "M",
      "Refuse: filesystem roots, detected camera-card layouts (`DCIM/` + `MISC/`), "
      "read-only mounts, paths whose real path escapes the library root via symlink, and "
      "libraries with an editor lock present. Every rule is a test.\n\n"
      "PRD: Safety rules 4–5, Story 8."),

    t("Pre-move re-hash and transactional multi-file commit with rollback",
      ["phase-1", "area-safety", "test"], P1, "area-safety", "L",
      "Re-hash every file immediately before moving and abort the whole operation if any "
      "hash differs from the indexed value. Stage all moves, verify all, commit; roll back "
      "on any failure. A pair moves together or not at all.\n\nPRD: Safety rules 2–3, "
      "Story 7."),

    t("Purge path and receipt — the only unlink call site in the codebase",
      ["phase-1", "area-safety", "test"], P1, "area-safety", "M",
      "Phase 2 of deletion. Deliberately isolated so that 'what can delete a file?' has "
      "exactly one answer. Requires explicit confirmation naming count and total size, and "
      "writes a purge receipt.\n\nPRD: Safety rule 1. ADR-0004."),

    # ---------------- Infra ----------------
    t("Hardware probe and Capability Report",
      ["phase-1", "area-infra"], P1, "area-infra", "M",
      "Probe GPU model/VRAM, which ONNX execution providers actually initialise, CPU cores "
      "and SIMD features, RAM, free disk, and reachable LAN model endpoints. Emit a "
      "plain-text report the user can read — 'why did it pick that model' must never be a "
      "mystery. Implement tiers 1–4 now; tier 0 arrives in Phase 3.\n\nPRD: tier ladder."),

    t("Single egress chokepoint with an allowlist",
      ["phase-1", "area-infra", "area-safety"], P1, "area-infra", "S",
      "All outbound network traffic through one function, allowlisted to localhost, "
      "explicitly configured LAN endpoints, and user-approved model hosts. Any other "
      "connection is a security defect, not a configuration issue.\n\n"
      "PRD: Security & Privacy."),

    t("CI: cargo test on macOS, Windows and Linux",
      ["phase-1", "area-infra", "test"], P1, "area-infra", "M",
      "The core is cross-platform or it is not cross-platform. Pair resolution in "
      "particular must be proven on all three, because that is where filesystem "
      "normalisation and case-sensitivity differences bite.\n\n"
      "Skill: `github-ops`, `deployment-patterns`."),

    t("CI: frontend test, typecheck and lint",
      ["phase-1", "area-infra", "test"], P1, "area-infra", "S",
      "Vitest for components and hooks, axe assertions for the keyboard flows, plus "
      "`tsc --noEmit` and lint.\n\nSkill: `react-testing`, `e2e-testing`."),

    t("CI: 50k grid frame-time benchmark with a per-OS budget",
      ["phase-1", "area-infra", "perf", "test"], P1, "area-infra", "M",
      "Synthetic 50,000-item grid, frame-time histogram, per-OS budget. WebKitGTK on "
      "Linux is the expected failure; if it fails, the fallback is a bundled webview on "
      "Linux only (a targeted reversal of ADR-0001, not a rewrite).\n\nPRD: K6, ADR-0001 risks."),

    t("Accessibility audit: WCAG 2.2 AA for a keyboard-only workflow",
      ["phase-1", "area-ui", "test"], P1, "area-ui", "M",
      "Culling is a keyboard-only activity, so accessibility here is also a performance "
      "feature. Focus management, contrast, screen-reader support for the grid, "
      "reduced-motion honouring.\n\nSkills: `accessibility`, `frontend-a11y`."),

    # ---------------- Deploy ----------------
    t("Deploy: macOS build, install and verify",
      ["phase-1", "area-deploy"], P1, "area-deploy", "M",
      "Build the bundle, install it on the developer machine, launch it, and read the "
      "logs. **The user performs the UI validation** — the harness installs and reads "
      "logs, nothing more.\n\nBlocked on: nothing."),

    t("Deploy: Linux server build over SSH, install and verify",
      ["phase-1", "area-deploy", "blocked"], P1, "area-deploy", "L",
      "Cross-compiling Tauri from macOS arm64 to Linux is impractical, so the build runs "
      "**on the server**. Needs: SSH username, distro and version, CPU architecture, and "
      "permission to install Tauri's Linux build dependencies "
      "(`libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`, "
      "`librsvg2-dev`, `build-essential`).\n\n"
      "**Boundary:** the deployed app must never scan a folder the user has not explicitly "
      "chosen. No default library path, ever.\n\nBlocked on: the four answers above."),

    t("Deployment runbook and rollback procedure",
      ["phase-1", "area-deploy", "area-docs"], P1, "area-deploy", "S",
      "How to build, install, verify and roll back on both targets, written so it can be "
      "followed cold. Includes what 'healthy' looks like in the logs.\n\n"
      "Skill: `deployment-patterns`."),

    # ---------------- Phase 2 ----------------
    t("FaceEngine trait plus ONNX Runtime integration in Rust",
      ["phase-2", "area-ml", "area-core"], P2, "area-ml", "L",
      "In-process inference via the `ort` crate, no Python anywhere. Execution providers "
      "selected by the hardware probe: TensorRT/CUDA → ROCm → DirectML → CoreML → CPU. "
      "Batch inference with a bounded worker pool and a warm model.\n\nADR-0002."),

    t("Default permissive face engine: OpenCV YuNet + SFace",
      ["phase-2", "area-ml"], P2, "area-ml", "M",
      "Apache-2.0 detection and 512-d recognition, small ONNX models. This is the shipped "
      "default. Implement 5-point alignment, NMS and quality gating ourselves — we forgo "
      "`insightface`'s helpers by not shipping Python.\n\nADR-0002."),

    t("Opt-in InsightFace buffalo_l engine with a licence notice",
      ["phase-2", "area-ml", "area-safety"], P2, "area-ml", "S",
      "More accurate, but **verified non-commercial**: upstream states 'ALL models are "
      "available for non-commercial research purposes only'. Never bundled, opt-in, "
      "labelled in the UI.\n\nADR-0002, PRD risk table."),

    t("Face clustering: HDBSCAN plus incremental centroid assignment",
      ["phase-2", "area-ml", "perf"], P2, "area-ml", "L",
      "Daily work assigns new faces to existing identities by cosine similarity against "
      "centroids. Full HDBSCAN re-cluster is explicit and cancellable, because it degrades "
      "past roughly 50k embeddings — a community-reported threshold this must measure for "
      "real rather than assume.\n\nPRD: Story 5, Phase 2."),

    t("Person naming UI with merge, split and undo",
      ["phase-2", "area-ui"], P2, "area-ui", "L",
      "Unnamed clusters as person cards ordered by face count; naming applies to the whole "
      "cluster; assigning a face to a person merges and relabels with undo. Names are "
      "durable identities that survive re-clustering.\n\nPRD: Story 5."),

    t("Ambiguous-face review queue",
      ["phase-2", "area-ui", "area-ml"], P2, "area-ml", "M",
      "Faces whose top-2 identity similarity falls within a margin are surfaced for review "
      "rather than silently assigned. Silence is how a tool loses trust.\n\nPRD: Story 5."),

    t("Face clustering accuracy eval harness (gate: K3 ≥97% precision)",
      ["phase-2", "area-ml", "test"], P2, "area-ml", "M",
      "Evaluate cluster precision against a labelled fixture corpus and gate the build on "
      "it. Precision means no foreign face inside a named person's cluster, which is the "
      "error users notice.\n\nPRD: K3. Skill: `eval-harness`, `ai-regression-testing`."),

    # ---------------- Phase 3 ----------------
    t("VLM client: OpenAI-compatible HTTP, schema-constrained, temperature 0",
      ["phase-3", "area-ml"], P3, "area-ml", "L",
      "Narrow vendor-neutral contract so llama.cpp, LM Studio, Ollama and vLLM all work. "
      "Grammar/JSON-schema-constrained output at temperature 0 so the same image yields "
      "the same tags. Downscale before sending: image tokens dominate cost, and it is "
      "simultaneously the throughput win and the privacy control.\n\nADR-0003."),

    t("Closed tag vocabulary with per-tag confidence and model provenance",
      ["phase-3", "area-ml", "area-ui"], P3, "area-ml", "M",
      "Reject free-form hallucinated tags. Record which model and tier produced each tag, "
      "and offer an explicit re-tag diff rather than silently mixing provenance when the "
      "tier changes.\n\nPRD: Story 6, ADR-0003."),

    t("Batch tagging with pause, resume and queue-on-outage",
      ["phase-3", "area-ml", "area-core"], P3, "area-ml", "M",
      "Throughput and ETA shown; pausable; survives a restart. When the LAN endpoint goes "
      "unhealthy mid-session, in-flight work is **queued, not failed**, and resumes.\n\n"
      "PRD: Story 6, KPI targets."),

    t("Remote tier 0: endpoint self-test with real diagnostics",
      ["phase-3", "area-infra"], P3, "area-infra", "M",
      "Report exactly what was probed and what came back, rather than a generic 'failed to "
      "connect'. Includes the measured throughput of the first batch, so the user sees the "
      "real cost on their own hardware instead of a figure from a blog post.\n\nADR-0003."),

    t("CLIP zero-shot tagging fallback for the CPU-only tier",
      ["phase-3", "area-ml"], P3, "area-ml", "M",
      "Tier 4 has no VLM at all. Rather than a dead feature, fall back to zero-shot "
      "classification against the closed vocabulary so a laptop with no GPU still gets "
      "searchable tags.\n\nPRD: tier ladder, tier 4."),

    # ---------------- Phase 4 ----------------
    t("XMP write-back: merge, never clobber, with conflict detection",
      ["phase-4", "area-core", "area-safety"], P4, "area-core", "M",
      "One sidecar per photograph, named after the RAW so both halves inherit the decision "
      "(the established RAW+JPEG convention). Opt-in, previewable before first write, "
      "merges rather than overwrites fields we do not own, and surfaces conflicts when the "
      "sidecar is newer than the catalog.\n\nPRD: Story 10."),

    t("Linux WebKitGTK performance pass",
      ["phase-4", "area-ui", "area-infra", "perf"], P4, "area-ui", "M",
      "The known weak platform for large image grids. Bring it inside the same frame-time "
      "budget as macOS and Windows, or execute the documented fallback.\n\nADR-0001 risks."),

    t("Packaging and signing for Windows, macOS and Linux",
      ["phase-4", "area-deploy"], P4, "area-deploy", "L",
      "MSI/NSIS, notarised `.dmg`, AppImage + `.deb` + `.rpm`. Notarisation and "
      "Authenticode are the slow parts; start them early in the phase."),

    t("Optional headless daemon so the 3090 box can index overnight",
      ["phase-4", "area-core", "area-infra"], P4, "area-core", "L",
      "Future consideration from the PRD. A batch mode pointed at a chosen folder, so a "
      "large library is scored before the user opens the GUI. Docker is applicable here "
      "and only here.\n\nSkill: `docker-patterns`."),
]


# ---------------------------------------------------------------------------
# gh helpers
# ---------------------------------------------------------------------------
def run(args: list[str], check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["gh", *args], capture_output=True, text=True, check=check)


def gh_json(args: list[str]):
    r = run(args)
    return json.loads(r.stdout) if r.stdout.strip() else None


def ensure_labels(repo: str, dry: bool) -> None:
    existing = {l["name"] for l in (gh_json(["label", "list", "--repo", repo, "--limit", "200", "--json", "name"]) or [])}
    for name, (color, desc) in LABELS.items():
        if name in existing:
            print(f"  label exists    {name}")
            continue
        if dry:
            print(f"  [dry] create    label {name}")
            continue
        run(["label", "create", name, "--repo", repo, "--color", color, "--description", desc])
        print(f"  label created   {name}")


def ensure_milestones(repo: str, dry: bool) -> dict[str, int]:
    existing = {m["title"]: m["number"] for m in (gh_json(["api", f"repos/{repo}/milestones?state=all&per_page=100"]) or [])}
    out: dict[str, int] = {}
    for title, desc in MILESTONES.items():
        if title in existing:
            out[title] = existing[title]
            print(f"  milestone exists  {title}")
            continue
        if dry:
            print(f"  [dry] create      milestone {title}")
            out[title] = -1
            continue
        r = run(["api", "-X", "POST", f"repos/{repo}/milestones",
                 "-f", f"title={title}", "-f", f"description={desc}"])
        num = json.loads(r.stdout)["number"]
        out[title] = num
        print(f"  milestone created {title} (#{num})")
    return out


def ensure_issues(repo: str, milestones: dict[str, int], dry: bool) -> None:
    existing = {i["title"] for i in (gh_json(["issue", "list", "--repo", repo, "--state", "all", "--limit", "300", "--json", "title"]) or [])}
    created = 0
    skipped = 0
    for task in TASKS:
        if task.title in existing:
            print(f"  issue exists    {task.title[:64]}")
            skipped += 1
            continue
        if dry:
            print(f"  [dry] create    issue {task.title[:64]}")
            created += 1
            continue

        body = task.body
        if task.size:
            body += f"\n\n---\n**Size**: {task.size} · **Area**: `{task.area}` · **Phase**: `{task.phase}`"

        args = ["issue", "create", "--repo", repo, "--title", task.title, "--body", body]
        for lbl in task.labels:
            args += ["--label", lbl]
        ms = milestones.get(task.milestone)
        if ms and ms > 0:
            args += ["--milestone", task.milestone]

        r = run(args)
        url = r.stdout.strip().splitlines()[-1] if r.stdout.strip() else "?"
        created += 1
        print(f"  issue created   {task.title[:58]:58s} {url}")

    verb = "would be created" if dry else "created"
    print(f"\n  {created} issue(s) {verb}, {skipped} already present, {len(TASKS)} total in plan")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True, help="owner/name")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--skip-project", action="store_true")
    args = ap.parse_args()

    print(f"== repo {args.repo} ==")
    print("\n-- labels --")
    ensure_labels(args.repo, args.dry_run)

    print("\n-- milestones --")
    miles = ensure_milestones(args.repo, args.dry_run)

    print("\n-- issues --")
    ensure_issues(args.repo, miles, args.dry_run)

    if args.skip_project:
        return 0

    print("\n-- project board --")
    if args.dry_run:
        print("  [dry] would create project 'Chaff Roadmap' and add all issues")
        return 0

    owner = args.repo.split("/")[0]
    proj = gh_json(["project", "list", "--owner", owner, "--limit", "50", "--format", "json"])
    number = None
    for p in (proj.get("projects", []) if proj else []):
        if p["title"] == "Chaff Roadmap":
            number = p["number"]
            print(f"  project exists  # {number}")
            break
    if number is None:
        r = run(["project", "create", "--owner", owner, "--title", "Chaff Roadmap", "--format", "json"])
        number = json.loads(r.stdout)["number"]
        print(f"  project created # {number}")

    print(f"\n  board: https://github.com/users/{owner}/projects/{number}")
    print("\n  Next: add issues to the board, which the CLI does one URL at a time.")
    print(f"        python3 {__file__} --repo {args.repo} --skip-project  # then use link_project.py")
    return 0


if __name__ == "__main__":
    sys.exit(main())
