import { describe, expect, it } from "vitest";
import { formatAperture, formatCaptured, formatFocal, formatShutter } from "./InfoPanel";

describe("the info panel's formatting", () => {
  it("reads a shutter speed the way a photographer does", () => {
    // `0.002 s` is technically correct and useless. Every camera and every editor writes
    // `1/500`, and a panel that does not is a panel that makes people convert in their head.
    expect(formatShutter(1 / 500)).toBe("1/500 s");
    expect(formatShutter(1 / 60)).toBe("1/60 s");
    expect(formatShutter(2)).toBe("2.0 s");
    expect(formatShutter(1)).toBe("1.0 s");
    // Absent and nonsensical are both absent, not "1/0 s".
    expect(formatShutter(null)).toBeNull();
    expect(formatShutter(0)).toBeNull();
    expect(formatShutter(-1)).toBeNull();
  });

  it("writes an aperture as f/N", () => {
    expect(formatAperture(2.8)).toBe("f/2.8");
    expect(formatAperture(8)).toBe("f/8");
    expect(formatAperture(1.4)).toBe("f/1.4");
    expect(formatAperture(null)).toBeNull();
    expect(formatAperture(0)).toBeNull();
  });

  it("rounds focal length, because nobody shoots 24.7 mm", () => {
    expect(formatFocal(24)).toBe("24 mm");
    expect(formatFocal(24.7)).toBe("25 mm");
    expect(formatFocal(null)).toBeNull();
    expect(formatFocal(0)).toBeNull();
  });

  it("labels the capture time as the camera's clock, not a real instant", () => {
    // `parse_exif_datetime` treats the camera's local wall-clock as if it were UTC.
    // Differences between photographs are correct; the absolute instant is not. A panel
    // printing "14:32" without saying which clock claims a precision the data lacks.
    const s = formatCaptured(1_700_000_000);
    expect(s).toContain("camera clock");
    expect(s).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2} \(camera clock\)$/);
    expect(formatCaptured(null)).toBeNull();
  });
});
