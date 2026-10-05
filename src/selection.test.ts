import { describe, expect, it } from "vitest";
import { clickSelection, EMPTY_SELECTION, single, type SelectionState } from "./selection";

const order = [10, 20, 30, 40, 50];
const plain = { toggle: false, extend: false };
const toggle = { toggle: true, extend: false };
const extend = { toggle: false, extend: true };

const sel = (...ids: number[]): SelectionState => ({ ids: new Set(ids), anchor: ids[0] ?? null });

describe("clicking in the grid", () => {
  it("a plain click replaces the selection", () => {
    const after = clickSelection(sel(10, 20, 30), 40, plain, order);
    expect([...after.ids]).toEqual([40]);
    expect(after.anchor).toBe(40);
  });

  it("a modified click toggles one in and out", () => {
    // **What the report called "does not let user multi-select".** The logic was here and
    // correct; the indicator was invisible, so the user could not tell it had worked.
    const one = clickSelection(EMPTY_SELECTION, 10, plain, order);
    const two = clickSelection(one, 30, toggle, order);
    expect([...two.ids].sort((a, b) => a - b)).toEqual([10, 30]);

    const back = clickSelection(two, 10, toggle, order);
    expect([...back.ids]).toEqual([30]);
  });

  it("a modified click on nothing selects one and sets the anchor", () => {
    // ⌘-clicking with an empty selection must still select something, or the first click of
    // a multi-select would do nothing at all.
    const after = clickSelection(EMPTY_SELECTION, 20, toggle, order);
    expect([...after.ids]).toEqual([20]);
    expect(after.anchor).toBe(20);
  });

  it("shift extends from the anchor, in either direction", () => {
    const from = sel(20);
    expect([...clickSelection(from, 40, extend, order).ids]).toEqual([20, 30, 40]);
    expect([...clickSelection(from, 10, extend, order).ids]).toEqual([10, 20]);
  });

  it("shift ranges over the visible order, not the library order", () => {
    // Extending over the library order would select photographs the user cannot see —
    // hidden by a filter — which is a selection they cannot review or clear.
    const filtered = [10, 30, 50];
    const after = clickSelection(sel(10), 50, extend, filtered);
    expect([...after.ids]).toEqual([10, 30, 50]);
    expect(after.ids.has(20)).toBe(false);
  });

  it("shift keeps the anchor, so a second shift re-extends from the same place", () => {
    // Dragging the end of a range is what Shift-click is for. Moving the anchor on every
    // extension would make the second one extend from the first one's end.
    const first = clickSelection(sel(20), 50, extend, order);
    const second = clickSelection(first, 30, extend, order);
    expect([...second.ids]).toEqual([20, 30]);
    expect(second.anchor).toBe(20);
  });

  it("shift with no anchor behaves as a plain click", () => {
    // Shift-clicking first, with nothing selected, is a thing people do. Selecting one
    // photograph is the useful answer; extending from nowhere is not.
    const after = clickSelection(EMPTY_SELECTION, 30, extend, order);
    expect([...after.ids]).toEqual([30]);
  });

  it("shift falls back to a plain click when the anchor is no longer visible", () => {
    // A filter changed, or the library reloaded. Extending from a photograph that is not on
    // screen would produce a range the user cannot see.
    const after = clickSelection(sel(10), 40, extend, [20, 30, 40]);
    expect([...after.ids]).toEqual([40]);
  });

  it("toggling the anchor off clears it, so the next shift does not extend from nowhere", () => {
    const one = clickSelection(EMPTY_SELECTION, 20, plain, order);
    const off = clickSelection(one, 20, toggle, order);
    expect(off.ids.size).toBe(0);
    expect(off.anchor).toBeNull();
  });

  it("toggling a different photograph off keeps the anchor", () => {
    let s = clickSelection(EMPTY_SELECTION, 20, plain, order);
    s = clickSelection(s, 40, toggle, order);
    s = clickSelection(s, 40, toggle, order);
    expect(s.anchor).toBe(20);
    expect([...s.ids]).toEqual([20]);
  });

  it("selects everything in a range including both ends", () => {
    // Off-by-one here is the classic: a range that excludes the clicked photograph looks
    // like the click did nothing.
    const after = clickSelection(sel(10), 50, extend, order);
    expect([...after.ids]).toEqual([10, 20, 30, 40, 50]);
  });

  it("reports a single selection only when there is exactly one", () => {
    expect(single(sel(10))).toBe(10);
    expect(single(sel(10, 20))).toBeNull();
    expect(single(EMPTY_SELECTION)).toBeNull();
  });

  it("never mutates the selection it was given", () => {
    // React state. A function that edited the Set in place would not re-render, and the
    // symptom is "the click did nothing" — the same symptom as the invisible border.
    const before = sel(10, 20);
    const snapshot = [...before.ids];
    clickSelection(before, 30, toggle, order);
    clickSelection(before, 40, plain, order);
    expect([...before.ids]).toEqual(snapshot);
  });
});
