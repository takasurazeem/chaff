/**
 * Narrowing a library down to the photographs worth looking at.
 *
 * Two independent axes, not one "status" dropdown. **Band** is what the engine thinks;
 * **decision** is what the user decided. They are genuinely different questions, and a
 * photograph the engine scored `Reject` that the user rated five stars is a real and
 * interesting case — usually the engine being wrong. One combined control would make it
 * impossible to ask for.
 *
 * Every option carries its count, because "Reject (312)" tells you whether the filter is
 * worth applying and "Reject" alone does not.
 */
import type { Counts, Facets, Filters } from "../filters";
import { isFiltering } from "../filters";

interface Props {
  filters: Filters;
  counts: Counts;
  /** The cameras, lenses and years this library actually contains. */
  facets: Facets;
  /** How many photographs the current filters show. */
  shown: number;
  total: number;
  onChange: (f: Filters) => void;
}

function Chip({
  label,
  count,
  active,
  onClick,
  tone = "neutral",
}: {
  label: string;
  count: number;
  active: boolean;
  onClick: () => void;
  tone?: "neutral" | "keep" | "reject";
}) {
  const activeTone =
    tone === "keep"
      ? "bg-emerald-500/20 text-emerald-300 ring-emerald-500/50"
      : tone === "reject"
        ? "bg-rose-500/20 text-rose-300 ring-rose-500/50"
        : "bg-sky-500/20 text-sky-200 ring-sky-500/50";

  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      // Zero-count options are dimmed but still clickable: an empty result is a legitimate
      // thing to ask for, and a disabled control that cannot explain itself is worse.
      className={[
        "min-h-6 rounded-full px-2.5 py-0.5 text-[11px] tabular-nums ring-1 ring-inset transition-colors",
        "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400",
        active ? activeTone : "bg-zinc-900 text-zinc-400 ring-zinc-800 hover:bg-zinc-800",
        count === 0 && !active ? "opacity-50" : "",
      ].join(" ")}
    >
      {label}
      <span className="pl-1 text-zinc-500">{count.toLocaleString()}</span>
    </button>
  );
}

/**
 * A dropdown of the values a library actually contains, with counts.
 *
 * Hidden entirely when there is nothing to choose between: a control that can only offer
 * one option, or none, is furniture. A library of JPEGs with stripped metadata has no
 * cameras, and showing an empty "Camera" dropdown would suggest a bug.
 */
function Select({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string | null;
  options: Array<{ value: string; count: number }>;
  onChange: (v: string | null) => void;
}) {
  if (options.length === 0) return null;
  return (
    <select
      aria-label={`Filter by ${label.toLowerCase()}`}
      value={value ?? ""}
      onChange={(e) => onChange(e.target.value === "" ? null : e.target.value)}
      className={[
        "min-h-6 max-w-40 rounded px-1.5 py-0.5 text-[11px] ring-1 ring-inset",
        "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400",
        value
          ? "bg-sky-500/20 text-sky-200 ring-sky-500/50"
          : "bg-zinc-900 text-zinc-400 ring-zinc-800",
      ].join(" ")}
    >
      <option value="">{label}</option>
      {options.map((o) => (
        <option key={o.value} value={o.value}>
          {o.value} ({o.count})
        </option>
      ))}
    </select>
  );
}

export function FilterBar({ filters, counts, facets, shown, total, onChange }: Props) {
  const set = (patch: Partial<Filters>) => onChange({ ...filters, ...patch });
  const filtering = isFiltering(filters);

  return (
    <div className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1.5 border-b border-zinc-800 px-4 py-1.5 text-xs">
      <span className="text-zinc-400" aria-hidden="true">
        Engine
      </span>
      <div className="flex gap-1" role="group" aria-label="Filter by the engine's band">
        <Chip label="All" count={counts.band.all} active={filters.band === "all"} onClick={() => set({ band: "all" })} />
        <Chip label="Keep" count={counts.band.keep} active={filters.band === "keep"} tone="keep" onClick={() => set({ band: "keep" })} />
        <Chip label="Review" count={counts.band.review} active={filters.band === "review"} onClick={() => set({ band: "review" })} />
        <Chip label="Reject" count={counts.band.reject} active={filters.band === "reject"} tone="reject" onClick={() => set({ band: "reject" })} />
      </div>

      <span className="pl-2 text-zinc-400" aria-hidden="true">
        Yours
      </span>
      <div className="flex gap-1" role="group" aria-label="Filter by your own decision">
        <Chip label="All" count={counts.decision.all} active={filters.decision === "all"} onClick={() => set({ decision: "all" })} />
        <Chip label="Unrated" count={counts.decision.unrated} active={filters.decision === "unrated"} onClick={() => set({ decision: "unrated" })} />
        <Chip label="Rated" count={counts.decision.rated} active={filters.decision === "rated"} onClick={() => set({ decision: "rated" })} />
        <Chip label="Rejected" count={counts.decision.rejected} active={filters.decision === "rejected"} tone="reject" onClick={() => set({ decision: "rejected" })} />
      </div>

      <Select
        label="Camera"
        value={filters.camera}
        options={facets.cameras}
        onChange={(v) => set({ camera: v })}
      />
      <Select
        label="Lens"
        value={filters.lens}
        options={facets.lenses}
        onChange={(v) => set({ lens: v })}
      />
      <Select
        label="Year"
        value={filters.year === null ? null : String(filters.year)}
        options={facets.years.map((y) => ({ value: String(y.value), count: y.count }))}
        onChange={(v) => set({ year: v === null ? null : Number(v) })}
      />

      <input
        type="search"
        value={filters.text}
        onChange={(e) => set({ text: e.target.value })}
        placeholder="filename…"
        aria-label="Filter by filename"
        className="ml-2 min-h-6 w-36 rounded bg-zinc-900 px-2 py-0.5 text-[11px] text-zinc-200 ring-1 ring-inset ring-zinc-800 placeholder:text-zinc-500 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400"
      />

      <span className="ml-auto flex items-center gap-2 tabular-nums text-zinc-400">
        {filtering ? (
          <>
            <span>
              {shown.toLocaleString()} of {total.toLocaleString()}
            </span>
            <button
              type="button"
              onClick={() => onChange({ ...filters, band: "all", decision: "all", text: "" })}
              className="min-h-6 rounded bg-zinc-800 px-2 py-0.5 hover:bg-zinc-700 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400"
            >
              Clear
            </button>
          </>
        ) : (
          <span>{total.toLocaleString()} photographs</span>
        )}
      </span>
    </div>
  );
}
