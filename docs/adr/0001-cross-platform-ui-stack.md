# ADR-0001: Tauri v2 + React 19 for the cross-platform shell and UI

**Date**: 2026-10-04
**Status**: accepted (pending user review)
**Deciders**: User (Sameer), DeepSeek Harness

## Context

Chaff must run on Windows, macOS and Linux from one codebase, browse a 50,000-item grid
at 60 fps, hold a bounded memory footprint (the user asked specifically for memory
efficiency), and stream a lot of image bytes from a native indexing/scoring core into the
UI. The dominant memory cost is not the framework — it is the thumbnail pipeline and
whether the shell bundles a browser engine — but the framework choice determines the
ecosystem available for the parts that *are* framework-bound: virtualized grids, state,
and the skill/tooling support for quality gates.

Measured evidence for the shell decision comes from a July 2026 benchmark that built the
same screen-recorder application in four frameworks and measured bundle size, startup time
and idle memory ([betterstack](https://betterstack.com/community/guides/scaling-nodejs/tauri-vs-electron-vs-deno-vs-electrobun/)):

| Framework | Installer | Cold start | Idle RSS |
|---|---|---|---|
| **Tauri** | **57 MB** | 311 ms | **109 MB** |
| Deno Desktop | 111 MB | 242 ms | 98 MB |
| Electron | 323 MB | 273 ms | 128 MB |
| Electrobun | 418 MB | 773 ms | 208 MB |

Two honest readings of that table, both of which matter:

1. Tauri's installer is **5.7× smaller** than Electron's because it uses the OS webview
   instead of bundling Chromium. That is the real, structural win.
2. Startup and idle memory are **much closer than the marketing suggests** — Electron was
   actually *faster* to start (273 ms vs 311 ms) and only ~19 MB heavier at idle. Anyone
   choosing Tauri purely for "it starts faster and uses way less RAM" is choosing on a
   false premise. The premise that holds is installer size and not shipping a second
   browser.

## Decision

Use **Tauri v2** as the application shell and **React 19 + TypeScript + Vite + Tailwind
CSS** as the UI, with:

- **TanStack Virtual** for the image grid (only visible rows mounted, small overscan).
- **Zustand** for local UI state; **TanStack Query** for catalog queries against the Rust core.
- **Tauri asset protocol** for image bytes — thumbnails never cross the JSON IPC bridge.
- **Rust** owns all pixel work: indexing, RAW decode, thumbnails, scoring, hashing, file
  operations. The webview owns layout, interaction and presentation only.

## Alternatives Considered

### Alternative 1: Electron + React
- **Pros**: Most mature ecosystem; identical rendering and identical Chromium version on
  every OS, so zero webview-compatibility surface; vast documentation; the benchmark above
  shows it is not actually slower to start.
- **Cons**: 323 MB installer (5.7× Tauri); ships a full Chromium per app; heavier on disk
  and update bandwidth; the memory advantage over Tauri is real but small.
- **Why not**: The user asked for memory efficiency and a cross-platform app they install
  once and keep. Shipping a redundant browser to do it is the single largest avoidable cost
  in the product, and the only thing we actually give up is webview uniformity — which is a
  testable risk (CI benchmark on all three OSes) rather than an unmeasurable one.

### Alternative 2: Tauri v2 + Svelte 5
- **Pros**: Genuinely the most memory-efficient web option — no virtual DOM, fine-grained
  reactivity, smaller runtime, lower per-component overhead.
- **Cons**: Far thinner ecosystem for the one component this app lives or dies by — a
  50,000-item virtualized image grid with drag-select, synchronous compare and zoom. Fewer
  drop-in libraries, fewer reference implementations, and dramatically less skill/harness
  support for the review gates the user asked for.
- **Why not**: The framework runtime is a rounding error next to the thumbnail cache (512 MB
  capped) and the DOM nodes for pinned images. Choosing the marginally smaller runtime at
  the cost of the strongest available grid ecosystem is optimising the wrong term. Recorded
  here so the trade-off is visible if React's overhead ever becomes measurable in profiling.

### Alternative 3: Native Rust UI (egui / iced / GPUI)
- **Pros**: Lowest possible memory, no webview, no JS, the fastest possible grid, one
  language for the whole app.
- **Cons**: "A nice user interface" — an explicit user requirement — becomes expensive
  hand-built work (typography, transitions, accessibility, platform conventions). No skill
  or tooling support in this harness. Cross-platform input/IME/DPI edge cases land on us.
- **Why not**: The user asked for a nice UI, and this path trades a solved problem
  (declarative UI) for a solved problem in Rust (systems programming) that we do not need
  help with. Rejected on effort, not on capability.

### Alternative 4: Qt / PySide6
- **Pros**: Truly native, mature, excellent for image-heavy desktop tools, one language.
- **Cons**: Licensing (LGPL/commercial) and deployment of the Qt runtime; Python GIL and
  packaging pain for the ML pipeline; a weaker story for the modern UI the user wants.
- **Why not**: Worst of both worlds for this project — native-app deployment cost with a
  scripting-language performance profile.

## Consequences

### Positive
- 57 MB installer and no bundled browser.
- Rust core means RAW decode, hashing, scoring and file moves run without GIL or IPC
  marshalling cost — the "blazing fast" requirement lives where the work is.
- The asset protocol keeps image bytes out of the JSON IPC bridge, which is the single
  biggest avoidable throughput mistake in Tauri apps.
- React gives us TanStack Virtual for the grid and the deepest bench of quality-gate skills
  available in this harness (see `docs/SKILLS.md`).

### Negative
- Three webviews to support (WebView2, WKWebView, WebKitGTK). WebKitGTK is the weak link,
  particularly for large image grids and CSS filters.
- Tauri v2 has a smaller ecosystem than Electron; some primitives must be written in Rust.
- The measured startup/memory advantage over Electron is small, so the decision rests on
  installer size and architecture, not on those numbers.

### Risks
- **WebKitGTK performance regression on Linux.** Mitigation: a synthetic 50,000-item grid
  benchmark in CI on all three OSes with a per-OS frame-time budget; no CSS filters in the
  grid; `content-visibility`; decode off the main thread. If Linux fails its budget, the
  fallback is to bundle a fixed WebView2/Chromium on Linux only — a targeted reversal of
  this ADR rather than a rewrite.
- **Memory creep in the webview.** Mitigation: hard-capped LRU thumbnail cache (512 MB
  default) owned by Rust, and a CI assertion on idle RSS with a 50k catalog. Unbounded
  caches are the actual failure mode, and the framework is not the guard — the cap is.
- **Lock-in to Tauri's plugin/asset model.** Mitigation: keep the Rust core free of Tauri
  types behind plain traits, so a shell swap is possible without rewriting the engine.
