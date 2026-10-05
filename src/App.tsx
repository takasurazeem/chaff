/**
 * Chaff — the culling window.
 *
 * Owns the library, the photograph list and the selection. The grid owns layout and
 * virtualisation; the tiles own their own thumbnails.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { open as openFolderDialog } from "@tauri-apps/plugin-dialog";
import {
  capabilities,
  commitDelete,
  listPhotos,
  onIndexProgress,
  openLibrary,
  planDelete,
  type IndexProgress,
  setDecision,
  type DeletePlanView,
} from "./api";
import { DeleteDialog } from "./components/DeleteDialog";
import { ProgressBar } from "./components/ProgressBar";
import { Loupe } from "./components/Loupe";
import { TrashPanel } from "./components/TrashPanel";
import type { LibraryView, PhotoView } from "./types";
import { Grid } from "./components/Grid";
import "./index.css";

type Status =
  | { kind: "idle" }
  | { kind: "indexing"; root: string }
  | { kind: "ready" }
  | { kind: "error"; message: string };

export default function App() {
  const [library, setLibrary] = useState<LibraryView | null>(null);
  const [photos, setPhotos] = useState<PhotoView[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [status, setStatus] = useState<Status>({ kind: "idle" });

  /**
   * The user's decisions, held apart from `photos`.
   *
   * A keystroke must not rebuild a fifty-thousand-element array, and it must not
   * invalidate the memoised tiles either. A map keyed by id means one entry changes and
   * only the tiles reading it re-render.
   */
  const [decisions, setDecisions] = useState<Map<number, { rating: number; rejected: boolean }>>(
    new Map(),
  );

  /**
   * Session undo stack.
   *
   * Holds the decision each action *replaced*, so undo restores exactly what was there
   * rather than guessing a default. Bounded, because an unbounded stack over a long
   * culling session is a slow memory leak — and the last few hundred actions are the ones
   * anyone ever reaches for.
   */
  const undoStack = useRef<Array<{ photoId: number; previous: { rating: number; rejected: boolean } }>>(
    [],
  );
  const [undoDepth, setUndoDepth] = useState(0);
  const UNDO_LIMIT = 500;

  /**
   * The Capability Report, fetched on demand.
   *
   * Deliberately not fetched at startup: it spawns a vendor tool, and a window that
   * pauses to run `system_profiler` before showing anything is a window that feels slow
   * for a report almost nobody reads twice.
   */
  const [capabilityReport, setCapabilityReport] = useState<string | null>(null);

  /**
   * The delete flow, in two steps.
   *
   * `plan` is what the user is shown; `null` means no dialog. The commit does **not** send
   * this plan back — it sends the photograph ids and the Rust side resolves and re-hashes
   * from scratch, so nothing the webview holds can name a file the engine did not choose.
   */
  /** Live index progress, from the engine's own events. */
  const [progress, setProgress] = useState<IndexProgress | null>(null);

  const [deletePlan, setDeletePlan] = useState<DeletePlanView | null>(null);
  const [deleteBusy, setDeleteBusy] = useState(false);
  const [trashOpen, setTrashOpen] = useState(false);

  /** Index into `photos` when the loupe is open, or null when it is closed. */
  const [loupeAt, setLoupeAt] = useState<number | null>(null);

  // Cursor for keyboard navigation: the photograph the arrow keys move from.
  const cursor = useRef<number>(0);
  const columns = useRef<number>(1);

  const chooseFolder = useCallback(async () => {
    // **Inside the try, not outside it.** This call used to sit above the try/catch, so a
    // failure to open the picker — which is what an unregistered plugin causes — rejected
    // silently and the button did nothing with no explanation. A dialog that cannot open
    // must say so.
    let picked: string | string[] | null;
    try {
      picked = await openFolderDialog({ directory: true, multiple: false });
    } catch (e) {
      setStatus({
        kind: "error",
        message: `Could not open the folder picker: ${String(e)}`,
      });
      return;
    }
    if (typeof picked !== "string") return;

    setProgress(null);
    setStatus({ kind: "indexing", root: picked });
    try {
      const view = await openLibrary(picked);
      const list = await listPhotos(view.library_id);
      setLibrary(view);
      setPhotos(list);
      // Seed from what the catalog already holds, so a reopened library shows its ratings.
      setDecisions(new Map(list.map((p) => [p.id, { rating: p.rating, rejected: p.rejected }])));
      undoStack.current = [];
      setUndoDepth(0);
      setSelected(new Set());
      cursor.current = 0;
      setStatus({ kind: "ready" });
    } catch (e) {
      // The engine's errors are written to be read by a person, so they are shown as-is
      // rather than replaced with a generic message.
      setStatus({ kind: "error", message: String(e) });
    }
  }, []);

  const activate = useCallback(
    (photo: PhotoView, event: React.MouseEvent) => {
      const index = photos.findIndex((p) => p.id === photo.id);
      if (index >= 0) cursor.current = index;

      setSelected((prev) => {
        // Command or control toggles, which is what every file browser does and therefore
        // what the hands already know.
        if (event.metaKey || event.ctrlKey) {
          const next = new Set(prev);
          if (next.has(photo.id)) next.delete(photo.id);
          else next.add(photo.id);
          return next;
        }
        if (event.shiftKey && prev.size > 0) {
          const first = photos.findIndex((p) => prev.has(p.id));
          if (first >= 0) {
            const [lo, hi] = first < index ? [first, index] : [index, first];
            return new Set(photos.slice(lo, hi + 1).map((p) => p.id));
          }
        }
        return new Set([photo.id]);
      });
    },
    [photos],
  );

  /**
   * Apply a decision to the current selection.
   *
   * Optimistic: the grid updates immediately and the write follows. A culling session is
   * thousands of keystrokes, and waiting on a round trip per key makes the tool feel
   * broken on a slow disk. If the write fails the change is rolled back and the reason is
   * shown, because a rating that silently did not save is worse than one that visibly did
   * not.
   */
  const applyDecision = useCallback(
    async (mutate: (current: { rating: number; rejected: boolean }) => {
      rating: number;
      rejected: boolean;
    }) => {
      if (selected.size === 0) return;

      const targets = photos.filter((p) => selected.has(p.id));
      const before: Array<{ photoId: number; previous: { rating: number; rejected: boolean } }> = [];

      setDecisions((prev) => {
        const next = new Map(prev);
        for (const p of targets) {
          const current = next.get(p.id) ?? { rating: p.rating, rejected: p.rejected };
          before.push({ photoId: p.id, previous: current });
          next.set(p.id, mutate(current));
        }
        return next;
      });

      undoStack.current.push(...before);
      if (undoStack.current.length > UNDO_LIMIT) {
        undoStack.current.splice(0, undoStack.current.length - UNDO_LIMIT);
      }
      setUndoDepth(undoStack.current.length);

      try {
        await Promise.all(
          before.map((b) => {
            const d = mutate(b.previous);
            return setDecision(b.photoId, d.rating, d.rejected);
          }),
        );
      } catch (e) {
        // Roll back to what was there before this action.
        setDecisions((prev) => {
          const next = new Map(prev);
          for (const b of before) next.set(b.photoId, b.previous);
          return next;
        });
        undoStack.current.splice(undoStack.current.length - before.length, before.length);
        setUndoDepth(undoStack.current.length);
        setStatus({ kind: "error", message: String(e) });
      }
    },
    [photos, selected],
  );

  /** Rate one photograph, wherever the request came from. */
  const rateOne = useCallback(
    (photo: PhotoView, rating: number) => {
      setDecisions((prev) => {
        const next = new Map(prev);
        const current = next.get(photo.id) ?? { rating: photo.rating, rejected: photo.rejected };
        undoStack.current.push({ photoId: photo.id, previous: current });
        if (undoStack.current.length > UNDO_LIMIT) {
          undoStack.current.splice(0, undoStack.current.length - UNDO_LIMIT);
        }
        setUndoDepth(undoStack.current.length);
        next.set(photo.id, { ...current, rating });
        return next;
      });
      void setDecision(photo.id, rating, decisions.get(photo.id)?.rejected ?? photo.rejected).catch(
        (e) => setStatus({ kind: "error", message: String(e) }),
      );
    },
    [decisions],
  );

  const rejectOne = useCallback(
    (photo: PhotoView) => {
      const current = decisions.get(photo.id) ?? { rating: photo.rating, rejected: photo.rejected };
      const next = { ...current, rejected: !current.rejected };
      setDecisions((prev) => {
        const m = new Map(prev);
        undoStack.current.push({ photoId: photo.id, previous: current });
        if (undoStack.current.length > UNDO_LIMIT) {
          undoStack.current.splice(0, undoStack.current.length - UNDO_LIMIT);
        }
        setUndoDepth(undoStack.current.length);
        m.set(photo.id, next);
        return m;
      });
      void setDecision(photo.id, next.rating, next.rejected).catch((e) =>
        setStatus({ kind: "error", message: String(e) }),
      );
    },
    [decisions],
  );

  const undo = useCallback(async () => {
    const entry = undoStack.current.pop();
    if (!entry) return;
    setUndoDepth(undoStack.current.length);

    setDecisions((prev) => {
      const next = new Map(prev);
      next.set(entry.photoId, entry.previous);
      return next;
    });

    try {
      await setDecision(entry.photoId, entry.previous.rating, entry.previous.rejected);
    } catch (e) {
      setStatus({ kind: "error", message: String(e) });
    }
  }, []);

  useEffect(() => {
    let unsubscribe: (() => void) | undefined;
    let cancelled = false;
    void onIndexProgress((p) => setProgress(p)).then((off) => {
      // The component can unmount before the listener resolves.
      if (cancelled) off();
      else unsubscribe = off;
    });
    return () => {
      cancelled = true;
      unsubscribe?.();
    };
  }, []);

  /** Reload the library from the catalog. Used after a restore changes what is on disk. */
  const reload = useCallback(async (libraryId: number) => {
    const list = await listPhotos(libraryId);
    setPhotos(list);
    setDecisions(new Map(list.map((p) => [p.id, { rating: p.rating, rejected: p.rejected }])));
    setSelected(new Set());
  }, []);

  /** Ask what a delete would do, and show it. Moves nothing. */
  const beginDelete = useCallback(async () => {
    if (!library || selected.size === 0) return;
    try {
      setDeletePlan(await planDelete(library.root, [...selected]));
    } catch (e) {
      setStatus({ kind: "error", message: String(e) });
    }
  }, [library, selected]);

  const confirmDelete = useCallback(async () => {
    if (!library || !deletePlan) return;
    setDeleteBusy(true);
    try {
      const receipt = await commitDelete(
        library.root,
        deletePlan.candidates.map((c) => c.photoId),
        "culled in Chaff",
      );
      setDeletePlan(null);
      await reload(library.library_id);
      setStatus({ kind: "ready" });
      if (receipt.moved === 0) {
        setStatus({ kind: "error", message: "Nothing was moved." });
      }
    } catch (e) {
      setDeletePlan(null);
      setStatus({ kind: "error", message: String(e) });
    } finally {
      setDeleteBusy(false);
    }
  }, [library, deletePlan, reload]);

  // Arrow-key navigation. Kept here rather than in the grid because it moves the
  // selection, and the selection is the app's.
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (photos.length === 0) return;
      const cols = Math.max(1, columns.current);
      let next = cursor.current;

      // Enter or Space opens the loupe on whatever the cursor is on.
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        setLoupeAt(cursor.current);
        return;
      }

      // Delete opens the confirmation. It does **not** delete anything — nothing in
      // Chaff removes a file without a second, explicit step.
      if (e.key === "Delete" || e.key === "Backspace") {
        e.preventDefault();
        void beginDelete();
        return;
      }

      // Culling keys come first, so a rating key never doubles as navigation.
      if (e.key >= "0" && e.key <= "5") {
        e.preventDefault();
        const rating = Number(e.key);
        void applyDecision((c) => ({ ...c, rating }));
        return;
      }
      switch (e.key.toLowerCase()) {
        case "x":
          e.preventDefault();
          void applyDecision((c) => ({ ...c, rejected: !c.rejected }));
          return;
        case "u":
          e.preventDefault();
          void undo();
          return;
        default:
          break;
      }

      switch (e.key) {
        case "ArrowRight": next += 1; break;
        case "ArrowLeft": next -= 1; break;
        case "ArrowDown": next += cols; break;
        case "ArrowUp": next -= cols; break;
        case "Home": next = 0; break;
        case "End": next = photos.length - 1; break;
        default: return;
      }

      e.preventDefault();
      next = Math.max(0, Math.min(photos.length - 1, next));
      cursor.current = next;
      const photo = photos[next];
      setSelected(new Set([photo.id]));

      // Bring it into view. `scrollIntoView` on the tile would be simplest but the tile
      // may not be mounted, which is the nature of a virtualised grid.
      const row = Math.floor(next / cols);
      document
        .querySelector(`[data-row="${row}"]`)
        ?.scrollIntoView({ block: "nearest" });
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [photos, applyDecision, undo, beginDelete]);

  return (
    <div className="flex h-screen flex-col bg-zinc-950 text-zinc-200">
      <header className="flex shrink-0 items-center gap-4 border-b border-zinc-800 px-4 py-2">
        <span className="text-sm font-semibold tracking-tight">Chaff</span>

        <button
          type="button"
          onClick={chooseFolder}
          disabled={status.kind === "indexing"}
          className="min-h-6 rounded bg-zinc-800 px-3 py-1 text-sm hover:bg-zinc-700 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400 disabled:opacity-50"
        >
          Open folder
        </button>

        {library && status.kind === "ready" && (
          <div className="flex items-center gap-3 text-xs text-zinc-400">
            <span title={library.root} className="max-w-[28ch] truncate">
              {library.root}
            </span>
            <span>{library.photos.toLocaleString()} photographs</span>
            {library.pairs > 0 && <span>{library.pairs.toLocaleString()} pairs</span>}
            <span className="text-emerald-400">{library.keep} keep</span>
            <span>{library.review} review</span>
            <span className="text-rose-400">{library.reject} reject</span>
            {library.unscoreable > 0 && (
              <span
                className="text-amber-400"
                title="These need a raw decoder this build does not have (issue #8)"
              >
                {library.unscoreable} unreadable
              </span>
            )}
            {library.needs_review > 0 && (
              <span className="text-amber-400">{library.needs_review} to check</span>
            )}
          </div>
        )}

        {library && (
          <button
            type="button"
            onClick={() => setTrashOpen(true)}
            className="min-h-6 rounded bg-zinc-800 px-2 py-1 text-xs hover:bg-zinc-700 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400"
            title="Files moved out of the library, and how to put them back"
          >
            Trash
          </button>
        )}

        <button
          type="button"
          onClick={() => {
            if (capabilityReport !== null) {
              setCapabilityReport(null);
              return;
            }
            void capabilities().then(setCapabilityReport).catch((e) => {
              setCapabilityReport(`Could not probe this machine: ${String(e)}`);
            });
          }}
          className="ml-auto min-h-6 rounded bg-zinc-800 px-2 py-1 text-xs hover:bg-zinc-700 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400"
          title="What this machine can do, and which model tier Chaff chose"
        >
          Capabilities
        </button>

        {selected.size > 0 && (
          <span
            role="status"
            aria-live="polite"
            className="flex items-center gap-3 text-xs text-zinc-400"
          >
            <span>{selected.size} selected</span>
            <button
              type="button"
              onClick={() => void beginDelete()}
              className="min-h-6 rounded bg-zinc-800 px-2 py-0.5 text-rose-400 hover:bg-zinc-700 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400"
              title="Move the selection to the trash (Delete)"
            >
              Move to trash
            </button>
            {undoDepth > 0 && (
              <button
                type="button"
                onClick={() => void undo()}
                className="min-h-6 rounded bg-zinc-800 px-2 py-0.5 hover:bg-zinc-700 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400"
              >
                Undo ({undoDepth})
              </button>
            )}
          </span>
        )}
      </header>

      {capabilityReport !== null && (
        <section
          className="shrink-0 border-b border-zinc-800 bg-zinc-900/60 px-4 py-3"
          aria-label="Capability report"
        >
          <pre className="max-h-56 overflow-auto whitespace-pre-wrap font-mono text-[11px] leading-relaxed text-zinc-400">
            {capabilityReport}
          </pre>
        </section>
      )}

      {deletePlan && (
        <DeleteDialog
          plan={deletePlan}
          busy={deleteBusy}
          onConfirm={() => void confirmDelete()}
          onCancel={() => setDeletePlan(null)}
        />
      )}

      {loupeAt !== null && photos[loupeAt] && (
        <Loupe
          photos={photos}
          index={loupeAt}
          decisions={decisions}
          onNavigate={setLoupeAt}
          onClose={() => setLoupeAt(null)}
          onRate={rateOne}
          onReject={rejectOne}
        />
      )}

      {trashOpen && library && (
        <TrashPanel
          libraryRoot={library.root}
          onClose={() => setTrashOpen(false)}
          onChanged={() => void reload(library.library_id)}
        />
      )}

      <main className="min-h-0 flex-1">
        {status.kind === "idle" && (
          <div className="flex h-full flex-col items-center justify-center gap-3 text-center">
            <p className="text-lg">Open a folder of photographs to begin.</p>
            <p className="max-w-prose text-sm text-zinc-400">
              Chaff reads the folder and writes nothing into it. Its catalog and thumbnails
              live in the application data directory, and the originals are untouched until
              you explicitly remove something.
            </p>
            <p className="mt-2 max-w-prose text-xs text-zinc-400">
              <kbd className="rounded bg-zinc-800 px-1">0</kbd>–
              <kbd className="rounded bg-zinc-800 px-1">5</kbd> rate ·{" "}
              <kbd className="rounded bg-zinc-800 px-1">X</kbd> reject ·{" "}
              <kbd className="rounded bg-zinc-800 px-1">U</kbd> undo ·{" "}
              <kbd className="rounded bg-zinc-800 px-1">↑↓←→</kbd> move ·{" "}
              <kbd className="rounded bg-zinc-800 px-1">Enter</kbd> open ·{" "}
              <kbd className="rounded bg-zinc-800 px-1">Delete</kbd> move to trash
            </p>
          </div>
        )}

        {status.kind === "indexing" && (
          <ProgressBar progress={progress} root={status.root} />
        )}

        {status.kind === "error" && (
          <div role="alert" className="flex h-full items-center justify-center px-8">
            <p className="max-w-prose text-sm text-rose-400">{status.message}</p>
          </div>
        )}

        {status.kind === "ready" && photos.length === 0 && (
          <div className="flex h-full items-center justify-center">
            <p className="text-sm text-zinc-400">No photographs found in that folder.</p>
          </div>
        )}

        {status.kind === "ready" && photos.length > 0 && (
          <Grid
            photos={photos}
            selected={selected}
            decisions={decisions}
            onActivate={activate}
            onOpen={(photo) => {
              const i = photos.findIndex((p) => p.id === photo.id);
              if (i >= 0) setLoupeAt(i);
            }}
            onColumnsChange={(c) => {
              columns.current = c;
            }}
          />
        )}
      </main>
    </div>
  );
}
