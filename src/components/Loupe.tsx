/**
 * The loupe: one photograph, large, and the keys to judge it.
 *
 * This is the culling loop. The grid is for orientation — you cannot judge focus, motion
 * blur or expression from a 256-pixel cell — so the actual decisions happen here, one
 * photograph at a time, with the arrow keys moving and the rating keys deciding.
 *
 * It loads the **1024-pixel** thumbnail, not the 256 one the grid already has. Reusing the
 * grid's image would mean judging sharpness from a picture that has already been resized
 * past the point where sharpness is visible, which is the one thing this view exists to
 * show.
 *
 * The neighbours are preloaded. Arrowing through a shoot at a few hundred milliseconds per
 * photograph is the difference between culling and waiting, and the next image is knowable
 * in advance — there is no reason to make the user pay for it.
 */
import { useEffect, useState } from "react";
import { photoExplanation } from "../api";
import type { PhotoView } from "../types";
import { useThumbnail } from "../hooks/useThumbnails";
import { Stars } from "./Stars";

interface Props {
  photos: PhotoView[];
  index: number;
  decisions: Map<number, { rating: number; rejected: boolean }>;
  onNavigate: (index: number) => void;
  onClose: () => void;
  /** Routed from the app so rating keys work identically here and in the grid. */
  onRate: (photo: PhotoView, rating: number) => void;
  onReject: (photo: PhotoView) => void;
}

const BAND_LABEL: Record<string, string> = {
  keep: "Keep",
  review: "Review",
  reject: "Reject",
};

export function Loupe({
  photos,
  index,
  decisions,
  onNavigate,
  onClose,
  onRate,
  onReject,
}: Props) {
  const photo = photos[index];
  const [explanation, setExplanation] = useState<string[] | null>(null);

  const thumb = useThumbnail(photo?.id ?? -1, "loupe", true);

  // Warm the neighbours so arrowing is instant.
  useThumbnail(photos[index + 1]?.id ?? -1, "loupe", true);
  useThumbnail(photos[index - 1]?.id ?? -1, "loupe", true);

  // Depend on the **id**, not the photograph.
  //
  // The effect fetches by id, so re-running when the object identity changes but the id does
  // not would be wasted work — and `[photo]` is what the lint rule asks for. Naming the id
  // separately satisfies both: the dependency is exactly what the effect uses, and the rule
  // can see that.
  const photoId = photo?.id ?? null;
  useEffect(() => {
    if (photoId === null) return;
    let cancelled = false;
    setExplanation(null);
    void photoExplanation(photoId)
      .then((lines) => {
        if (!cancelled) setExplanation(lines);
      })
      .catch(() => {
        if (!cancelled) setExplanation(["Could not load the explanation."]);
      });
    return () => {
      cancelled = true;
    };
  }, [photoId]);

  // Capture phase, so the grid's window handler never sees these. Without it, pressing
  // `3` here would rate the photograph *and* the one behind it.
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (!photo) return;

      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.key === "ArrowRight" || e.key === " ") {
        e.preventDefault();
        e.stopPropagation();
        onNavigate(Math.min(photos.length - 1, index + 1));
        return;
      }
      if (e.key === "ArrowLeft") {
        e.preventDefault();
        e.stopPropagation();
        onNavigate(Math.max(0, index - 1));
        return;
      }
      if (e.key >= "0" && e.key <= "5") {
        e.preventDefault();
        e.stopPropagation();
        onRate(photo, Number(e.key));
        return;
      }
      if (e.key.toLowerCase() === "x") {
        e.preventDefault();
        e.stopPropagation();
        onReject(photo);
        return;
      }
      // Everything else is swallowed rather than allowed to reach the grid behind.
      e.stopPropagation();
    }
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [photo, index, photos.length, onNavigate, onClose, onRate, onReject]);

  if (!photo) return null;

  const decision = decisions.get(photo.id) ?? { rating: photo.rating, rejected: photo.rejected };
  const score = photo.composite === null ? null : Math.round(photo.composite);

  return (
    <div
      className="fixed inset-0 z-40 flex flex-col bg-zinc-950"
      role="dialog"
      aria-modal="true"
      aria-label={`${photo.stem}, photograph ${index + 1} of ${photos.length}`}
    >
      <header className="flex shrink-0 items-center gap-3 border-b border-zinc-800 px-4 py-2 text-xs">
        <span className="font-mono text-zinc-300">{photo.stem}</span>
        <span className="text-zinc-400">
          {index + 1} of {photos.length}
        </span>
        {score !== null && (
          <span className="text-zinc-400">
            {BAND_LABEL[photo.band ?? "review"]} · {score}/100
          </span>
        )}
        {decision.rating > 0 && <Stars rating={decision.rating} />}
        {decision.rejected && (
          <span className="rounded bg-rose-500/90 px-1 font-semibold text-rose-50">rejected</span>
        )}
        <button
          type="button"
          onClick={onClose}
          className="ml-auto min-h-6 rounded bg-zinc-800 px-2 py-1 hover:bg-zinc-700 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400"
        >
          Close (Esc)
        </button>
      </header>

      <div className="flex min-h-0 flex-1 items-center justify-center overflow-hidden p-4">
        {thumb.status === "ready" ? (
          <img
            src={thumb.url}
            alt={photo.stem}
            draggable={false}
            className={`max-h-full max-w-full object-contain ${decision.rejected ? "opacity-60" : ""}`}
          />
        ) : (
          <p className="text-sm text-zinc-400">
            {thumb.status === "loading" ? "Loading…" : "No preview available for this file."}
          </p>
        )}
      </div>

      <footer className="shrink-0 border-t border-zinc-800 px-4 py-2">
        {explanation && (
          <pre className="mb-2 whitespace-pre-wrap font-mono text-[11px] leading-relaxed text-zinc-400">
            {explanation.join("\n")}
          </pre>
        )}
        <p className="text-[11px] text-zinc-400">
          <kbd className="rounded bg-zinc-800 px-1">←</kbd>{" "}
          <kbd className="rounded bg-zinc-800 px-1">→</kbd> move ·{" "}
          <kbd className="rounded bg-zinc-800 px-1">0</kbd>–
          <kbd className="rounded bg-zinc-800 px-1">5</kbd> rate ·{" "}
          <kbd className="rounded bg-zinc-800 px-1">X</kbd> reject ·{" "}
          <kbd className="rounded bg-zinc-800 px-1">Esc</kbd> back to the grid
        </p>
      </footer>
    </div>
  );
}
