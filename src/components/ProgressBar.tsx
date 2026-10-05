/**
 * Index progress.
 *
 * Three phases, and only the middle one is a fraction.
 *
 * The walk reports a running count because the size of a tree is not known until the walk
 * finishes — a determinate bar over an unknown total is a bar that lies, so that phase
 * shows an indeterminate stripe and the count so far. Scoring knows its total and shows a
 * real bar. Ranking is quick but happens *after* the last decode, so without its own state
 * the bar would sit at 100% and look stuck.
 */
import type { IndexProgress } from "../api";

interface Props {
  progress: IndexProgress | null;
  root: string;
}

export function ProgressBar({ progress, root }: Props) {
  const pct =
    progress?.phase === "scoring" && progress.total > 0
      ? Math.round((progress.done / progress.total) * 100)
      : progress?.phase === "ranking"
        ? 100
        : null;

  const label =
    progress === null
      ? "Starting…"
      : progress.phase === "scanning"
        ? `Looking through the folder — ${progress.files.toLocaleString()} files so far`
        : progress.phase === "scoring"
          ? `Scoring ${progress.done.toLocaleString()} of ${progress.total.toLocaleString()}`
          : `Ranking ${progress.photographs.toLocaleString()} photographs`;

  return (
    <div
      role="status"
      aria-live="polite"
      className="flex h-full flex-col items-center justify-center gap-3 px-8"
    >
      <p className="max-w-[52ch] truncate text-xs text-zinc-400" title={root}>
        {root}
      </p>

      <div
        className="h-1.5 w-full max-w-md overflow-hidden rounded-full bg-zinc-800"
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={pct ?? undefined}
        aria-valuetext={label}
      >
        {pct === null ? (
          // Indeterminate: an honest "working, total unknown" rather than a fake fraction.
          <div className="h-full w-1/3 animate-pulse rounded-full bg-sky-500/70" />
        ) : (
          <div
            className="h-full rounded-full bg-sky-500 transition-[width] duration-200"
            style={{ width: `${pct}%` }}
          />
        )}
      </div>

      <p className="text-sm tabular-nums">
        {pct === null ? label : `${label} — ${pct}%`}
      </p>

      <p className="max-w-prose text-center text-xs text-zinc-400">
        A first pass over a large library takes a few minutes. The window stays responsive
        and you can keep working once it finishes.
      </p>
    </div>
  );
}
