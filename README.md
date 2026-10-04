# Chaff

> Working name. A local-first, cross-platform photo culling workstation.

Chaff indexes a folder of RAW+JPEG pairs, scores every frame for technical quality,
groups bursts and picks the keeper, clusters faces so you can name people once, auto-tags
content with a vision LLM, and treats `IMG_1234.CR3` + `IMG_1234.JPG` as **one photograph**
that moves and dies as a unit.

Everything runs on your machine or your own LAN. No telemetry, ever.

**Status**: planning. Nothing is implemented yet — this repository currently contains
requirements and architecture decisions only.

---

## Read these first

| Document | What it decides |
|---|---|
| [docs/PRD.md](docs/PRD.md) | The full product requirements: user stories, scoring model, safety model, hardware tiers, KPIs, risks |
| [docs/adr/0001-cross-platform-ui-stack.md](docs/adr/0001-cross-platform-ui-stack.md) | **The UI library decision**: Tauri v2 + React 19, with measured evidence and the rejected alternatives |
| [docs/adr/0002-rust-onnx-over-python-sidecar.md](docs/adr/0002-rust-onnx-over-python-sidecar.md) | Rust + ONNX Runtime in-process instead of a Python ML sidecar |
| [docs/adr/0003-remote-vlm-over-lan-with-tier-ladder.md](docs/adr/0003-remote-vlm-over-lan-with-tier-ladder.md) | How the app talks to a GPU box and picks a model based on the hardware it finds |
| [docs/adr/0004-two-phase-trash-for-paired-delete.md](docs/adr/0004-two-phase-trash-for-paired-delete.md) | Why deletion is reversible, hash-verified and manifest-backed |
| [docs/SKILLS.md](docs/SKILLS.md) | Which skills to load for this stack, mapped to subsystem and phase |

---

## The decisions in one paragraph

**Tauri v2 + React 19 + TypeScript** (57 MB installer, 311 ms cold start, 109 MB idle RSS,
versus Electron's 323 MB / 273 ms / 128 MB on the same benchmark app). **Rust** owns every
pixel and every file operation. **ONNX Runtime in-process** runs face detection and
embedding — no Python ships. A **vision LLM over an OpenAI-compatible HTTP endpoint** does
content tagging, chosen at runtime by a **five-tier hardware ladder** so the app is fully
useful on a laptop with no GPU and better on a 3090. **Reject is a flag; only trash moves
files; only purge unlinks them.**

## Three requirements that shape everything else

1. **Subject-weighted, shoot-normalised scoring.** Focus is measured on the subject region,
   never the whole frame, and ranked within the shoot rather than against an absolute
   threshold. Absolute Laplacian variance is not comparable across lenses and ISOs, and
   getting this wrong is how culling tools mark intentional bokeh as blur.
2. **Hardware capability is a runtime property.** The app probes and picks. It never
   installs a model without asking, and it steps down a tier on OOM instead of crashing.
3. **Irreversibility is designed out.** Every destructive operation is a move to a dated
   trash folder with a hash-verified manifest, and restore is a first-class feature.

---

## Development environment (probed 2026-10-04)

| Tool | Found |
|---|---|
| Node | v26.9.0 ✅ |
| pnpm | 12.5.1 ✅ |
| npm | 11.19.1 ✅ |
| cargo / rustc | 1.91.1 ✅ |
| ffmpeg | 9.0.2 ✅ |
| git | 2.54.0 ✅ |
| python3 | 3.9.6 ⚠️ system Python — too old for the fixture/eval tooling; use the harness's bundled Python |

This machine is an **Apple M1 Pro (Metal 4, macOS 27.0.1, arm64)** — no NVIDIA GPU. Two
consequences worth stating plainly:

- Tiers **2–4** (CoreML/Metal local inference, CPU-only) and the entire UI are developable
  and testable here. Face inference will exercise the CoreML execution provider, not CUDA.
- **Tier 0/1 (the RTX 3090) cannot be developed or validated on this machine.** That path
  is built against a stub endpoint and validated by the user against their own server. No
  claim about 3090 throughput will be made here — it will be measured there.

---

## Development boundary — non-negotiable

**This project is developed against synthetic fixtures only.**

No development, test, demo, benchmark, debugging or screenshot activity reads, writes,
moves, hashes or copies any personal photograph or photo library. Fixtures are generated
programmatically at build time and live under `fixtures/`, which is gitignored.

All user-facing validation is performed by the user, on their own data, on their own
machine. The harness builds, installs, launches and reads logs. It does not judge the UI.

---

## Planned layout

```
photo_culling/
├── docs/
│   ├── PRD.md
│   ├── SKILLS.md
│   └── adr/
│       ├── README.md
│       ├── template.md
│       └── 000{1..4}-*.md
├── src/                      # React 19 + TypeScript UI (planned)
├── src-tauri/                # Rust core: indexing, scoring, faces, file ops (planned)
├── fixtures/                 # synthetic only, gitignored (planned)
└── tools/                    # fixture generation + eval harness, Python (planned)
```

## Roadmap

- **Phase 1 — MVP**: indexing, pairing, thumbnails, technical scoring, bursts, keyboard
  review UI, two-phase paired delete, hardware probe.
- **Phase 2 — Faces**: detect, embed, cluster, name, incremental assignment.
- **Phase 3 — Tagging**: constrained-vocabulary VLM tagging, remote tier, queue on outage.
- **Phase 4 — Interop**: XMP write-back, Linux performance pass, packaging and signing.
