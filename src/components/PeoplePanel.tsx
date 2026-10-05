/**
 * Suggested people, and the tools to correct them.
 *
 * ## A group is a suggestion until a human says otherwise
 *
 * Nothing here is a name until someone types one. The wording throughout reflects that —
 * "12 groups", not "12 people" — and it is not pedantry: treating a cluster as fact is how a
 * stranger's face ends up under someone's name.
 *
 * ## Naming confirms
 *
 * Typing a name sets `confirmed`, which is what makes the next clustering pass leave the
 * group alone. There is deliberately no separate "confirm" button: a name a user has typed
 * *is* the confirmation, and a second step to ratify it is a step nobody takes.
 *
 * ## Merging and splitting are the corrections that matter
 *
 * Clustering is never right about everyone. Merge says "these are one person"; split says
 * "these are two". Both mark the result confirmed, because a correction that the next pass
 * undoes is not a correction.
 */
import { useCallback, useEffect, useState } from "react";
import {
  ambiguousFaces,
  deletePerson,
  listPeople,
  mergePeople,
  namePerson,
  runFacePass,
  splitPerson,
  type AmbiguousFaceView,
  type FacePassReport,
  type PersonView,
} from "../api";

interface Props {
  libraryId: number;
  /** Called after a pass or a correction, so the grid can pick up new face counts. */
  onChanged: () => void;
  onSelectPerson: (personId: number | null) => void;
  selectedPerson: number | null;
}

type Mode =
  | { kind: "idle" }
  | { kind: "merging"; from: number }
  | { kind: "splitting"; person: number; faces: Set<number> };

