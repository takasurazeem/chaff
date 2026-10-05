import { describe, expect, it } from "vitest";
import { formatEta } from "./ProgressBar";

describe("formatEta", () => {
  it("never prints a number it cannot justify", () => {
    // The whole point of the coarse phrasing. A countdown in seconds implies a precision
    // this cannot have, and makes a stalled run look broken.
    expect(formatEta(0)).toBe("almost done");
    expect(formatEta(-5)).toBe("almost done");
    expect(formatEta(NaN)).toBe("almost done");
    expect(formatEta(Infinity)).toBe("almost done");
  });

  it("rounds seconds to the nearest ten rather than counting down", () => {
    expect(formatEta(3)).toBe("a few seconds left");
    expect(formatEta(9)).toBe("a few seconds left");
    expect(formatEta(11)).toBe("about 10 seconds left");
    expect(formatEta(44)).toBe("about 40 seconds left");
    expect(formatEta(89)).toBe("about 90 seconds left");
  });

  it("switches to minutes past ninety seconds", () => {
    expect(formatEta(90)).toBe("about 2 min left");
    expect(formatEta(300)).toBe("about 5 min left");
    expect(formatEta(3540)).toBe("about 59 min left");
  });

  it("switches to hours past an hour", () => {
    expect(formatEta(3600)).toBe("about 1 hour left");
    expect(formatEta(3500)).toBe("about 58 min left");
    expect(formatEta(5400)).toBe("about 1h 30m left");
    expect(formatEta(7200)).toBe("about 2 hours left");
  });

  it("gives up rather than printing a number nobody believes", () => {
    expect(formatEta(60 * 60 * 25)).toBe("over a day left");
    expect(formatEta(60 * 60 * 24 * 30)).toBe("over a day left");
  });

  it("is monotonic — a longer wait never reads as shorter", () => {
    // A sanity property. If a larger input ever produced a smaller-sounding answer, the
    // estimate would visibly go backwards as the run progressed.
    const order = [
      "almost done",
      "a few seconds left",
      "about 20 seconds left",
      "about 60 seconds left",
      "about 2 min left",
      "about 30 min left",
      "about 1 hour left",
      "about 2 hours left",
      "about 20 hours left",
      "over a day left",
    ];
    const samples = [0, 5, 20, 60, 120, 1800, 3600, 7200, 72000, 90000];
    const seen = samples.map((s) => order.indexOf(formatEta(s)));
    for (let i = 1; i < seen.length; i++) {
      expect(seen[i]).toBeGreaterThanOrEqual(seen[i - 1]);
    }
  });
});
