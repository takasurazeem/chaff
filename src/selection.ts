/**
 * What a click does to the selection.
 *
 * ## Why this is a module and not a `useCallback`
 *
 * It lived inside `App.tsx` as twenty lines of set arithmetic, which is exactly the kind of
 * code that is wrong in a way nobody notices — and it was the prime suspect when selection
 * turned out to be invisible. Extracting it means the behaviour is **testable**, and the bug
 * hunt can rule it in or out in a second rather than by reading.
 *
 * ## The conventions, which are not invented here
 *
 * Plain click replaces, ⌘/Ctrl-click toggles, Shift-click extends from the anchor. Every
 * file browser does this and the hands already know it; a photo tool that invents its own is
 * one people fight.
 */

/** The state a selection carries between clicks. */
export interface SelectionState {
  /** The selected ids. */
  ids: Set<number>;
  /**
   * Where a Shift-click extends from.
   *
   * Separate from the selection because it survives a ⌘-click that removes the anchor, and
   * because "the first selected" is not the same thing after a toggle. Without it, Shift
   * after a ⌘-click extends from somewhere the user did not choose.
   */
  anchor: number | null;
}

export const EMPTY_SELECTION: SelectionState = { ids: new Set(), anchor: null };

/** What a click was modified by. */
export interface ClickModifiers {
  toggle: boolean;
  extend: boolean;
}

/**
 * The selection after a click on `id`, given the visible order.
 *
 * `order` is the ids as displayed, which is what Shift-click ranges over — extending over
 * the *library* order rather than the visible one would select photographs the user cannot
 * see.
 */
export function clickSelection(
  current: SelectionState,
  id: number,
  modifiers: ClickModifiers,
  order: number[],
): SelectionState {
  if (modifiers.toggle) {
    const ids = new Set(current.ids);
    if (ids.has(id)) {
      ids.delete(id);
      // The anchor survives unless it is the one removed: extending from a deselected
      // photograph would produce a range from nowhere.
      return { ids, anchor: current.anchor === id ? null : current.anchor };
    }
    ids.add(id);
    return { ids, anchor: current.anchor ?? id };
  }

  if (modifiers.extend && current.anchor !== null) {
    const from = order.indexOf(current.anchor);
    const to = order.indexOf(id);
    if (from >= 0 && to >= 0) {
      const [lo, hi] = from <= to ? [from, to] : [to, from];
      return { ids: new Set(order.slice(lo, hi + 1)), anchor: current.anchor };
    }
    // The anchor is not in the visible order any more — a filter changed, or the library was
    // reloaded. Falling back to a plain click is better than extending from nowhere.
  }

  return { ids: new Set([id]), anchor: id };
}

/** Is exactly one photograph selected? */
export function single(selection: SelectionState): number | null {
  return selection.ids.size === 1 ? [...selection.ids][0] : null;
}