export function PeoplePanel({ libraryId, onChanged, onSelectPerson, selectedPerson }: Props) {
  const [people, setPeople] = useState<PersonView[]>([]);
  const [queue, setQueue] = useState<AmbiguousFaceView[]>([]);
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<FacePassReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [mode, setMode] = useState<Mode>({ kind: "idle" });
  const [renaming, setRenaming] = useState<number | null>(null);
  const [draft, setDraft] = useState("");

  /**
   * Undo for the corrections, not for the clustering.
   *
   * A merge and a split are the two operations a user can get wrong, and both are
   * destructive to the grouping — a wrong merge silently swallows a person into another.
   * Each entry records enough to reverse itself using the same primitives, so undo is not
   * a second implementation that can drift from the first.
   *
   * Bounded, like the culling undo: a long session should not accumulate closures forever.
   */
  const [undo, setUndo] = useState<Array<{ label: string; run: () => Promise<void> }>>([]);
  const UNDO_LIMIT = 50;

  const refresh = useCallback(async () => {
    try {
      setPeople(await listPeople(libraryId));
      setQueue(await ambiguousFaces(libraryId, undefined, 50).catch(() => []));
    } catch (e) {
      setError(String(e));
    }
  }, [libraryId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const run = async () => {
    setBusy(true);
    setError(null);
    setReport(null);
    try {
      const r = await runFacePass(libraryId);
      setReport(r);
      await refresh();
      onChanged();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const commitName = async (personId: number) => {
    setRenaming(null);
    try {
      await namePerson(personId, draft);
      await refresh();
      onChanged();
    } catch (e) {
      setError(String(e));
    }
  };

  const doMerge = async (into: number) => {
    if (mode.kind !== "merging") return;
    const from = mode.from;
    setMode({ kind: "idle" });

    // What the merge will move, captured before it happens so undo can put it back.
    const moving = await facesOf(from).catch(() => [] as number[]);

    try {
      const moved = await mergePeople(from, into);
      setNote(`Moved ${moved} face${moved === 1 ? "" : "s"} into the other group.`);
      pushUndo(`merge into group ${into}`, async () => {
        // The reverse of a merge is a split: put the faces back into a group of their own.
        const created = await splitPerson(into, moving);
        if (created === null) {
          setNote("Those faces are all that group has, so the merge cannot be undone.");
        }
      });
      await refresh();
      onChanged();
    } catch (e) {
      setError(String(e));
    }
  };

  const doSplit = async () => {
    if (mode.kind !== "splitting" || mode.faces.size === 0) return;
    const { person, faces } = mode;
    setMode({ kind: "idle" });
    try {
      const created = await splitPerson(person, [...faces]);
      if (created === null) {
        setNote("A group cannot be split into nothing, so nothing was changed.");
      } else {
        setNote(`Moved ${faces.size} face${faces.size === 1 ? "" : "s"} into a new group.`);
        pushUndo(`split ${faces.size} from group ${person}`, async () => {
          // The reverse of a split is a merge.
          await mergePeople(created, person);
        });
      }
      await refresh();
      onChanged();
    } catch (e) {
      setError(String(e));
    }
  };

  /** Record an undoable correction. */
  const pushUndo = (label: string, run: () => Promise<void>) => {
    setUndo((prev) => {
      const next = [...prev, { label, run }];
      return next.length > UNDO_LIMIT ? next.slice(next.length - UNDO_LIMIT) : next;
    });
  };

  const undoLast = async () => {
    const entry = undo[undo.length - 1];
    if (!entry) return;
    setUndo((prev) => prev.slice(0, -1));
    try {
      await entry.run();
      setNote(`Undid: ${entry.label}`);
      await refresh();
      onChanged();
    } catch (e) {
      setError(String(e));
    }
  };

  /** The faces in a group, for undoing a merge. */
  const facesOf = async (personId: number): Promise<number[]> => {
    // Read from the catalog rather than the panel's state: the panel shows counts, and a
    // merge needs the actual ids.
    const { personFaces } = await import("../api");
    return personFaces(personId);
  };

  const discard = async (personId: number) => {
    try {
      await deletePerson(personId);
      setNote("Group discarded. The faces are still there and may be regrouped.");
      await refresh();
      onChanged();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <section aria-label="Suggested people" className="border-t border-zinc-800 px-2 py-2">
      <div className="mb-1 flex items-center gap-2">
        <h3 className="text-[10px] uppercase tracking-wide text-zinc-500">People</h3>
        <button
          type="button"
          onClick={() => void run()}
          disabled={busy}
          className="ml-auto min-h-6 rounded bg-zinc-800 px-2 py-0.5 text-[11px] hover:bg-zinc-700 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400 disabled:opacity-50"
          title="Find faces and group the ones that look alike"
        >
          {busy ? "Looking…" : people.length > 0 ? "Find again" : "Find people"}
        </button>
      </div>

      {error && (
        <p role="alert" className="mb-1 text-[11px] text-rose-400">
          {error}
        </p>
      )}
      {note && (
        <p role="status" className="mb-1 text-[11px] text-zinc-300">
          {note}
        </p>
      )}

      {report && (
        <p className="mb-1 text-[10px] leading-tight text-zinc-400">
          {report.faces_found.toLocaleString()} faces in{" "}
          {report.detected_files.toLocaleString()} files · {report.people} group
          {report.people === 1 ? "" : "s"}
          {report.unreadable > 0 && ` · ${report.unreadable} unreadable`}
          {" · "}
          <span title={`Model licence: ${report.licence}`} className="text-zinc-500">
            {report.licence}
          </span>
        </p>
      )}

      {undo.length > 0 && (
        <button
          type="button"
          onClick={() => void undoLast()}
          className="mb-1 min-h-6 w-full rounded bg-zinc-800 px-2 py-0.5 text-[10px] text-zinc-300 hover:bg-zinc-700 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400"
          title={undo[undo.length - 1].label}
        >
          Undo: {undo[undo.length - 1].label}
        </button>
      )}

      {mode.kind === "merging" && (
        <p className="mb-1 rounded bg-sky-500/15 px-1.5 py-1 text-[10px] text-sky-200">
          Merging. Click the group to merge <em>into</em>, or{" "}
          <button type="button" className="underline" onClick={() => setMode({ kind: "idle" })}>
            cancel
          </button>
          .
        </p>
      )}

      {mode.kind === "splitting" && (
        <p className="mb-1 rounded bg-sky-500/15 px-1.5 py-1 text-[10px] text-sky-200">
          {mode.faces.size} selected.{" "}
          <button type="button" className="underline" onClick={() => void doSplit()}>
            Split into a new group
          </button>{" "}
          or{" "}
          <button type="button" className="underline" onClick={() => setMode({ kind: "idle" })}>
            cancel
          </button>
        </p>
      )}

      {people.length === 0 && !busy && !report && (
        <p className="text-[11px] leading-tight text-zinc-500">
          Not run yet. Faces are found and grouped on this machine; nothing is uploaded.
        </p>
      )}

      <ul className="space-y-0.5">
        {people.map((p) => (
          <li key={p.id}>
            {renaming === p.id ? (
              <input
                autoFocus
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onBlur={() => void commitName(p.id)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void commitName(p.id);
                  if (e.key === "Escape") setRenaming(null);
                }}
                aria-label={`Name for group ${p.id}`}
                placeholder="name…"
                className="w-full min-h-6 rounded bg-zinc-900 px-1.5 text-[11px] text-zinc-100 ring-1 ring-inset ring-sky-500/50 focus-visible:outline-none"
              />
            ) : (
              <div className="group flex items-center gap-1">
                <button
                  type="button"
                  onClick={() =>
                    mode.kind === "merging" ? void doMerge(p.id) : onSelectPerson(selectedPerson === p.id ? null : p.id)
                  }
                  aria-pressed={selectedPerson === p.id}
                  className={[
                    "flex min-h-6 min-w-0 flex-1 items-center gap-2 rounded px-1.5 text-left text-[11px]",
                    selectedPerson === p.id
                      ? "bg-sky-500/20 text-sky-100"
                      : "text-zinc-300 hover:bg-zinc-800",
                    "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400",
                  ].join(" ")}
                  title={`${p.faces} faces across ${p.photos} photographs${p.confirmed ? " — confirmed" : " — a suggestion"}`}
                >
                  <span className="truncate">
                    {p.name ?? `Group ${p.id}`}
                    {p.confirmed && <span className="pl-1 text-emerald-400">✓</span>}
                  </span>
                  <span className="ml-auto shrink-0 tabular-nums text-zinc-500">
                    {p.photos.toLocaleString()}
                  </span>
                </button>

                <button
                  type="button"
                  onClick={() => {
                    setRenaming(p.id);
                    setDraft(p.name ?? "");
                  }}
                  title="Name this group"
                  className="min-h-6 rounded px-1 text-[10px] text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200 focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400"
                >
                  name
                </button>
                <button
                  type="button"
                  onClick={() => setMode({ kind: "merging", from: p.id })}
                  title="Merge this group into another"
                  className="min-h-6 rounded px-1 text-[10px] text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200 focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400"
                >
                  merge
                </button>
                <button
                  type="button"
                  onClick={() => setMode({ kind: "splitting", person: p.id, faces: new Set() })}
                  title="Split faces out of this group"
                  className="min-h-6 rounded px-1 text-[10px] text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200 focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400"
                >
                  split
                </button>
                <button
                  type="button"
                  onClick={() => void discard(p.id)}
                  title="Discard this grouping, keeping the faces"
                  className="min-h-6 rounded px-1 text-[10px] text-zinc-500 hover:bg-zinc-800 hover:text-rose-300 focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400"
                >
                  ✕
                </button>
              </div>
            )}
          </li>
        ))}
      </ul>

      {queue.length > 0 && (
        <div className="mt-2 border-t border-zinc-800 pt-2">
          <h4
            className="mb-1 text-[10px] uppercase tracking-wide text-amber-400"
            title="Faces that sit between two groups — the ones most likely to be wrong"
          >
            Unsure ({queue.length})
          </h4>
          <ul className="space-y-0.5">
            {queue.slice(0, 8).map((f) => (
              <li key={f.faceId} className="flex items-center gap-2 px-1.5 text-[10px] text-zinc-400">
                <span className="tabular-nums">
                  {f.own.toFixed(2)} vs {f.other.toFixed(2)}
                </span>
                <span className="truncate text-zinc-500">
                  {f.personId === null ? "ungrouped" : `group ${f.personId}`}
                </span>
              </li>
            ))}
          </ul>
          <p className="mt-1 text-[10px] leading-tight text-zinc-500">
            These sit between two groups. A person can tell in a second what no threshold can.
          </p>
        </div>
      )}
    </section>
  );
}
