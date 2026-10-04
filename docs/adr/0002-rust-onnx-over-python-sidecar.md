# ADR-0002: Rust + ONNX Runtime in-process instead of a Python ML sidecar

**Date**: 2026-10-04
**Status**: accepted (pending user review)
**Deciders**: User (Sameer), DeepSeek Harness

## Context

Chaff needs face detection, face embedding (512-d), a zero-shot/zero-frame aesthetic model,
and possibly CLIP-family embeddings for tagging. The obvious path — and the one every
tutorial takes — is a Python sidecar: FastAPI or a CLI bundled with PyInstaller/Nuitka,
spawned by the shell, spoken to over loopback HTTP.

The dominant design in the ecosystem is exactly this, and Tauri documents it
([sidecar docs](https://v2.tauri.app/develop/sidecar/), [example repo](https://github.com/dieharders/example-tauri-v2-python-server-sidecar)).
It works. It also means shipping a Python runtime, a second process lifecycle to supervise,
a local HTTP surface to secure, and a second packaging pipeline per OS.

Meanwhile the relevant models are **ONNX files**, and ONNX Runtime has first-class Rust
bindings. There is no Python-specific capability being used here — the models were never
Python, they are just usually *loaded* from Python.

## Decision

Run local inference **in-process in Rust** via the `ort` crate (ONNX Runtime bindings),
with no Python dependency anywhere in the shipped application.

- Execution providers selected by the hardware probe: TensorRT/CUDA → ROCm → DirectML →
  CoreML → CPU.
- A `FaceEngine` trait with a permissive default implementation: **OpenCV YuNet**
  (detection) + **SFace** (512-d recognition), both Apache-2.0, both small ONNX models,
  both fast. `InsightFace buffalo_l` is available as an opt-in higher-accuracy engine.
- Batch inference with a bounded worker pool; the model is loaded once and held warm.

## Alternatives Considered

### Alternative 1: Python sidecar (FastAPI + insightface + onnxruntime)
- **Pros**: `insightface` is a one-line install and the reference implementation for
  everything we need; the Python ML ecosystem is unmatched for experimentation; fast to
  prototype and easy to iterate on accuracy.
- **Cons**: Ships a Python runtime plus native wheels; PyInstaller/Nuitka bundling is a
  per-OS maintenance burden; a second supervised process; a local HTTP surface that must be
  authenticated or it becomes a file-deletion attack vector; ~150–400 MB added to the
  installer; GIL constrains the batch pipeline.
- **Why not**: It buys nothing we cannot get from ONNX Runtime directly, and it costs a
  second language, a second process, a second installer path per OS, and an extra attack
  surface — in an application whose stated hard requirement is that it never loses a file.

### Alternative 2: WASM inference in the webview (onnxruntime-web)
- **Pros**: No native code at all; trivially cross-platform; no execution-provider matrix.
- **Cons**: No CUDA/TensorRT — GPU acceleration in the webview is WebGPU-only and immature
  for these models; strongly limited memory ceiling; competes with the UI for the main
  thread; slowest option on the CPU-only tier which is a tier we must support well.
- **Why not**: It removes exactly the GPU capability the user bought a 3090 for, and makes
  the laptop tier worse.

### Alternative 3: Bundle a Python runtime but call it as a short-lived CLI per batch
- **Pros**: Sidesteps the long-lived server and the HTTP surface; simpler lifecycle.
- **Cons**: Model load cost per invocation dominates (hundreds of ms to seconds for
  detector + recogniser); process spawn overhead per batch; still ships Python.
- **Why not**: Loses the warm-model advantage that makes batch face passes fast.

### Alternative 4: PyTorch via `tch-rs` (libtorch)
- **Pros**: Pure Rust API, full PyTorch model zoo reachable, good for experimentation.
- **Cons**: libtorch is a large native dependency (~1–2 GB unpacked); slower cold start;
  CUDA version coupling is painful across three OSes; overkill for running fixed ONNX graphs.
- **Why not**: Much heavier than ONNX Runtime for the same inference, with stricter
  version pinning.

## Consequences

### Positive
- One language (Rust) for the entire native core; no inter-process boundary in the hot path.
- Installer stays small; the models are downloaded on demand with the user's consent.
- In-process means batching, memory and thread affinity are ours to control — which is what
  the throughput targets (≥30 img/s faces on CPU, ≥150/s on GPU) depend on.
- The local IPC surface only has to serve the UI, not the UI *and* a Python service.
- Apache-2.0 defaults remove the licensing question for anyone who ever shares the app.

### Negative
- Model experimentation is slower than in Python; new model architectures must be exported
  to ONNX before they can be tried.
- `ort` tracks ONNX Runtime releases; execution-provider matrix (CUDA/TensorRT/DirectML/
  CoreML/ROCm) is a real cross-platform testing burden.
- We forgo `insightface`'s pre/post-processing helpers and must implement alignment
  (5-point similarity transform), NMS, and quality gating ourselves.

### Risks
- **Accuracy regression from using SFace instead of InsightFace `buffalo_l`.** Mitigation:
  the `FaceEngine` trait makes it a config swap; evaluate both against the labelled fixture
  corpus and against K3 (≥97% cluster precision) before choosing the default; document that
  `buffalo_l` is the more accurate option for personal, non-distributed use.
- **Licensing surprise.** InsightFace's *code* is MIT but its released model packs are
  non-commercial-research only. Mitigation: permissive engines are the default; the
  non-permissive engine is opt-in, labelled in the UI, and never bundled.
- **ONNX Runtime execution-provider failures on a specific GPU/driver.** Mitigation: the
  hardware probe records which EPs actually initialised; a failed EP falls back one level
  and is reported in the Capability Report rather than crashing.
