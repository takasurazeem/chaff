# 1. Two user interfaces, and what would collapse them back to one

**Date:** 2026-10-05
**Status:** Accepted

## Context

Chaff began as a Tauri application: a React front end in a webview, a Rust engine behind it. It
shipped, and it works on macOS, Windows and Linux from one codebase.

Then macOS got a second, native SwiftUI shell. This records why, because "we wrote the UI twice"
is a decision that needs a better reason than novelty.

## The measurements that decided it

| | Tauri | Native SwiftUI |
|---|---|---|
| Binary | 33 MB | 5 MB |
| Resident | 1,388 MB | 100 MB |
| Webview | WebKit, always | none |

The memory figure is the one that mattered. **1.4 GB to show a grid of thumbnails** is not a
rounding error; it is WebKit plus a React tree plus a decoded-thumbnail cache that cannot be
handed back to the system the way `NSCache` can. On a 16 GB machine that is a tenth of the
budget, for a program whose whole job is to show photographs.

The binary is 6.6× larger. For an application people download once, that is the less important
number.

## What each shell is actually for

**The native shell exists for macOS, and only for macOS.** It is the platform where the memory
cost is paid on a laptop, where a culling session is hours long, and where the keyboard-first
interaction a culling tool needs is a first-class system concern — `⌘1`–`⌘5` in a menu bar a
user can *find*, rather than a shortcut in a README.

**The web shell is the cross-platform answer.** One codebase, three platforms, and it is what
Windows and Linux users get. Nothing about it is a fallback.

## The constraint that makes this affordable

**The engine is GUI-free.** `chaff-core` and `chaff-faces` have no webview dependency, no Tauri
import, and no FFI annotation. Verified three ways:

```
grep -rn tauri crates/          -> two hits, both comments
cargo tree -p chaff-core        -> no webview crate
grep -rn uniffi crates/chaff-core -> nothing
```

`chaff-cli` already runs the whole engine on a headless Linux box. So the shells are *replaceable*
and the engine is *shared* — which is the only reason a second shell is a week of work rather
than a rewrite.

Two boundaries sit on that seam:

- `crates/chaff-ffi` — UniFFI bindings for Swift, and the only crate that depends on both the
  engine and `chaff-faces`.
- `src-tauri` — 38 `#[tauri::command]` functions.

Both are thin. Both are tested. Neither contains a decision the other would make differently.

## The cost, stated plainly

**Two shells is two of everything at the edges.** Two delete confirmations, two progress
overlays, two sets of keyboard handling. The web app has had bugs the native one does not, and
the native one has had bugs the web one cannot have.

The mitigation is that the *dangerous* logic is in the engine, not the shells. `DeleteSession` —
the plan, the hashes, the refusal to move a file that changed — is one implementation shared by
both, and its invariant is a type-level test, so relaxing it stops compiling.

What is duplicated is layout and input handling. That is the cheap half.

## What would collapse them back to one

Any one of these, and the decision should be revisited rather than defended:

1. **Tauri's memory cost coming down by an order of magnitude.** If a webview shell could show a
   50,000-photograph grid in 200 MB, the reason for the native shell is gone.
2. **A cross-platform native toolkit that is genuinely good.** Rust has several. None is close.
3. **The native shell needing to run anywhere but macOS.** The moment it needs Windows support,
   the arithmetic changes completely — and the answer would be to delete it, not to port it.
4. **The engine ceasing to be GUI-free.** If a shell's concerns leak into `chaff-core`, the
   second shell becomes a fork and this decision has failed.

## Consequences

- **Every feature is built twice or not at all.** The honest position is that the native shell
  will always lag. Faces and tags arrived there a month after the web app.
- **CI covers both.** `native-macos` is a job; the Tauri shell builds on all three platforms.
- **The web UI is not deprecated.** #69 is "keep it working", not "wind it down".
- **A third shell is not on the table.** Two is the most this architecture supports before the
  edge cost outweighs the engine's reusability.
