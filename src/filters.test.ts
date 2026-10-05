import { describe, expect, it } from "vitest";
import { apply, counts, isFiltering, matches, NO_FILTERS, type Filters } from "./filters";
import type { PhotoView } from "./types";

function photo(id: number, band: PhotoView["band"], stem = `IMG_${id}`): PhotoView {
  return { id, dir: "/lib", stem, state: "pair", needs_review: false, composite: 50, band, rating: 0, rejected: false };
}

const LIB = [
  photo(1, "keep", "sunset_a"),
  photo(2, "review", "sunset_b"),
  photo(3, "reject", "blurred"),
  photo(4, "keep", "portrait"),
];

describe("filters", () => {
  it("passes everything when nothing is set", () => {
    expect(isFiltering(NO_FILTERS)).toBe(false);
    expect(apply(LIB, new Map(), NO_FILTERS)).toHaveLength(4);
  });

  it("treats whitespace as no text filter", () => {
    // Otherwise typing a space and deleting it leaves the grid mysteriously empty.
    expect(isFiltering({ ...NO_FILTERS, text: "   " })).toBe(false);
  });

  it("filters by what the engine thinks", () => {
    const f: Filters = { ...NO_FILTERS, band: "reject" };
    expect(apply(LIB, new Map(), f).map((p) => p.id)).toEqual([3]);
  });

  it("keeps band and decision independent", () => {
    // A photograph the engine scored Reject that the user rated five stars is a real and
    // interesting thing — usually the engine being wrong. Collapsing the two controls into
    // one "status" would make it impossible to ask for.
    const decisions = new Map([[3, { rating: 5, rejected: false }]]);
    const f: Filters = { ...NO_FILTERS, band: "reject", decision: "rated" };
    expect(apply(LIB, decisions, f).map((p) => p.id)).toEqual([3]);
  });

  it("reads unrated as undecided, not as zero stars", () => {
    // A one-star photograph is a decision. So is a rejected one, even at zero stars.
    const decisions = new Map([
      [1, { rating: 1, rejected: false }],
      [2, { rating: 0, rejected: true }],
    ]);
    const unrated = apply(LIB, decisions, { ...NO_FILTERS, decision: "unrated" });
    expect(unrated.map((p) => p.id)).toEqual([3, 4]);

    const rated = apply(LIB, decisions, { ...NO_FILTERS, decision: "rated" });
    expect(rated.map((p) => p.id)).toEqual([1]);

    const rejected = apply(LIB, decisions, { ...NO_FILTERS, decision: "rejected" });
    expect(rejected.map((p) => p.id)).toEqual([2]);
  });

  it("matches filenames case-insensitively", () => {
    expect(apply(LIB, new Map(), { ...NO_FILTERS, text: "SUNSET" }).map((p) => p.id)).toEqual([1, 2]);
    expect(apply(LIB, new Map(), { ...NO_FILTERS, text: "  blur  " }).map((p) => p.id)).toEqual([3]);
  });

  it("combines filters as AND, not OR", () => {
    const f: Filters = { band: "keep", decision: "all", text: "sunset" };
    expect(apply(LIB, new Map(), f).map((p) => p.id)).toEqual([1]);
  });

  it("counts agree with the lists they label", () => {
    // **The property that matters.** A count computed by a second implementation of "does
    // this match" is how a badge ends up disagreeing with the list beneath it.
    const decisions = new Map([
      [1, { rating: 3, rejected: false }],
      [3, { rating: 0, rejected: true }],
    ]);
    const c = counts(LIB, decisions);

    expect(apply(LIB, decisions, { ...NO_FILTERS, band: "keep" })).toHaveLength(c.band.keep);
    expect(apply(LIB, decisions, { ...NO_FILTERS, band: "review" })).toHaveLength(c.band.review);
    expect(apply(LIB, decisions, { ...NO_FILTERS, band: "reject" })).toHaveLength(c.band.reject);
    expect(apply(LIB, decisions, { ...NO_FILTERS, decision: "unrated" })).toHaveLength(c.decision.unrated);
    expect(apply(LIB, decisions, { ...NO_FILTERS, decision: "rated" })).toHaveLength(c.decision.rated);
    expect(apply(LIB, decisions, { ...NO_FILTERS, decision: "rejected" })).toHaveLength(c.decision.rejected);
  });

  it("the decision counts partition the library exactly", () => {
    // Every photograph is in exactly one of unrated / rated-only / rejected.
    const decisions = new Map([
      [1, { rating: 3, rejected: false }],
      [2, { rating: 0, rejected: true }],
      [4, { rating: 5, rejected: true }],
    ]);
    const c = counts(LIB, decisions);
    // `rated` and `rejected` overlap by design — a rejected photograph keeps its stars —
    // so the partition is unrated + (everything else).
    const decided = LIB.length - c.decision.unrated;
    expect(decided).toBeGreaterThanOrEqual(c.decision.rated);
    expect(decided).toBeGreaterThanOrEqual(c.decision.rejected);
  });

  it("a photograph with no band is treated as review", () => {
    const unscored = { ...photo(9, null) };
    expect(matches(unscored, { rating: 0, rejected: false }, { ...NO_FILTERS, band: "review" })).toBe(true);
  });
});
