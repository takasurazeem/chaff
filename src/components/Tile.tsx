/**
 * One grid cell.
 *
 * Draws a thumbnail, a score, and a band indicator. Every visual state is reachable and
 * distinguishable without colour alone — the band also carries a letter, because a
 * red/green distinction is invisible to a good fraction of the people who will use this.
 */
import { memo } from "react";
import type { PhotoView } from "../types";
import { useThumbnail } from "../hooks/useThumbnails";
import { Stars } from "./Stars";

interface TileProps {
  photo: PhotoView;
  /** Pixel width of the cell; the height follows the grid's fixed row height. */
  width: number;
  height: number;
  selected: boolean;
  /** True while the grid is scrolling: thumbnails are deferred until it settles. */
  scrolling: boolean;
  /** The user's decision, which is separate from the engine's band. */
  rating: number;
  rejected: boolean;
  onActivate: (photo: PhotoView, event: React.MouseEvent) => void;
  /** Double-click opens the loupe — the same gesture every file browser uses. */
  onOpen?: (photo: PhotoView) => void;
}

const BAND_STYLE: Record<string, string> = {
  keep: "bg-emerald-500/90 text-emerald-950",
  review: "bg-zinc-500/90 text-zinc-50",
  reject: "bg-rose-500/90 text-rose-50",
};

const BAND_LABEL: Record<string, string> = {
  keep: "K",
  review: "R",
  reject: "X",
};

function TileInner({
  photo,
  width,
  height,
  selected,
  scrolling,
  rating,
  rejected,
  onActivate,
  onOpen,
}: TileProps) {
  // Deferred while scrolling. A fast scroll mounts and unmounts cells faster than a
  // thumbnail can be rendered, so fetching for them is pure waste — and it competes for
  // the same thread pool as the cells the user has actually stopped on.
  const thumb = useThumbnail(photo.id, "grid", !scrolling);

  const score = photo.composite === null ? null : Math.round(photo.composite);
  const band = photo.band ?? "review";

  return (
    <button
      type="button"
      onClick={(e) => onActivate(photo, e)}
      onDoubleClick={() => onOpen?.(photo)}
      style={{ width, height }}
      className={[
        "group relative overflow-hidden rounded-md bg-zinc-900 text-left",
        // The base edge only. Selection is drawn as an **overlay** below — see there for
        // why a ring on this element is invisible.
        "ring-1 ring-inset ring-zinc-800 transition-[box-shadow,transform] duration-75",
        "hover:ring-zinc-600 focus-visible:ring-2 focus-visible:ring-sky-400 focus:outline-none",
      ].join(" ")}
      aria-label={[
        photo.stem,
        score === null ? null : `score ${score}`,
        rating > 0 ? `${rating} stars` : null,
        rejected ? "rejected" : null,
      ]
        .filter(Boolean)
        .join(", ")}
      aria-pressed={selected}
    >
      {thumb.status === "ready" ? (
        <img
          src={thumb.url}
          alt=""
          draggable={false}
          className="h-full w-full object-cover"
          // The grid is fixed-size, so a decoding image must not reflow it.
          style={{ contentVisibility: "auto" }}
        />
      ) : (
        <div className="flex h-full w-full items-center justify-center bg-zinc-900 px-2">
          <span className="line-clamp-3 text-center text-[10px] leading-tight text-zinc-400">
            {thumb.status === "unavailable" ? photo.stem : ""}
          </span>
        </div>
      )}

      {/* **Selection is an overlay, not a ring on the button.**
          An inset box-shadow paints *below* child content, and the `<img>` is a child — so
          `ring-2 ring-sky-400` on the button was drawn and then covered by the photograph.
          Every layer above was correct: the state was set, the prop was passed, the CSS was
          generated with `.ring-2` after `.ring-1`. It was correct in every respect except
          which layer it painted on, which is why reading the code did not find it.

          The rejected state below already did it this way. One state had the pattern and the
          other did not. */}
      {selected && (
        <>
          <span className="pointer-events-none absolute inset-0 bg-sky-400/10" />
          <span className="pointer-events-none absolute inset-0 ring-2 ring-inset ring-sky-400" />
        </>
      )}

      {/* Rejected is a state of the photograph, not a tint: the cell dims and takes a
          rose edge, so it reads as "set aside" at a glance without hiding the image. */}
      {rejected && (
        <>
          <span className="pointer-events-none absolute inset-0 bg-rose-950/45" />
          <span className="pointer-events-none absolute inset-0 ring-2 ring-inset ring-rose-500/80" />
        </>
      )}

      {rating > 0 && (
        <span className="absolute left-1 top-1 rounded bg-black/65 px-1 py-px">
          <Stars rating={rating} />
        </span>
      )}

      {photo.needs_review && (
        <span
          className="absolute right-1 top-1 rounded bg-amber-400/90 px-1 text-[10px] font-semibold text-amber-950"
          title="Chaff is unsure about this photograph and would like you to look"
        >
          !
        </span>
      )}

      {score !== null && (
        <span className="absolute bottom-1 left-1 flex items-center gap-1">
          <span
            className={`rounded px-1 text-[10px] font-semibold tabular-nums ${BAND_STYLE[band]}`}
            title={`${band} — ${score}/100`}
          >
            {BAND_LABEL[band]} {score}
          </span>
        </span>
      )}
    </button>
  );
}

export const Tile = memo(TileInner);
