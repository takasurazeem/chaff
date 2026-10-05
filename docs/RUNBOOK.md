# Runbook

How to deploy, verify, and roll back Chaff. Written to be followed by someone who did not
build it, at a time when something is already wrong.

---

## What you are deploying

| | |
|---|---|
| **macOS** | `/Applications/Chaff.app`, installed by `tools/mac/install.sh` |
| **Linux** | a binary from `tools/linux/build_in_container.sh`, verified by `verify.sh` |
| **Catalog** | `<app-data>/chaff/catalog.db` — **never inside the photo library** |
| **Trash** | `<library>/.cull-trash/` — the only thing Chaff writes inside a library |

The catalog is per-machine. Two machines indexing the same library have two catalogs, and
neither knows about the other's decisions. That is a known limitation, not a bug: see
"Two machines, one library" below.

---

## Before you deploy

1. **The build is green.** `cargo test --workspace`, `pnpm test`, `pnpm lint`,
   `cargo clippy --workspace --all-targets`. All four, locally — CI is a second opinion, not
   the first.
2. **The version is the one you mean.** `git rev-parse --short HEAD`. Every binary carries
   its commit; `install.sh` prints it and compares.
3. **You know the rollback.** The previous binary, and where the catalog backup is.

---

## macOS

```bash
./tools/mac/install.sh
```

For a **distributable** bundle rather than a local install — the `.app` and a `.dmg` — see
[`docs/PACKAGING.md`](PACKAGING.md), which also states plainly that nothing is signed and what
that costs on each platform.

Builds, installs to `/Applications/Chaff.app`, **and verifies what landed**: it hashes the
installed binary, checks the commit stamp is inside it, and launches it. A successful build
is not a successful install — this script exists because a stale DMG was dragged onto the
machine once and the binary was seven minutes old.

**If it reports a hash mismatch:** the install did not replace the binary. Quit Chaff
(`pkill -f 'Chaff.app'`), remove `/Applications/Chaff.app`, and run it again. A running app
holds its own binary.

**Rollback:** `rm -rf /Applications/Chaff.app`, then reinstall from the previous commit:
`git checkout <sha> && ./tools/mac/install.sh`.

---

## Linux

The build needs `webkit2gtk4.1-devel` and friends, which the host may not have. Use the
container — rootless podman, nothing on the host is modified:

```bash
distrobox create --name chaff-build --image fedora:41    # once
distrobox enter chaff-build -- sudo dnf install -y webkit2gtk4.1-devel gcc-c++ clang-devel
./tools/linux/build_in_container.sh
./tools/linux/verify.sh
```

`verify.sh` **launches it and reads `/proc`** — the process must hold a display socket and
link `libwebkit2gtk-4.1.so.0`. "It compiled" and "it runs" are different claims, and only the
second one matters.

**If it fails with `libEGL … failed to create dri2 screen`:** the container has no GPU
passthrough and the webview is software-rendering. It still runs. For the CUDA path, create
the container with `--nvidia` or install on the host.

**Rollback:** `distrobox rm -f chaff-build` removes the container entirely. Nothing on the
host was touched.

---

## The GPU server (optional)

VLM tagging needs an OpenAI-compatible endpoint. On the 3090:

```bash
~/llamAmpere/build-sm86/bin/llama-server \
  -m ~/.lmstudio/models/unsloth/Qwen3.6-35B-A3B-MTP-GGUF/Qwen3.6-35B-A3B-UD-Q3_K_M.gguf \
  --mmproj ~/.lmstudio/models/unsloth/Qwen3.6-35B-A3B-MTP-GGUF/mmproj-F32.gguf \
  -c 8192 -ngl 99 -np 1 --host 0.0.0.0 --port 8080 -a chaff-vlm --jinja
```

Then `export CHAFF_VLM=http://192.168.1.150:8080` before launching Chaff. **Nothing inside the
webview chooses this** — it is read from the environment, which is what makes the egress
allowlist meaningful.

**Check it before trusting it.** The Tags panel's `test` button asks the three questions that
matter — is it up, does it have a vision model, does it honour the schema — and returns a
verdict in words. A port check does not distinguish those, and each has a different fix.

**It costs ~18 GB of VRAM** and the 3090 has 24. `pkill llama-server` stops it.

---

## Two machines, one library

The intended setup is **one library, one machine doing the writing**. A network share mounted
on both, with Chaff running on both, will produce two catalogs that disagree — and the second
machine's index will not see the first's ratings.

Sync the *photographs*; run Chaff on one side. If you must run it on both, treat the catalog
as disposable: deleting it costs a re-index, not data.

