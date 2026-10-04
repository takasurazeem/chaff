# Required Skills for the Chaff Stack

The stack (ADR-0001) is:

```
Tauri v2 shell  →  Rust core  →  SQLite (WAL) catalog
                →  ONNX Runtime (ort) for faces / CLIP / aesthetic
                →  OpenAI-compatible HTTP for the VLM tier
UI: React 19 + TypeScript + Vite + Tailwind + TanStack Virtual + Zustand + TanStack Query
```

Every skill below is named exactly as it appears in this harness's skill catalogue.
Skill names are the argument to the `skill` tool.

---

## Tier 1 — Required at the start of any implementation session

These four carry the project's non-negotiable constraints. Load them before writing code.

| Skill | Why it is required here |
|---|---|
| `tdd-workflow` | The safety model is only real if it is tested. Every refusal rule, every pairing rule, and every destructive-path invariant is a test. This is the load-bearing skill for the whole project. |
| `safety-guard` | The app can move and unlink the user's photographs. This skill's job is preventing destructive operations during development on production-like data — which maps exactly onto the synthetic-fixtures-only boundary. |
| `content-hash-cache-pattern` | The thumbnail cache, the pre-move verification, and the catalog's change detection are all the same pattern: content-addressed, path-independent, auto-invalidating. This skill is the design, already worked out. |
| `error-handling` | A file-moving, multi-process, cross-platform app fails in a hundred ways. Typed errors, retries and user-facing messages are product features here, not polish. |

---

## Tier 2 — Required per subsystem

### Rust core (`src-tauri`)

| Skill | Use for |
|---|---|
| `rust-patterns` | Ownership, error handling, traits, concurrency. The `FaceEngine`/`ModelProvider`/`Scorer` traits are the architecture; this skill keeps them idiomatic. |
| `rust-testing` | Unit, integration, async, property-based tests. Property-based testing is the right tool for pairing and path-normalisation logic. |
| `database-migrations` | The SQLite catalog schema will change; migrations must be reversible and must never run against a user's catalog without a backup. |
| `benchmark-optimization-loop` | Directly serves the "blazing fast" requirement: measure, vary, re-measure. The K5/K6 targets are its exit criteria. |
| `benchmark` | Baseline capture and regression detection for the indexing and scoring pipelines before/after PRs. |
| `security-review` | The local IPC surface and the single-egress-chokepoint allowlist are security boundaries. This gets a review before Phase 1 ships. |
| `git-workflow` | Commit conventions and branching for a repo that will accumulate safety-critical code. |

### React UI (`src`)

| Skill | Use for |
|---|---|
| `react-patterns` | Hooks discipline, boundaries, Suspense, state-management decision tree. |
| `react-performance` | **The 50,000-item grid.** Waterfalls, re-render, rendering and bundle categories all apply directly to the K6 frame-time budget. |
| `vite-patterns` | Config, plugins, env, build optimisation for the webview bundle. |
| `frontend-patterns` | Component and state architecture beyond React specifics. |
| `react-testing` | React Testing Library + Vitest for components and hooks; axe assertions for the keyboard-first flows. |
| `e2e-testing` | Playwright against the built app for the structural flows that must never regress (pair resolution → warning → trash → restore). |
| `frontend-a11y` + `accessibility` | Culling is a keyboard-only activity by design. WCAG 2.2 AA is both an accessibility requirement and, here, a performance feature — neither is optional. |
| `design-system` + `make-interfaces-feel-better` + `frontend-design-direction` | The "nice user interface" requirement. Spacing, typography, hit areas, motion, and a coherent visual direction rather than default-bootstrap drift. |
| `motion-foundations` + `motion-patterns` | Grid transitions, loupe open/close, compare mode. Load foundations first; the others depend on it. Reduced-motion handling is mandatory. |
| `frontend-slides` | Only if a demo or walkthrough deck is requested. Otherwise skip. |

### Models and evaluation

