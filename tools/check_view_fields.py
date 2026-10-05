#!/usr/bin/env python3
"""Check that every TypeScript view type matches the Rust struct it mirrors.

# Why this exists

Tauri v2 converts command *arguments* to snake_case for Rust, but **return values are
serialised exactly as the struct is written**. There is no `rename_all` here, so a Rust
field `op_id` arrives in JavaScript as `op_id` — and a TypeScript interface declaring
`opId` compiles cleanly and is `undefined` at runtime.

That is not hypothetical. The whole trash panel read `opId`; restore and purge were broken,
and `tsc` could not see it because it only checks its own declaration. The bug was found by
accident, while fixing an unrelated one.

This runs in CI so the next one is found on purpose.

# What it checks

Every `pub struct ...View` / `...Report` in `src-tauri/src/commands.rs`, against the
same-named `interface` in `src/api.ts`. Field sets must match exactly.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RUST = ROOT / "src-tauri" / "src" / "commands.rs"
TS_FILES = [ROOT / "src" / "api.ts", ROOT / "src" / "types.ts"]

# Only the shapes that cross the IPC boundary back to the frontend.
SUFFIXES = ("View", "Report")


def rust_structs(text: str) -> dict[str, set[str]]:
    """`pub struct Name { pub field: Type }` for the view structs."""
    out: dict[str, set[str]] = {}
    for m in re.finditer(r"pub struct (\w+)\s*\{(.*?)\n\}", text, re.S):
        name, body = m.group(1), m.group(2)
        if not name.endswith(SUFFIXES):
            continue
        fields = set()
        for line in body.splitlines():
            fm = re.match(r"\s*pub (\w+):", line)
            if fm:
                fields.add(fm.group(1))
        if fields:
            out[name] = fields
    return out


def ts_interfaces(text: str) -> dict[str, set[str]]:
    out: dict[str, set[str]] = {}
    for m in re.finditer(r"export interface (\w+)\s*\{(.*?)\n\}", text, re.S):
        name, body = m.group(1), m.group(2)
        fields = set()
        for line in body.splitlines():
            fm = re.match(r"\s*(\w+)\??:", line)
            if fm:
                fields.add(fm.group(1))
        if fields:
            out[name] = fields
    return out


def main() -> int:
    rust = rust_structs(RUST.read_text())
    # **Both files.** Half the view types live in `types.ts`, and checking only `api.ts`
    # reported four false "missing" results the first time it ran — a check that cries wolf
    # is one that gets switched off.
    ts: dict[str, set[str]] = {}
    for f in TS_FILES:
        ts.update(ts_interfaces(f.read_text()))

    problems = 0
    checked = 0

    for name, fields in sorted(rust.items()):
        if name not in ts:
            print(f"  {name}: declared in Rust, missing from the TypeScript types")
            problems += 1
            continue
        checked += 1
        missing = fields - ts[name]
        extra = ts[name] - fields
        if missing:
            print(f"  {name}: TypeScript is missing {sorted(missing)}")
            problems += 1
        if extra:
            print(f"  {name}: TypeScript declares {sorted(extra)}, which Rust never sends")
            problems += 1

    if problems:
        print(
            f"\n{problems} mismatch(es) across {checked} view types.\n"
            "A field Rust does not send is `undefined` at runtime and `tsc` cannot see it."
        )
        return 1

    print(f"  {checked} view types match their Rust structs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
