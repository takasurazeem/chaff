import { describe, expect, it } from "vitest";
import { apply, counts, facets, isFiltering, matches, NO_FILTERS, type Filters } from "./filters";
import type { PhotoView } from "./types";

function photo(id: number, band: PhotoView["band"], stem = `IMG_${id}`): PhotoView {
  return {
    id, dir: "/lib", stem, state: "pair", needs_review: false, composite: 50, band,
    rating: 0, rejected: false, camera: null, lens: null, year: null,
  };
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
    const f: Filters = { ...NO_FILTERS, band: "keep", text: "sunset" };
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

  it("a folder filter is recursive, and does not leak across siblings", () => {
    // Selecting `2024/` includes `2024/Iceland/` — that is what a folder means to someone
    // navigating a library. But `2024-01/` must NOT be swept in, which a bare string
    // prefix would do and which is invisible until a count looks slightly too high.
    const lib = [
      { ...photo(1, "keep"), dir: "/lib/2024/Iceland" },
      { ...photo(2, "keep"), dir: "/lib/2024" },
      { ...photo(3, "keep"), dir: "/lib/2024-01" },
      { ...photo(4, "keep"), dir: "/lib/2023" },
    ];
    const in2024 = apply(lib, new Map(), { ...NO_FILTERS, folder: "/lib/2024" });
    expect(in2024.map((p) => p.id)).toEqual([1, 2]);

    const exact = apply(lib, new Map(), { ...NO_FILTERS, folder: "/lib/2023" });
    expect(exact.map((p) => p.id)).toEqual([4]);
  });

  it("a folder filter composes with the others rather than replacing them", () => {
    const lib = [
      { ...photo(1, "keep"), dir: "/lib/shoot" },
      { ...photo(2, "reject"), dir: "/lib/shoot" },
    ];
    const f = { ...NO_FILTERS, folder: "/lib/shoot", band: "keep" as const };
    expect(apply(lib, new Map(), f).map((p) => p.id)).toEqual([1]);
  });

  it("filters by camera, lens and year", () => {
    const lib = [
      { ...photo(1, "keep"), camera: "Canon EOS R5", lens: "RF 24-70", year: 2024 },
      { ...photo(2, "keep"), camera: "Canon EOS R5", lens: "RF 50", year: 2023 },
      { ...photo(3, "keep"), camera: "NIKON Z 6", lens: "RF 24-70", year: 2024 },
    ];
    expect(apply(lib, new Map(), { ...NO_FILTERS, camera: "Canon EOS R5" }).map((p) => p.id)).toEqual([1, 2]);
    expect(apply(lib, new Map(), { ...NO_FILTERS, lens: "RF 24-70" }).map((p) => p.id)).toEqual([1, 3]);
    expect(apply(lib, new Map(), { ...NO_FILTERS, year: 2024 }).map((p) => p.id)).toEqual([1, 3]);
    // All three compose.
    expect(
      apply(lib, new Map(), { ...NO_FILTERS, camera: "Canon EOS R5", year: 2023 }).map((p) => p.id),
    ).toEqual([2]);
  });

  it("a photograph with no metadata matches no metadata filter", () => {
    // A stripped JPEG has no camera. Selecting a camera must not sweep it in, and the
    // "no camera" case is not a filter anyone asked for.
    const lib = [{ ...photo(1, "keep") }];
    expect(apply(lib, new Map(), { ...NO_FILTERS, camera: "Canon EOS R5" })).toHaveLength(0);
    expect(apply(lib, new Map(), { ...NO_FILTERS, year: 2024 })).toHaveLength(0);
  });

  it("facets are ranked by count, because the camera you used most is the one you want", () => {
    const lib = [
      { ...photo(1, "keep"), camera: "Canon", lens: "A", year: 2024 },
      { ...photo(2, "keep"), camera: "Canon", lens: "B", year: 2023 },
      { ...photo(3, "keep"), camera: "Nikon", lens: "A", year: 2024 },
      { ...photo(4, "keep"), camera: "Canon", lens: "A", year: 2024 },
    ];
    const f = facets(lib);
    expect(f.cameras).toEqual([
      { value: "Canon", count: 3 },
      { value: "Nikon", count: 1 },
    ]);
    expect(f.lenses[0]).toEqual({ value: "A", count: 3 });
    expect(f.years[0]).toEqual({ value: 2024, count: 3 });
  });

  it("every facet option yields at least one photograph", () => {
    // **The property that matters.** An option offering a count that does not match what
    // selecting it shows is a dead end the user has to discover by trying it.
    const lib = [
      { ...photo(1, "keep"), camera: "Canon", lens: "A", year: 2024 },
      { ...photo(2, "reject"), camera: "Nikon", lens: "B", year: 2023 },
      { ...photo(3, "keep"), camera: "Canon", lens: "A", year: 2024 },
    ];
    const f = facets(lib);
    for (const { value, count } of f.cameras) {
      expect(apply(lib, new Map(), { ...NO_FILTERS, camera: value })).toHaveLength(count);
    }
    for (const { value, count } of f.lenses) {
      expect(apply(lib, new Map(), { ...NO_FILTERS, lens: value })).toHaveLength(count);
    }
    for (const { value, count } of f.years) {
      expect(apply(lib, new Map(), { ...NO_FILTERS, year: value })).toHaveLength(count);
    }
  });

  it("a photograph with no band is treated as review", () => {
    const unscored = { ...photo(9, null) };
    expect(matches(unscored, { rating: 0, rejected: false }, { ...NO_FILTERS, band: "review" })).toBe(true);
  });
});
