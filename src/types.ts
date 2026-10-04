/**
 * View types mirroring the Rust command surface.
 *
 * Hand-written rather than generated. The surface is five commands and will stay small,
 * and a generator is another build step to keep working for a handful of interfaces.
 * If it grows past a dozen, generate them.
 */

/** What `open_library` returns. */
export interface LibraryView {
  library_id: number;
  root: string;
  scanned_files: number;
  photos: number;
  pairs: number;
  needs_review: number;
  scored: number;
  /** Photographs whose image data this build cannot read — a raw format needing LibRaw. */
  unscoreable: number;
  shoots: number;
  keep: number;
  review: number;
  reject: number;
  elapsed_ms: number;
}

export type Band = "keep" | "review" | "reject";

/** One photograph as the grid sees it. No thumbnail: that is fetched per visible cell. */
export interface PhotoView {
  id: number;
  dir: string;
  stem: string;
  state: string;
  needs_review: boolean;
  composite: number | null;
  band: Band | null;
}

/** A rendered thumbnail's location inside the cache. */
export interface ThumbnailView {
  /** An absolute path inside the thumbnail cache. Converted to an asset URL in the UI. */
  path: string;
  size: string;
}

export type ThumbSize = "grid" | "loupe" | "zoom";
