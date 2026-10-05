import { describe, expect, it } from "vitest";
import { MAX_PANES, panesFor } from "./Compare";

describe("choosing what to compare", () => {
  const photo = (id: number) => ({ id }) as never;

  it("shows every frame when there are few enough", () => {
    expect(panesFor([photo(1), photo(2)])).toHaveLength(2);
    expect(panesFor([photo(1), photo(2), photo(3), photo(4)])).toHaveLength(4);
  });

  it("caps at four, because beyond that the panes are too small to judge focus in", () => {
    // The cap is the feature, not a limitation. Focus is the thing being judged, and a fifth
    // pane makes every pane smaller than the detail it is there to show.
    expect(MAX_PANES).toBe(4);
    expect(panesFor([1, 2, 3, 4, 5, 6].map(photo))).toHaveLength(4);
  });

  it("takes the first frames, so the selection order decides", () => {
    const panes = panesFor([1, 2, 3, 4, 5].map(photo)) as Array<{ id: number }>;
    expect(panes.map((p) => p.id)).toEqual([1, 2, 3, 4]);
  });

  it("handles an empty selection without pretending to compare", () => {
    expect(panesFor([])).toHaveLength(0);
  });

  it("handles a single frame, which is a legitimate thing to look at closely", () => {
    expect(panesFor([photo(1)])).toHaveLength(1);
  });
});
