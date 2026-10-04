/**
 * The virtualized photo grid.
 *
 * Only the rows intersecting the viewport are mounted, plus a small overscan. A 50,000
 * photograph library therefore costs the same as a 200 photograph one: a few dozen DOM
 * nodes and a few dozen thumbnails.
 *
 * That is the whole reason this is virtualized rather than a CSS grid of everything.
 * Mounting 50,000 <img> elements does not merely scroll badly; it exhausts the webview's
 * image cache, and the cells the user is looking at are the ones that get evicted.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { PhotoView } from "../types";
import { Tile } from "./Tile";

interface GridProps {
  photos: PhotoView[];
  selected: Set<number>;
  /** The user's decisions, keyed by photograph id. Held apart from `photos` so a
   *  keystroke does not rebuild a fifty-thousand-element array. */
  decisions: Map<number, { rating: number; rejected: boolean }>;
  onActivate: (photo: PhotoView, event: React.MouseEvent) => void;
  onScrollingChange?: (scrolling: boolean) => void;
  /** Reported upward so arrow-key navigation can move by a row. */
  onColumnsChange?: (columns: number) => void;
}

/** Cell geometry. Kept in one place so the row height and the column maths cannot drift. */
const TARGET_CELL = 180;
const GAP = 8;

export function Grid({
  photos,
  selected,
  decisions,
  onActivate,
  onScrollingChange,
  onColumnsChange,
}: GridProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [scrolling, setScrolling] = useState(false);

  // Track the container width so the column count follows a resize.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) => {
      setWidth(entry.contentRect.width);
    });
    observer.observe(el);
    setWidth(el.clientWidth);
    return () => observer.disconnect();
  }, []);

  const columns = Math.max(1, Math.floor((width - GAP) / (TARGET_CELL + GAP)));
  const cellWidth = columns > 0 ? Math.floor((width - GAP * (columns + 1)) / columns) : TARGET_CELL;
  const cellHeight = Math.round(cellWidth * 0.75); // 4:3, the common camera aspect
  const rowCount = Math.ceil(photos.length / columns);

  const virtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => cellHeight + GAP,
    overscan: 3,
  });

  useEffect(() => {
    onColumnsChange?.(columns);
  }, [columns, onColumnsChange]);

  const isScrolling = virtualizer.isScrolling;
  useEffect(() => {
    setScrolling(isScrolling);
    onScrollingChange?.(isScrolling);
  }, [isScrolling, onScrollingChange]);

  const rows = virtualizer.getVirtualItems();

  const items = useMemo(() => {
    return rows.map((row) => {
      const start = row.index * columns;
      return {
        row,
        cells: photos.slice(start, start + columns),
      };
    });
  }, [rows, columns, photos]);

  const handleActivate = useCallback(
    (photo: PhotoView, event: React.MouseEvent) => onActivate(photo, event),
    [onActivate],
  );

  return (
    <div
      ref={scrollRef}
      className="h-full w-full overflow-y-auto overflow-x-hidden"
      // **A labelled group of toggle buttons, not a listbox.**
      //
      // This was `role="listbox"` with `aria-multiselectable`, which was wrong: the
      // listbox pattern requires `role="option"` children carrying `aria-selected`, and
      // these are `<button>`s carrying `aria-pressed`. Incorrect ARIA is worse than none —
      // a screen reader announces a listbox and then finds no options in it.
      //
      // Each tile genuinely is a button (activating it selects), so the native semantics
      // are already correct and the only thing missing was a name for the collection.
      //
      // Focusable so a keyboard user can reach the grid itself. Without a tabindex the
      // whole grid is unreachable by Tab.
      role="group"
      aria-label={`${photos.length} photographs`}
      tabIndex={0}
    >
      <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
        {items.map(({ row, cells }) => (
          <div
            key={row.key}
            className="absolute left-0 top-0 flex w-full"
            style={{
              height: cellHeight,
              transform: `translateY(${row.start}px)`,
              gap: GAP,
              paddingLeft: GAP,
              paddingRight: GAP,
            }}
          >
            {cells.map((photo) => {
              const decision = decisions.get(photo.id);
              return (
                <Tile
                  key={photo.id}
                  photo={photo}
                  width={cellWidth}
                  height={cellHeight}
                  selected={selected.has(photo.id)}
                  scrolling={scrolling}
                  rating={decision?.rating ?? photo.rating}
                  rejected={decision?.rejected ?? photo.rejected}
                  onActivate={handleActivate}
                />
              );
            })}
          </div>
        ))}
      </div>
    </div>
  );
}
