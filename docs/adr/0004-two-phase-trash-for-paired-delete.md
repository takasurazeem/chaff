# ADR-0004: Two-phase dated trash for paired deletion

**Date**: 2026-10-04
**Status**: accepted (pending user review)
**Deciders**: User (Sameer), DeepSeek Harness

## Context

The user's headline requirement — *"if I remove a jpeg it automatically removes the raw
image along with it and vice versa with a warning"* — is simultaneously the most valuable
and the most dangerous feature in the product. It is the feature that fixes orphaned halves,
and it is the feature that can destroy a photograph with one mis-click.

Three facts shape the design:

1. The RAW+JPEG pair convention is well established: the pair is **one photograph**, and
   every keep/reject/move must apply to both or you get orphaned halves. The standard
   interop answer is a single XMP sidecar named after the RAW, so both halves inherit the
   decision.
2. There is an existing open-source precedent for bidirectional orphan cleanup — detecting
   `raw_to_jpg` (JPEG with no RAW) and `jpg_to_raw` (RAW with no JPEG) mismatches — which
   confirms orphan detection is a real, recurring pain worth solving explicitly.
3. `rename()` is atomic within a volume and is **not** atomic across volumes. A library on
   an external drive with trash on the boot volume turns a safe move into a copy+delete,
   which is a categorically different risk.

## Decision

**Reject is a flag; only trash moves files; only purge unlinks them.**

Phase 1 — *Trash*: move the resolved pair group to
`<library-root>/.cull-trash/<YYYY-MM-DD>/<original relative path>` using an atomic rename
on the same volume, after re-hashing every file and comparing against the indexed hash.
Append a JSONL manifest entry per operation (operation id, timestamp, source, destination,
size, hash, reason, pair-group id) and fsync it **before** the move.

Phase 2 — *Empty Trash*: the only code path in the application that calls `unlink`. Requires
explicit confirmation naming count and total size, and writes a purge receipt.

Supporting rules:
- Sidecars travel with their parent.
- Cross-volume pairs are detected and require explicit confirmation.
- Unpaired partners are governed by a policy: *warn* (default), *always pair*, *never pair*.
- Refusals: filesystem roots, camera-card layouts, read-only mounts, symlink escapes from
  the library root, and libraries with an editor lock present.
- Multi-file operations are transactional: stage all, verify all, commit; roll back on any
  failure.
- Restore verifies the hash before moving back, and reports a mismatch instead of
  overwriting.

## Alternatives Considered

### Alternative 1: OS Trash / Recycle Bin
- **Pros**: Familiar recovery path; Finder/Explorer integration; no custom trash folder in
  the library; the OS handles per-volume semantics.
- **Cons**: Restoring a *pair* is two separate manual operations and the pairing knowledge
  is lost; Linux desktop-trash support varies by desktop environment and by filesystem;
  no manifest, so nothing is auditable; no "why did this go" record.
- **Why not**: It solves single-file recovery and fails at exactly the thing this feature
  exists for — keeping a pair together through both the delete and the restore.

### Alternative 2: Permanent delete with an undo journal
- **Pros**: Simplest code path; no disk space held hostage; immediate space reclamation.
- **Cons**: Unguinhole: if the app crashes or the journal is lost, the photograph is gone.
  Undo only works within a session.
- **Why not**: Irreversible by default is unacceptable for family photographs. Space is
  cheap; a lost photograph is not.

### Alternative 3: XMP reject flags only — never move files
- **Pros**: Safest conceivable; zero file operations; perfect interop.
- **Cons**: The library never actually shrinks; the user still has to do the real deletion
  in Lightroom; the headline requirement goes unmet.
- **Why not**: It sidesteps the user's actual problem. It is, however, offered as the
  *default* until the user opts into file movement — see Consequences.

### Alternative 4: Move to a trash folder in the OS app-data directory
- **Pros**: Invisible in the library; consistent location; survives a library reorganisation.
- **Cons**: Almost always on a different volume from a library on an external drive →
  copy+delete for every operation → slow and non-atomic, which is the exact risk we are
  trying to remove.
- **Why not**: Optimises tidiness over the atomicity that makes the feature safe.

## Consequences

### Positive
- Every destructive action is reversible until the user explicitly empties trash — which is
  a genuinely different trust posture from "we moved it somewhere, hope it's there".
- The manifest makes every operation auditable and testable: the test suite can assert that
  100% of recorded operations restore byte-identically.
- Re-hash-before-move protects against the real-world case where an editor rewrote a sidecar
  or a sync client replaced a file between indexing and culling.
- Pairing knowledge is preserved through both directions of the operation.

### Negative
- Trash holds disk space until emptied; a heavy cull of 3,000 RAW frames can hold hundreds
  of gigabytes.
- A visible `.cull-trash/` directory inside the library will confuse other tools — it must
  be excluded from indexing, and users will ask about it.
- Same-volume trash means a library spread across two drives needs a trash folder per drive.

### Risks
- **The user empties trash in a hurry and destroys a keeper.** Mitigation: purge names count
  and total size; the divergence audit surfaces suspicious rejects *before* purge; a
  configurable retention period is a Phase 4 candidate (deliberately not in the MVP, since
  an automatic timer is the beginning of automatic destruction).
- **`.cull-trash/` inside a library that is itself backed up or synced** — cloud sync clients
  will upload trashed files. Mitigation: detect known sync roots (Dropbox/iCloud/OneDrive
  markers) and recommend the app-data trash location instead, with the cross-volume warning.
- **Filesystem quirks on case-insensitive or Unicode-normalising volumes** (macOS APFS,
  Windows NTFS) mean a "same stem" check that works on Linux can mis-pair on macOS.
  Mitigation: normalise with a documented Unicode form and test pairing on all three OSes
  with a fixture set covering case and normalisation variants.
- **A user's existing tool also moves files** (a photo manager, a sync client). Mitigation:
  the pre-move re-hash catches concurrent modification; the file watcher reconciles
  external changes and surfaces conflicts instead of resolving them silently.
