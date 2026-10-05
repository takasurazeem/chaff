/**
 * How far a pass has got, and how to stop it.
 *
 * # Why this is shared
 *
 * The face pass and the tag pass are the two longest operations in the application — tens of
 * minutes on a real library — and they were the two with **no progress and no cancel**. The
 * native shell got both first; this is the same affordance for the web shell, and it is one
 * component so the two cannot drift apart.
 *
 * # `total` can be zero
 *
 * The scan phase genuinely does not know how many files there are until the walk finishes. A bar
 * over an unknown total is a lie, so that case shows a count with an indeterminate indicator
 * rather than dividing by zero.
 */
import type { PassProgress } from "../api";

interface Props {
  progress: PassProgress | null;
  busy: boolean;
  onCancel: () => void;
}

export function PassProgressBar({ progress, busy, onCancel }: Props) {
  if (!busy) return null;

  const known = progress !== null && progress.total > 0;
  const pct = known ? Math.round((progress.done / progress.total) * 100) : 0;

  return (
    <div className="mb-1 flex items-center gap-2" role="status" aria-live="polite">
      <div
        className="h-1 min-w-0 flex-1 overflow-hidden rounded bg-zinc-800"
        role="progressbar"
        // **`aria-valuenow` is omitted when the total is unknown**, which is what makes a screen
        // reader announce it as indeterminate rather than as 0%.
        {...(known
          ? { "aria-valuenow": pct, "aria-valuemin": 0, "aria-valuemax": 100 }
          : {})}
        aria-label={progress ? `${progress.stage} progress` : "working"}
      >
        <div
          className={
            known
              ? "h-full bg-sky-500 transition-[width] duration-200"
              : "h-full w-1/3 animate-pulse bg-sky-500"
          }
          style={known ? { width: `${pct}%` } : undefined}
        />
      </div>

      <span className="shrink-0 text-[10px] tabular-nums text-zinc-400">
        {progress ? `${progress.done}${known ? ` / ${progress.total}` : ""}` : "…"}
      </span>

      <button
        type="button"
        onClick={onCancel}
        className="min-h-6 shrink-0 rounded px-1.5 py-0.5 text-[10px] text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200 focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400"
        // **Said plainly, because stopping sounds destructive and is not.** Nothing is lost: the
        // pass commits each file as it goes and the work list is the catalog.
        title="Stop after the current photograph. Nothing already done is lost."
      >
        stop
      </button>
    </div>
  );
}
