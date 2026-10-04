/**
 * Thumbnail fetching, with a cache and in-flight de-duplication.
 *
 * Three properties this needs, each of which is a bug if it is missing:
 *
 * 1. **A cache.** Scrolling back up must not re-invoke a command for a thumbnail already
 *    fetched. The map holds asset URLs, which are strings — a few hundred bytes each for
 *    fifty thousand photographs, against megabytes for the images themselves. The images
 *    live in the webview's own cache and on disk; this only remembers where they are.
 *
 * 2. **De-duplication.** Two cells for the same photograph — a re-render during a scroll,
 *    a stale row not yet unmounted — must not both issue a request. Without this a fast
 *    scroll issues the same command several times per cell.
 *
 * 3. **Bounded concurrency.** Fifty visible cells firing at once saturates the blocking
 *    pool and starves the cells the user is actually looking at. Requests queue behind a
 *    small window instead.
 */
import { useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { photoThumbnail } from "../api";
import type { ThumbSize } from "../types";

/** photo id + size -> asset URL, or null when the source could not be read. */
const cache = new Map<string, string | null>();
const inflight = new Map<string, Promise<string | null>>();

/** How many thumbnail commands may be in flight at once. */
const MAX_CONCURRENT = 6;
let active = 0;
const queue: Array<() => void> = [];

function acquire(): Promise<void> {
  if (active < MAX_CONCURRENT) {
    active += 1;
    return Promise.resolve();
  }
  return new Promise((resolve) => {
    queue.push(() => {
      active += 1;
      resolve();
    });
  });
}

function release(): void {
  active -= 1;
  const next = queue.shift();
  if (next) next();
}

function key(photoId: number, size: ThumbSize): string {
  return `${size}:${photoId}`;
}

async function fetchThumbnail(photoId: number, size: ThumbSize): Promise<string | null> {
  const k = key(photoId, size);
  if (cache.has(k)) return cache.get(k) ?? null;

  const existing = inflight.get(k);
  if (existing) return existing;

  const promise = (async () => {
    await acquire();
    try {
      const view = await photoThumbnail(photoId, size);
      // `convertFileSrc` turns a cache path into an asset URL. The asset protocol is
      // scoped to the thumbnail cache in tauri.conf.json, so this cannot be used to read
      // anything else on disk even if the path were tampered with.
      const url = view ? convertFileSrc(view.path) : null;
      cache.set(k, url);
      return url;
    } catch {
      // A failure to render one cell must not break the grid.
      cache.set(k, null);
      return null;
    } finally {
      release();
      inflight.delete(k);
    }
  })();

  inflight.set(k, promise);
  return promise;
}

export type ThumbState =
  | { status: "loading" }
  | { status: "ready"; url: string }
  | { status: "unavailable" };

/** Fetch one thumbnail, once it is worth fetching. */
export function useThumbnail(
  photoId: number,
  size: ThumbSize,
  enabled: boolean,
): ThumbState {
  const [state, setState] = useState<ThumbState>(() => {
    const k = key(photoId, size);
    if (cache.has(k)) {
      const url = cache.get(k);
      return url ? { status: "ready", url } : { status: "unavailable" };
    }
    return { status: "loading" };
  });

  useEffect(() => {
    // Only fetch for cells that are actually near the viewport. A virtualised grid
    // mounts a few dozen, but it also mounts them during a fast scroll and unmounts them
    // immediately, and fetching those is wasted work.
    if (!enabled) return;

    const k = key(photoId, size);
    const cached = cache.get(k);
    if (cached !== undefined) {
      setState(cached ? { status: "ready", url: cached } : { status: "unavailable" });
      return;
    }

    let cancelled = false;
    setState({ status: "loading" });
    fetchThumbnail(photoId, size).then((url) => {
      if (cancelled) return;
      setState(url ? { status: "ready", url } : { status: "unavailable" });
    });

    return () => {
      cancelled = true;
    };
  }, [photoId, size, enabled]);

  return state;
}

/** Test and diagnostic hook: how many entries the cache is holding. */
export function thumbnailCacheSize(): number {
  return cache.size;
}