---

## When something is wrong

**The grid is empty after opening a folder.** Check the folder filter first — the tree
remembers where you were, and a remembered folder that no longer exists is cleared on open.
Then the band filter: `Review` is the default and the bands are **uncalibrated**, so on some
libraries almost everything lands there.

**Photographs are missing.** `chaff.log` in the app data directory. The indexer logs what it
could not read, and a raw format with no embedded preview is the usual answer (#8).

**A delete moved the wrong thing.** It did not: `commit_delete` takes no argument naming a
file. It commits the plan you were shown and verifies every file against the hash taken at
that moment. If a file changed in between, the whole operation aborts.

**Restore from the trash:** the Trash panel, or `restore_trash` with the operation id. The
manifest is at `<library>/.cull-trash/MANIFEST.jsonl` and is plaintext — Chaff re-checks
every path against the library root before moving anything, so a hand-edited manifest cannot
send a file outside it.

**Undo:** `Cmd/Ctrl+Z`. Ratings are reversed by writing the previous value back; a move is
reversed by **restoring the operation**, which is a different command. Session-scoped.

---

## What Chaff writes, and where

- **`<library>/.cull-trash/`** — on an explicit delete. The only thing inside a library.
- **`<library>/**/*.xmp`** — only when you run the sidecar write, and only for photographs
  you have decided about. Merged, never replaced.
- **`<app-data>/chaff/catalog.db`** — the catalog.
- **`<app-data>/chaff/thumbnails/`** — content-addressed, capped at 512 MB, evicted on write.
- **`<app-data>/chaff/models/`** — downloaded models, hash-verified before use.

Originals are never modified. There is no code path that writes to a photograph.


---

# Running the native macOS app

The SwiftUI shell. Same engine, different front end — see
[the ADR](adr/0001-two-user-interfaces.md) for why both exist.

## Build and run

```bash
cd macos
make app       # cargo builds the FFI library, swift builds the shell, bundle.sh packages it
make run       # builds and opens it
make test      # the shell's own tests
```

`make app` matters: `swift build` produces a **bare executable**, which has no Dock icon and
nothing to double-click. `bundle.sh` turns it into `build/Chaff.app`.

`make bindings` runs **cargo every time**, not behind a file target. It was `bindings: $(LIB)` for
a while, and `$(LIB)` existed — so cargo never rebuilt it and `uniffi-bindgen` regenerated the same
**stale** Swift. The symptom was `cannot find type 'PhotoDetail' in scope` for a type plainly in the
Rust.

## Signing

Three tiers, and this machine reaches the second. `docs/PACKAGING.md` has the detail; the short
version:

| tier | here |
|---|---|
| ad-hoc | yes |
| Apple Development — **not distributable** | **yes, this is what the build uses** |
| Developer ID + notarization | **no — needs a paid Apple Developer Program membership** |

The certificate cannot be generated by a script, a CI job or an agent. It needs the account holder.

## The two failures worth recognising

**`pipefail` and `head`.** The identity lookup piped `security find-identity` through `grep | head
-1`. `head` closes the pipe, `grep` takes SIGPIPE, and `pipefail` turns that into a failing command
substitution — so the assignment silently produced nothing and the build fell back to ad-hoc,
**while the same grep worked perfectly in a terminal.** A one-liner that passes by hand and fails in
a script is usually this.

**A C target with only headers produces no object file.** SwiftPM then fails at link time with
`Build input file cannot be found: .../chaff_ffiFFI.o` — naming a file the target was never going
to produce. Hence `Sources/ChaffFFI/shim.c`.

## Deployment target, and what degrades

**macOS 14**, with everything newer behind `#available` in `Sources/Chaff/Compatibility.swift`.

The target was briefly `26.0` — a hard floor — because that is what makes the system hand the
chrome Liquid Glass. It works, and it **drops every user on 14 or 15** to get a material on a
toolbar. The deployment target says what you *require*; availability says what you *prefer*.

| | macOS 26+ | macOS 14–15 |
|---|---|---|
| Floating surfaces | Liquid Glass | `.regularMaterial` |
| Button chrome | `.glass` | `.bordered` |

## Where the models come from

`<app data>/models`. The two small ones ship in the repository — YuNet at 233 KB and the CLIP
vocabulary at 78 KB — because the tests that must run everywhere need them. SFace (38 MB) and the
CLIP encoder (88 MB) download on first use, verified by blake3.

**The model server is never started by this project, on either machine.** If a pass needs one and
none is running, it says so; see `AGENTS.md`.
