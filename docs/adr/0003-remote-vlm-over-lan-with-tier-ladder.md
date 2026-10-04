# ADR-0003: Remote VLM over LAN behind a hardware tier ladder

**Date**: 2026-10-04
**Status**: accepted (pending user review)
**Deciders**: User (Sameer), DeepSeek Harness

## Context

The user wants a "small LLM with vision capability" to categorise photographs, and runs an
RTX 3090 box at `192.168.1.150`. They also asked — unprompted, and this is the important
part — that the app *"check what resources the system has and then pick the model based on
that."*

The research says three things that constrain the design:

1. A 24 GB card (3090/4090) comfortably runs 27B–32B vision models at Q4 with KV headroom.
   Representative picks: a 27B dense VLM (~17 GB at Q4_K_M, Apache-2.0, 262K context) as the
   quality option, and an MoE multimodal model (~18 GB, ~3.8B active per token) as the fast
   option — the MoE is markedly faster per token for similar quality.
2. Measured throughput on a 3090 for 8B-class vision models is ~80–120 tok/s, falling to
   ~25–35 tok/s at 27B. Vision inputs cost anywhere from ~730 to ~3,600 tokens per image,
   and first-token latency on a 7–8B model is 2–5 s. **Per-image cost is dominated by image
   tokens, not by decode speed** — which means resolution budgeting matters more than model
   size for throughput.
3. Ollama, as of the current generation, still does not wire up the separate vision `mmproj`
   sidecar for the newest Qwen vision models, so llama.cpp or LM Studio is the reliable path.

Sources: [vision model tier guide](https://insiderllm.com/guides/vision-models-locally/),
[Roboflow local VLM survey](https://blog.roboflow.com/local-vision-language-models/).

Meanwhile the app must still be fully useful on a laptop with no GPU (secondary persona).

## Decision

Two decisions, taken together:

**A. Speak a narrow, vendor-neutral protocol to a model server.** Chaff talks
OpenAI-compatible `/v1/chat/completions` over HTTP to a user-configured endpoint
(loopback or LAN). It does not embed a model server, does not manage one, and does not
require a particular one. llama.cpp `llama-server`, LM Studio, Ollama and vLLM are all
acceptable if they expose that shape.

**B. Select the tier at runtime from a probe, never at install time.** A hardware probe
runs at startup and re-runs on endpoint-health change, GPU OOM, or thermal event, and
walks this ladder:

| Tier | Condition | Face engine | Vision tagging |
|---|---|---|---|
| 0 — Remote GPU | configured LAN endpoint healthy | local ONNX | remote, largest model |
| 1 — Local large | ≥ 20 GB VRAM | local ONNX (GPU EP) | 27B-class dense VLM, Q4_K_M |
| 2 — Local mid | 12–19 GB VRAM | local ONNX (GPU EP) | 7–8B VLM, Q4 |
| 3 — Local small | 6–11 GB VRAM | local ONNX (GPU EP) | 4B-class int4, or 2.2B edge model |
| 4 — CPU only | no usable GPU | local ONNX (CPU) | **none** — CLIP zero-shot tags only; VLM queued |

Model candidates per tier live in a **versioned JSON catalogue**, not in code, so models can
be added without a release.

Additional requirements that follow from the research:

- **Resolution budgeting is a first-class knob.** Tagging sends a downscaled image (768 px
  long edge default) with a short output budget, because image tokens dominate cost. Full
  resolution is never sent anywhere, including to the LAN box.
- **Queue, do not fail, on outage.** In-flight VLM work is persisted and resumed when the
  endpoint returns.
- **EXIF stripped, GPS stripped** before any image leaves the process.
- **Per-image cost is measured and shown**, so the user can see the throughput/latency
  trade-off they are actually paying for.

## Alternatives Considered

### Alternative 1: Bundle a VLM in the installer
- **Pros**: Zero setup; works offline on a fresh machine; one install and done.
- **Cons**: A 27B model is ~17 GB and a 2.2B model is still ~2 GB; installer size balloons;
  the laptop tier gets a model it cannot run well; most users would download a model they
  never use.
- **Why not**: Contradicts the hardware-tiering requirement and makes the installer hostile.
  On-demand download with an explicit size/licence prompt is strictly better.

### Alternative 2: Embed llama.cpp in-process (linked as a library)
- **Pros**: No separate server to run; no HTTP; tightest possible control over the model.
- **Cons**: Builds and ships CUDA/ROCm/Metal toolchains per OS; CUDA version coupling;
  enormous CI and packaging burden; crashes in inference take down the whole app.
- **Why not**: The user already runs a GPU box. The marginal benefit does not justify
  shipping a CUDA toolchain with a photo culler.

### Alternative 3: Cloud API (OpenAI/Anthropic/Gemini vision endpoints)
- **Pros**: Best quality, no local GPU needed, trivial to integrate.
- **Cons**: Photographs of the user's family leave the machine; per-image cost; requires
  connectivity; contradicts the local-first premise of the product.
- **Why not**: The user's photographs are the sensitive asset. This is a hard no, and the
  egress allowlist in the PRD enforces it technically rather than by policy.

### Alternative 4: A fixed single model, chosen at build time
- **Pros**: Simplest possible implementation; one code path; easiest to test.
- **Cons**: Directly contradicts the user's stated requirement; breaks on the laptop; every
  hardware change becomes a code change.
- **Why not**: The user explicitly asked for capability-dependent selection.

## Consequences

### Positive
- The GPU box is used without the app depending on it; laptop culling keeps faces,
  sharpness and bursts working at tier 4.
- Model choice is data, so the model landscape moving (as it demonstrably is) does not
  require a release.
- A narrow HTTP contract means any future local runtime can be adopted without changing
  the app.
- Downscaling before send is simultaneously the throughput win and the privacy control.

### Negative
- The user must run a model server themselves; Chaff cannot do it for them.
- An OpenAI-compatible surface is a lowest-common-denominator contract — server-specific
  features (grammar files, speculative decoding, batching hints) are not portable.
- Queue-on-outage means an extra persisted work-queue with its own failure modes.

### Risks
- **Endpoint drift** — the user changes servers, the API shape shifts subtly. Mitigation: a
  connection self-test that reports exactly what it probed and what came back, rather than
  a generic "failed to connect".
- **OOM mid-batch on the local tier.** Mitigation: step down one tier, log, retry the batch
  once; never crash.
- **Throughput disappointment** — 1,000 images sounds fast until image tokens are counted.
  Mitigation: measure and display real throughput from the first batch; resolution budget is
  user-adjustable with a visible quality/speed trade-off.
- **Silent quality loss at lower tiers** — tags produced by a 4B model are not the same as
  tags from a 27B model. Mitigation: tags record the producing model and tier; switching
  tiers offers an explicit re-tag diff rather than silently mixing provenance.
