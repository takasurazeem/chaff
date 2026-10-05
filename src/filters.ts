/**
 * Filtering a library down to the photographs worth looking at.
 *
 * Pure and separate from the component so it can be tested without a DOM, and so the
 * predicate the counts use is provably the same one the grid uses. Two implementations of
 * "does this match" is how a count ends up disagreeing with the list beneath it.
 */
import type { PhotoView } from "./types";

export type BandFilter = "all" | "keep" | "review" | "reject";
export type DecisionFilter = "all" | "unrated" | "rated" | "rejected";

export interface Filters {
  /** What the *engine* thinks. */
  band: BandFilter;
  /** What the *user* has decided. */
  decision: DecisionFilter;
  /** Substring of the filename, case-insensitive. */
  text: string;
  /**
   * A folder path, or null for the whole library.
   *
   * Recursive: selecting `2024/` includes `2024/Iceland/`, because that is what a folder
   * means to someone navigating a library. The sidebar shows both counts for exactly this
   * reason.
   */
  folder: string | null;
}

export const NO_FILTERS: Filters = { band: "all", decision: "all", text: "", folder: null };

export interface Decision {
  rating: number;
  rejected: boolean;
}

export function isFiltering(f: Filters): boolean {
  return (
    f.band !== "all" || f.decision !== "all" || f.text.trim() !== "" || f.folder !== null
  );
}

/** The user's decision, falling back to what the catalog reported. */
export function decisionOf(
  photo: PhotoView,
  decisions: Map<number, Decision>,
): Decision {
  return decisions.get(photo.id) ?? { rating: photo.rating, rejected: photo.rejected };
}

/**
 * Does one photograph pass?
 *
 * The band and the decision are deliberately independent filters rather than one combined
 * "status". A photograph the engine scored `Reject` that the user rated five stars is a
 * real and interesting thing — it usually means the engine is wrong — and collapsing the
 * two into one control would make that case impossible to ask for.
 */
export function matches(photo: PhotoView, decision: Decision, f: Filters): boolean {
  // Truthy, not `!== null`. A `Filters` value built as an object literal without this
  // field has `undefined` here, and `undefined !== null` is **true** — so the check would
  // enter the branch and match nothing, silently emptying the grid. The test that caught
  // this was "combines filters as AND", which is about something else entirely.
  if (f.folder) {
    // Prefix with a separator, so `2024-01` is not counted under `2024`. A bare string
    // prefix would do exactly that, and the mistake is invisible until someone notices a
    // count that is slightly too high.
    const prefix = `${f.folder}/`;
    if (photo.dir !== f.folder && !photo.dir.startsWith(prefix)) return false;
  }

  if (f.band !== "all" && (photo.band ?? "review") !== f.band) return false;

  switch (f.decision) {
    case "unrated":
      // Unrated means *undecided*: no stars and not rejected. A one-star photograph is a
      // decision, and a rejected one certainly is.
      if (decision.rating > 0 || decision.rejected) return false;
      break;
    case "rated":
      if (decision.rating === 0) return false;
      break;
    case "rejected":
      if (!decision.rejected) return false;
      break;
    case "all":
      break;
  }

  const text = f.text.trim().toLowerCase();
  if (text !== "" && !photo.stem.toLowerCase().includes(text)) return false;

  return true;
}

export function apply(
  photos: PhotoView[],
  decisions: Map<number, Decision>,
  f: Filters,
): PhotoView[] {
  if (!isFiltering(f)) return photos;
  return photos.filter((p) => matches(p, decisionOf(p, decisions), f));
}

/** How many photographs each option would show, for the labels on the controls. */
export interface Counts {
  band: Record<BandFilter, number>;
  decision: Record<DecisionFilter, number>;
}

export function counts(photos: PhotoView[], decisions: Map<number, Decision>): Counts {
  const band: Record<BandFilter, number> = { all: photos.length, keep: 0, review: 0, reject: 0 };
  const decision: Record<DecisionFilter, number> = {
    all: photos.length,
    unrated: 0,
    rated: 0,
    rejected: 0,
  };

  for (const p of photos) {
    band[(p.band ?? "review") as BandFilter] += 1;
    const d = decisionOf(p, decisions);
    if (d.rejected) decision.rejected += 1;
    if (d.rating > 0) decision.rated += 1;
    if (d.rating === 0 && !d.rejected) decision.unrated += 1;
  }
  return { band, decision };
}
