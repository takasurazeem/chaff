/**
 * Index progress, with a time estimate.
 *
 * Three phases, and only the middle one is a fraction.
 *
 * The walk reports a running count because the size of a tree is not known until the walk
 * finishes — a determinate bar over an unknown total is a bar that lies, so that phase
 * shows an indeterminate stripe and the count so far. Scoring knows its total and shows a
 * real bar. Ranking is quick but happens *after* the last decode, so without its own state
 * the bar would sit at 100% and look stuck.
 *
 * ## Why the estimate uses the average rate, not the current one
 *
 * Photographs are not the same size. A run that has just decoded twenty 8 MB JPEGs is
 * moving several times faster than one working through 45 MB raws, so an instantaneous
 * rate produces a number that swings between "30 seconds" and "20 minutes" and is worse
 * than no estimate at all — the user stops believing it.
 *
 * The average since the run began is stable and self-correcting: it is briefly wrong at the
 * start and converges. Which is why there is also a warm-up — under a few seconds or a few
 * photographs, any rate is noise and the honest answer is "estimating…".
 */
import { useEffect, useRef, useState } from "react";
import type { IndexProgress } from "../api";

interface Props {
  progress: IndexProgress | null;
  root: string;
}

/** Below this many completed photographs, the rate is too noisy to show. */
const WARMUP_COUNT = 12;
/** And below this many seconds, likewise. */
const WARMUP_SECONDS = 4;

/**
 * Human phrasing for a duration.
 *
 * Deliberately coarse. "4 minutes" is useful; "3 minutes 47 seconds" implies a precision
 * this cannot have, and a countdown that ticks down in seconds makes a stalled run look
 * broken. Above an hour it switches to hours, and above a day it gives up and says so
 * rather than printing a number nobody believes.
 */
export function formatEta(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return "almost done";
  if (seconds < 10) return "a few seconds left";
  if (seconds < 90) return `about ${Math.round(seconds / 10) * 10} seconds left`;
  const minutes = seconds / 60;
  if (minutes < 60) return `about ${Math.round(minutes)} min left`;
  const hours = minutes / 60;
  if (hours < 24) {
    const whole = Math.floor(hours);
    const part = Math.round((hours - whole) * 60);
    // "about 1h 30m" reads better than "about 2 hours" when it is 1.5.
    if (part >= 10 && whole < 6) return `about ${whole}h ${part}m left`;
    const rounded = Math.round(hours);
    // Singular. "about 1 hours left" is the kind of thing that makes an interface feel
    // unfinished, and the test that caught it was checking the boundaries anyway.
    return `about ${rounded} ${rounded === 1 ? "hour" : "hours"} left`;
  }
  return "over a day left";
}

/** `mm:ss`, for the elapsed counter. */
function formatElapsed(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, "0")}`;
}

export function ProgressBar({ progress, root }: Props) {
  // When the *scoring* phase began. Reset when a new run starts, which is detectable
  // because `done` goes backwards to zero.
  const startedAt = useRef<number | null>(null);
  const lastDone = useRef(0);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (progress?.phase === "scoring") {
      if (startedAt.current === null || progress.done < lastDone.current) {
        startedAt.current = Date.now();
      }
      lastDone.current = progress.done;
    } else if (progress === null) {
      startedAt.current = null;
      lastDone.current = 0;
    }
  }, [progress]);

  // A one-second tick so the estimate counts down. Only while scoring — a timer running
  // during the idle phases is a timer waking the CPU for nothing.
  useEffect(() => {
    if (progress?.phase !== "scoring") return;
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [progress?.phase]);

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

  // The estimate, only once there is enough to base one on.
  let elapsed = 0;
  let eta: string | null = null;
  if (progress?.phase === "scoring" && startedAt.current !== null) {
    elapsed = (now - startedAt.current) / 1000;
    const warm = progress.done >= WARMUP_COUNT && elapsed >= WARMUP_SECONDS;
    if (warm && progress.done > 0 && progress.total > progress.done) {
      const perPhoto = elapsed / progress.done;
      eta = formatEta(perPhoto * (progress.total - progress.done));
    } else if (!warm) {
      eta = "estimating…";
    }
  }

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
        aria-valuetext={eta ? `${label} — ${pct ?? 0}%. ${eta}` : label}
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

      {(eta || elapsed > 0) && (
        <p className="text-xs text-zinc-400 tabular-nums">
          {elapsed > 0 && <span>{formatElapsed(elapsed)} elapsed</span>}
          {elapsed > 0 && eta && <span className="px-1.5">·</span>}
          {eta && <span>{eta}</span>}
        </p>
      )}

      <p className="max-w-prose text-center text-xs text-zinc-400">
        A first pass over a large library takes a few minutes. The window stays responsive
        and you can keep working once it finishes.
      </p>
    </div>
  );
}
