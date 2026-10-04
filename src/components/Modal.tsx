/**
 * A modal that behaves like one.
 *
 * Three things the accessibility skill names as anti-patterns, all of which the first
 * version of these dialogs did:
 *
 * 1. **Uncontained focus.** `aria-modal` tells a screen reader the background is inert,
 *    but it does nothing for the Tab key. Without a trap, tabbing walks out of the dialog
 *    and into the grid behind it, where the user cannot see where they are.
 * 2. **No escape.** WCAG 2.1.2: a keyboard user must be able to leave. `Escape` closes.
 * 3. **Focus not restored.** Closing a dialog that took focus must give it back, or the
 *    next Tab starts from the top of the document.
 *
 * It also **stops key events reaching the window**. The grid listens on `window` for arrow
 * keys and rating keys, so without this, pressing `3` inside the delete confirmation would
 * rate the photographs behind it — a real bug, not a hypothetical one, and one that would
 * be very hard to explain to the person it happened to.
 */
import { useCallback, useEffect, useRef, type ReactNode } from "react";

const FOCUSABLE =
  'a[href], button:not([disabled]), textarea, input, select, [tabindex]:not([tabindex="-1"])';

interface Props {
  labelId: string;
  onClose: () => void;
  children: ReactNode;
  /** Applied to the panel, so each dialog can set its own width. */
  panelClassName?: string;
}

export function Modal({ labelId, onClose, children, panelClassName = "" }: Props) {
  const panelRef = useRef<HTMLDivElement>(null);
  // Where focus was before the dialog opened, so it can be given back.
  const restoreTo = useRef<HTMLElement | null>(null);

  const focusables = useCallback((): HTMLElement[] => {
    const root = panelRef.current;
    if (!root) return [];
    return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (el) => el.offsetParent !== null || el === document.activeElement,
    );
  }, []);

  useEffect(() => {
    restoreTo.current = document.activeElement as HTMLElement | null;

    // Move focus into the dialog. The first focusable is usually the least destructive
    // control in these dialogs (Cancel, or a restore button), which is deliberate.
    const first = focusables()[0];
    first?.focus();

    return () => {
      restoreTo.current?.focus?.();
    };
  }, [focusables]);

  // Capture phase, so the grid's window listener never sees these.
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.key !== "Tab") {
        // Everything else is swallowed rather than allowed to reach the window. A rating
        // key pressed inside a confirmation dialog must not rate anything.
        e.stopPropagation();
        return;
      }

      const items = focusables();
      if (items.length === 0) {
        e.preventDefault();
        return;
      }
      const first = items[0];
      const last = items[items.length - 1];
      const active = document.activeElement as HTMLElement | null;

      if (e.shiftKey && (active === first || !panelRef.current?.contains(active))) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    }

    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [focusables, onClose]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-6"
      onMouseDown={(e) => {
        // Clicking the backdrop closes, which is what every desktop dialog does. Guarded on
        // the target so a drag that starts inside and ends outside does not close it.
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={labelId}
        className={`flex max-h-full w-full flex-col rounded-lg border border-zinc-700 bg-zinc-900 shadow-2xl ${panelClassName}`}
      >
        {children}
      </div>
    </div>
  );
}
