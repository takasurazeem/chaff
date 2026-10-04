/**
 * Chaff — the culling window.
 *
 * Owns the library, the photograph list and the selection. The grid owns layout and
 * virtualisation; the tiles own their own thumbnails.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { open as openFolderDialog } from "@tauri-apps/plugin-dialog";
import { listPhotos, openLibrary } from "./api";
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

  // Cursor for keyboard navigation: the photograph the arrow keys move from.
  const cursor = useRef<number>(0);
  const columns = useRef<number>(1);

  const chooseFolder = useCallback(async () => {
    const picked = await openFolderDialog({ directory: true, multiple: false });
    if (typeof picked !== "string") return;

    setStatus({ kind: "indexing", root: picked });
    try {
      const view = await openLibrary(picked);
      const list = await listPhotos(view.library_id);
      setLibrary(view);
      setPhotos(list);
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

  // Arrow-key navigation. Kept here rather than in the grid because it moves the
  // selection, and the selection is the app's.
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (photos.length === 0) return;
      const cols = Math.max(1, columns.current);
      let next = cursor.current;

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
  }, [photos]);

  return (
    <div className="flex h-screen flex-col bg-zinc-950 text-zinc-200">
      <header className="flex shrink-0 items-center gap-4 border-b border-zinc-800 px-4 py-2">
        <span className="text-sm font-semibold tracking-tight">Chaff</span>

        <button
          type="button"
          onClick={chooseFolder}
          disabled={status.kind === "indexing"}
          className="rounded bg-zinc-800 px-3 py-1 text-sm hover:bg-zinc-700 disabled:opacity-50"
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

        {selected.size > 0 && (
          <span className="ml-auto text-xs text-zinc-400">{selected.size} selected</span>
        )}
      </header>

      <main className="min-h-0 flex-1">
        {status.kind === "idle" && (
          <div className="flex h-full flex-col items-center justify-center gap-3 text-center">
            <p className="text-lg">Open a folder of photographs to begin.</p>
            <p className="max-w-prose text-sm text-zinc-500">
              Chaff reads the folder and writes nothing into it. Its catalog and thumbnails
              live in the application data directory, and the originals are untouched until
              you explicitly remove something.
            </p>
          </div>
        )}

        {status.kind === "indexing" && (
          <div className="flex h-full flex-col items-center justify-center gap-2">
            <p className="text-sm">Indexing and scoring…</p>
            <p className="max-w-prose truncate text-xs text-zinc-500">{status.root}</p>
            <p className="text-xs text-zinc-600">
              A first pass over a large library takes a few minutes. The window stays
              responsive.
            </p>
          </div>
        )}

        {status.kind === "error" && (
          <div className="flex h-full items-center justify-center px-8">
            <p className="max-w-prose text-sm text-rose-400">{status.message}</p>
          </div>
        )}

        {status.kind === "ready" && photos.length === 0 && (
          <div className="flex h-full items-center justify-center">
            <p className="text-sm text-zinc-500">No photographs found in that folder.</p>
          </div>
        )}

        {status.kind === "ready" && photos.length > 0 && (
          <Grid
            photos={photos}
            selected={selected}
            onActivate={activate}
            onColumnsChange={(c) => {
              columns.current = c;
            }}
          />
        )}
      </main>
    </div>
  );
}
