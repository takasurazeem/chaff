/**
 * Comparing two to four frames side by side.
 *
 * ## Why this is not the loupe with more panes
 *
 * The loupe shows one photograph. Judging a burst means seeing them **at the same size, at
 * the same time** — a keeper and a near-miss differ by a little focus, a little expression,
 * a half-stop of exposure, and arrowing between them one at a time makes you remember the
 * last one instead of seeing them together.
 *
 * ## Why one shared zoom rather than four
 *
 * Four panes each with their own zoom would be four photographs to operate. The point is
 * comparison, so the zoom and pan are **one** — move it and every pane follows, and the same
 * part of each frame stays under the cursor.
 *
 * ## Why the panes are capped at four
 *
 * Two is the common case and four is the most that stays legible on a laptop. Beyond that
 * the panes are too small to judge focus in, which is the thing being judged.
 */
import { useCallback, useEffect, useState } from "react";
import { photoThumbnail } from "../api";
import type { PhotoView } from "../types";
import { Stars } from "./Stars";

/** The most panes that stay large enough to judge focus in. */
export const MAX_PANES = 4;

/**
 * The frames to show, capped.
 *
 * Extracted so the cap is testable without rendering. It is the part with a decision in it —
 * beyond four the panes are smaller than the detail they exist to show — and the rest of the
 * component is layout.
 */
export function panesFor<T>(photos: T[]): T[] {
  return photos.slice(0, MAX_PANES);
}

interface Props {
  photos: PhotoView[];
  onClose: () => void;
  onRate: (photoId: number, rating: number) => void;
  onReject: (photoId: number) => void;
}

export function Compare({ photos, onClose, onRate, onReject }: Props) {
  const panes = panesFor(photos);

  /**
   * One zoom for every pane.
   *
   * The whole reason to compare side by side is to see the same thing in each frame, so a
   * per-pane zoom would defeat it — you would be operating four photographs instead of
   * comparing one moment.
   */
  const [zoom, setZoom] = useState(1);

  // The loupe's key handling: capture phase and stopPropagation, so pressing `3` rates the
  // photograph here and not also the one behind.
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    }
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [onClose]);

  const imageFor = useCallback(async (photo: PhotoView) => {
    try {
      const t = await photoThumbnail(photo.id, "loupe");
      // `null` when the thumbnail cannot be made — a raw this build cannot decode. A pane
      // that says "loading…" forever is worse than one that admits it has nothing.
      return t?.path ?? null;
    } catch {
      return null;
    }
  }, []);

  return (
    <div
      role="dialog"
      aria-label={`Comparing ${panes.length} photographs`}
      className="fixed inset-0 z-50 flex flex-col bg-zinc-950/98"
    >
      <header className="flex items-center gap-3 border-b border-zinc-800 px-3 py-2">
        <h2 className="text-sm text-zinc-200">
          Comparing {panes.length} frame{panes.length === 1 ? "" : "s"}
        </h2>

        <label className="ml-auto flex items-center gap-2 text-[11px] text-zinc-400">
          Zoom
          <input
            type="range"
            min={1}
            max={4}
            step={0.25}
            value={zoom}
            onChange={(e) => setZoom(Number(e.target.value))}
            aria-label="Zoom, shared by every pane"
            className="w-32 accent-sky-400"
          />
          <span className="w-10 tabular-nums">{zoom.toFixed(2)}×</span>
        </label>

        <button
          type="button"
          onClick={onClose}
          className="min-h-6 rounded bg-zinc-800 px-2 text-[11px] text-zinc-300 hover:bg-zinc-700 focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400"
        >
          Close
        </button>
      </header>

      <div
        className="grid min-h-0 flex-1 gap-1 p-1"
        style={{
          gridTemplateColumns: panes.length <= 2 ? `repeat(${panes.length}, 1fr)` : "repeat(2, 1fr)",
          gridTemplateRows: panes.length <= 2 ? "1fr" : "repeat(2, 1fr)",
        }}
      >
        {panes.map((p) => (
          <Pane
            key={p.id}
            photo={p}
            zoom={zoom}
            load={imageFor}
            onRate={onRate}
            onReject={onReject}
          />
        ))}
      </div>

      {photos.length > MAX_PANES && (
        <p className="border-t border-zinc-800 px-3 py-1 text-[10px] text-amber-400">
          Showing the first {MAX_PANES} of {photos.length}. More panes than that are too small
          to judge focus in, which is the thing being judged.
        </p>
      )}
    </div>
  );
}

function Pane({
  photo,
  zoom,
  load,
  onRate,
  onReject,
}: {
  photo: PhotoView;
  zoom: number;
  load: (p: PhotoView) => Promise<string | null>;
  onRate: (photoId: number, rating: number) => void;
  onReject: (photoId: number) => void;
}) {
  const [src, setSrc] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setSrc(null);
    void load(photo).then((p) => {
      if (!cancelled) setSrc(p);
    });
    return () => {
      cancelled = true;
    };
  }, [photo, load]);

  return (
    <figure className="flex min-h-0 flex-col overflow-hidden rounded border border-zinc-800 bg-black">
      <div className="flex min-h-0 flex-1 items-center justify-center overflow-hidden">
        {src ? (
          <img
            src={src}
            alt={photo.stem}
            // `object-contain` and a transform, so every pane shows the same framing at the
            // same zoom. Cropping each pane to fill would compare different parts of the
            // frame and call it a comparison.
            className="max-h-full max-w-full object-contain transition-transform duration-75"
            style={{ transform: `scale(${zoom})` }}
          />
        ) : (
          <span className="text-[11px] text-zinc-600">loading…</span>
        )}
      </div>

      <figcaption className="flex items-center gap-2 border-t border-zinc-800 px-2 py-1">
        <span className="min-w-0 flex-1 truncate text-[11px] text-zinc-300" title={photo.stem}>
          {photo.stem}
        </span>
        {photo.composite !== null && (
          <span className="shrink-0 tabular-nums text-[10px] text-zinc-500">
            {Math.round(photo.composite)}
          </span>
        )}
        <Stars rating={photo.rating} />
        <span className="flex shrink-0 gap-0.5" role="group" aria-label={`Rate ${photo.stem}`}>
          {[1, 2, 3, 4, 5].map((r) => (
            <button
              key={r}
              type="button"
              onClick={() => onRate(photo.id, r)}
              aria-label={`${r} star${r === 1 ? "" : "s"} for ${photo.stem}`}
              className={[
                "min-h-5 w-4 rounded text-[10px] leading-none",
                "focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400",
                r <= photo.rating ? "text-amber-300" : "text-zinc-600 hover:text-zinc-400",
              ].join(" ")}
            >
              ★
            </button>
          ))}
        </span>
        <button
          type="button"
          onClick={() => onReject(photo.id)}
          title="Reject this frame"
          className="min-h-5 shrink-0 rounded px-1 text-[10px] text-zinc-500 hover:bg-zinc-800 hover:text-rose-300 focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400"
        >
          ✕
        </button>
      </figcaption>
    </figure>
  );
}
