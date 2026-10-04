#!/usr/bin/env python3
"""Add every Chaff issue to the GitHub Project board and set its custom fields.

The board is only useful if it can be grouped and filtered by Phase, Area and Size,
and GitHub's CLI sets those one item at a time. This script drives that, deriving the
values from what is already authoritative on each issue:

    Phase  <- the phase-N label
    Area   <- the area-* label
    Size   <- the "**Size**: X" line the backlog script appends to the body

Nothing is invented here. If a value cannot be derived, the field is left unset and
reported, rather than guessed.

Idempotent: re-running re-sets the same values and skips nothing, which is fine
because setting a single-select field to its current value is a no-op.

Usage:
    python3 tools/gh/link_project.py --repo takasurazeem/chaff --project 9
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys

PROJECT_ID = None
FIELDS: dict[str, dict] = {}


def run(args: list[str], check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["gh", *args], capture_output=True, text=True, check=check)


def gh_json(args: list[str]):
    r = run(args)
    return json.loads(r.stdout) if r.stdout.strip() else None


def load_project(owner: str, number: int) -> None:
    global PROJECT_ID, FIELDS
    meta = gh_json(["project", "view", str(number), "--owner", owner, "--format", "json"])
    PROJECT_ID = meta["id"]
    fields = gh_json(["project", "field-list", str(number), "--owner", owner, "--format", "json"])
    for f in fields["fields"]:
        if "options" in f:
            FIELDS[f["name"]] = {
                "id": f["id"],
                "options": {o["name"]: o["id"] for o in f["options"]},
            }


def derive(issue: dict) -> dict[str, str]:
    labels = [l["name"] for l in issue.get("labels", [])]
    out: dict[str, str] = {}

    for lbl in labels:
        m = re.fullmatch(r"phase-([1-4])", lbl)
        if m:
            out["Phase"] = f"Phase {m.group(1)}"
            break

    # Area is a list on the issue; pick the first area-* label in declaration order so
    # the result is stable rather than dependent on GitHub's label ordering.
    #
    # `safety` is deliberately first: an issue that touches file deletion must be
    # visible as a safety issue even when it is also core code, because the safety
    # surface is what a reviewer needs to find.
    priority = ["safety", "core", "ui", "ml", "infra", "deploy", "docs"]
    found = [lbl.split("-", 1)[1] for lbl in labels if lbl.startswith("area-")]
    for p in priority:
        if p in found:
            out["Area"] = p
            break

    m = re.search(r"\*\*Size\*\*:\s*([SML])", issue.get("body", "") or "")
    if m:
        out["Size"] = m.group(1)

    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--project", type=int, required=True)
    ap.add_argument("--limit", type=int, default=500)
    args = ap.parse_args()

    owner = args.repo.split("/")[0]
    load_project(owner, args.project)
    print(f"project {args.project} = {PROJECT_ID}")
    print(f"fields: {', '.join(k for k in FIELDS if k in ('Phase', 'Area', 'Size'))}\n")

    existing = gh_json(["project", "item-list", str(args.project), "--owner", owner,
                        "--limit", "500", "--format", "json"])
    already = set()
    for item in (existing.get("items", []) if existing else []):
        content = item.get("content") or {}
        if content.get("number"):
            already.add(content["number"])
    print(f"already on the board: {len(already)}\n")

    issues = gh_json(["issue", "list", "--repo", args.repo, "--state", "all",
                      "--limit", str(args.limit), "--json",
                      "number,title,url,body,labels,milestone"])
    if not issues:
        print("no issues found", file=sys.stderr)
        return 1

    added = 0
    updated = 0
    skipped_fields: list[str] = []

    for issue in sorted(issues, key=lambda i: i["number"]):
        num = issue["number"]
        title = issue["title"]

        if num in already:
            item_id = None
            for item in existing.get("items", []):
                if (item.get("content") or {}).get("number") == num:
                    item_id = item["id"]
                    break
            if item_id is None:
                print(f"  #{num:<3} on board but id not found, skipping")
                continue
        else:
            r = run(["project", "item-add", str(args.project), "--owner", owner,
                     "--url", issue["url"], "--format", "json"])
            item_id = json.loads(r.stdout)["id"]
            added += 1

        values = derive(issue)
        missing = [f for f in ("Phase", "Area", "Size") if f not in values]
        if missing:
            skipped_fields.append(f"#{num} {title[:40]} -> missing {missing}")

        for fname, fval in values.items():
            field = FIELDS.get(fname)
            if not field:
                continue
            opt_id = field["options"].get(fval)
            if not opt_id:
                skipped_fields.append(f"#{num} unknown {fname}={fval}")
                continue
            run(["project", "item-edit", "--project-id", PROJECT_ID, "--id", item_id,
                 "--field-id", field["id"], "--single-select-option-id", opt_id])
            updated += 1

        print(f"  #{num:<3} {title[:56]:56s} {'+'.join(f'{k}={v}' for k, v in values.items())}")

    print(f"\n{added} added, {len(already)} already present, {updated} field values set")
    if skipped_fields:
        print(f"\n{len(skipped_fields)} value(s) could not be derived:")
        for s in skipped_fields:
            print(f"  {s}")
    print(f"\nboard: https://github.com/users/{owner}/projects/{args.project}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