| Skill | Use for |
|---|---|
| `eval-harness` | The K2–K4 gates are evals. This is the framework that makes "≥92% agreement" a build gate rather than a claim. |
| `ai-regression-testing` | A vision model that gets swapped between tiers must not silently regress tagging or scoring quality. This catches AI blind spots where the same model writes and reviews. |
| `mle-workflow` | Model selection, evaluation, deployment and rollback — the tier catalogue is a model registry in miniature. |
| `pytorch-patterns` | **Offline only.** Model conversion/quantisation experiments. PyTorch is not a shipped dependency (ADR-0002). |
| `python-patterns` + `python-testing` | **Tooling only.** Fixture generation and the eval harness. Python is not a shipped dependency. |
| `benchmark-methodology` | Scoring the scoring model: weighted dimensions with explicit rubrics instead of vibes. |

### Process and orchestration

| Skill | Use for |
|---|---|
| `orch-build-mvp` | Turn this PRD into Phase 1 through planned vertical slices with gated commits. This is the intended entry point for implementation. |
| `orch-add-feature` / `orch-change-feature` / `orch-fix-defect` / `orch-refine-code` | The per-operation lanes once Phase 1 exists. |
| `orch-pipeline` | The shared gated Research → Plan → TDD → Review → Commit engine those skills delegate to. |
| `verification-loop` | The pre-"done" check. Pairs with `delivery-gate`. |
| `delivery-gate` | Mechanically blocks declaring the work finished before the quality checks actually pass. |
| `agent-self-evaluation` | Post-task 5-axis scorecard with evidence. Use honestly, including on this deliverable. |
| `codebase-onboarding` | Regenerate architecture context as the repo grows. |

### Documentation and research

| Skill | Use for |
|---|---|
| `product-requirements` | **Used to produce `docs/PRD.md`.** Re-run it when a feature needs its own requirements pass. |
| `product-capability` | The PRD→SRS lane that pins down constraints, invariants and interfaces. The natural next step after this PRD. |
| `intent-driven-development` | Turning the Open Questions in the PRD into scoped, verifiable acceptance criteria. |
| `architecture-decision-records` | **Used to produce `docs/adr/`.** Every future framework/library/schema decision gets one. |
| `living-docs-governance` | Keep the PRD and ADRs from rotting as the code drifts from them. |
| `documentation-lookup` | Up-to-date Tauri/React/ONNX Runtime API references instead of training-data recall. |
| `research-ops` / `deep-research` | The next time a stack or model decision needs the same evidence discipline this PRD was built on. |
| `blueprint` / `plan-orchestrate` | Convert Phase 1–4 into a multi-session, multi-agent construction plan with self-contained step briefs. |
| `council` | Any decision where two of these ADRs' alternatives are genuinely close. |

---

## Explicitly constrained — read before reaching for these

| Skill | Constraint |
|---|---|
| `browser-qa`, `ui-demo`, `device-interaction`, `e2e-testing` | These drive a UI to verify it. The user-global rule for this workstation is that **the user performs all manual QA, always**; the harness builds, installs, launches and reads logs, and nothing else. `e2e-testing` is permitted **only** as an automated regression suite asserted in CI against synthetic fixtures — never as a substitute for the user judging the interface. `browser-qa` and `ui-demo` are not used for verification at all. |
| `agent-eval` | Only if the user later wants a head-to-head agent comparison. Not a product requirement. |
| `picsum` / `image-assets` | Not applicable. This project's imagery is the user's own photographs or synthetic fixtures. There is no royalty-free stock imagery in this product, and no generated imagery is ever emitted. |
| `docker-patterns` | Applicable only for a future headless-daemon distribution (Phase 4+ / Future Considerations). Not part of the desktop app. |
| `pytorch-patterns`, `python-patterns`, `python-testing` | Tooling and offline experiments only. Nothing Python ships. See ADR-0002. |
| `design-system` | Load it, but its output is a UI-token source of truth — it does not replace the user's visual judgement. Present token choices, let the user react. |

---

## Minimum set to start coding

If you are picking up Phase 1 cold, load exactly these six, in order:

```
tdd-workflow
safety-guard
content-hash-cache-pattern
rust-patterns
react-performance
accessibility
```

Then read `docs/PRD.md`, `docs/adr/0001-cross-platform-ui-stack.md` and
`docs/adr/0004-two-phase-trash-for-paired-delete.md`. Those three documents contain
every constraint that will make a wrong implementation expensive.
