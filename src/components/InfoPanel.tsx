/**
 * What is known about the selected photograph.
 *
 * ## Every field is optional, and says so
 *
 * A JPEG with its metadata stripped is a normal thing, and a panel that renders an empty
 * row for it is a panel that looks broken. Absent fields are omitted rather than shown
 * blank, and a photograph with nothing known says so once instead of five times.
 *
 * ## The score explains itself
 *
 * The composite alone is a number nobody can act on. The per-term percentiles underneath
 * are the answer to "why 62?", and without them the user's only recourse is to trust it or
 * ignore it — which is how a scoring feature ends up switched off.
 */
import { useEffect, useState } from "react";
import { photoDetail, type PhotoDetail } from "../api";
import { Stars } from "./Stars";

interface Props {
  photoId: number | null;
}

function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** `1/500 s`, which is how a photographer reads a shutter speed, not `0.002 s`. */
export function formatShutter(seconds: number | null): string | null {
  if (seconds === null || seconds <= 0) return null;
  if (seconds >= 1) return `${seconds.toFixed(1)} s`;
  return `1/${Math.round(1 / seconds)} s`;
}

/** `f/2.8`, not `2.8`. */
export function formatAperture(f: number | null): string | null {
  return f === null || f <= 0 ? null : `f/${f % 1 === 0 ? f.toFixed(0) : f.toFixed(1)}`;
}

export function formatFocal(mm: number | null): string | null {
  return mm === null || mm <= 0 ? null : `${Math.round(mm)} mm`;
}

/**
 * The capture time, as the camera recorded it.
 *
 * Shown in UTC deliberately and labelled as such. `parse_exif_datetime` treats the camera's
 * local wall-clock as if it were UTC — differences between photographs are correct, the
 * absolute instant is not — and a panel that printed "14:32" without saying which clock
 * would be claiming a precision the data does not have.
 */
export function formatCaptured(epoch: number | null): string | null {
  if (epoch === null) return null;
  const d = new Date(epoch * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(
    d.getUTCHours(),
  )}:${pad(d.getUTCMinutes())} (camera clock)`;
}

const BAND_LABEL: Record<string, string> = {
  keep: "Keep",
  review: "Review",
  reject: "Reject",
};

function Row({ label, value }: { label: string; value: string | null }) {
  if (value === null) return null;
  return (
    <div className="flex gap-2 py-0.5 text-[11px]">
      <span className="w-16 shrink-0 text-zinc-500">{label}</span>
      <span className="min-w-0 flex-1 break-words text-zinc-300">{value}</span>
    </div>
  );
}

export function InfoPanel({ photoId }: Props) {
  const [detail, setDetail] = useState<PhotoDetail | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (photoId === null) {
      setDetail(null);
      return;
    }
    let cancelled = false;
    setError(null);
    void photoDetail(photoId)
      .then((d) => {
        if (!cancelled) setDetail(d);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [photoId]);

  if (photoId === null) {
    return (
      <aside
        aria-label="Photograph information"
        className="flex w-64 shrink-0 flex-col border-l border-zinc-800 bg-zinc-950 px-3 py-2"
      >
        <p className="text-[11px] text-zinc-500">Select a photograph to see its details.</p>
      </aside>
    );
  }

  if (error) {
    return (
      <aside
        aria-label="Photograph information"
        className="flex w-64 shrink-0 flex-col border-l border-zinc-800 bg-zinc-950 px-3 py-2"
      >
        <p role="alert" className="text-[11px] text-rose-400">
          {error}
        </p>
      </aside>
    );
  }

  if (!detail) {
    return (
      <aside
        aria-label="Photograph information"
        className="flex w-64 shrink-0 flex-col border-l border-zinc-800 bg-zinc-950 px-3 py-2"
      >
        <p className="text-[11px] text-zinc-500">Loading…</p>
      </aside>
    );
  }

  const exposure = [formatAperture(detail.f_number), formatShutter(detail.exposure_time)]
    .filter(Boolean)
    .join(" · ");
  const nothingKnown =
    !detail.camera && !detail.lens && !exposure && detail.iso === null && !detail.captured_at;

  return (
    <aside
      aria-label="Photograph information"
      className="flex w-64 shrink-0 flex-col gap-3 overflow-y-auto border-l border-zinc-800 bg-zinc-950 px-3 py-2"
    >
      <section>
        <h3 className="truncate font-mono text-[11px] text-zinc-200" title={detail.dir}>
          {detail.stem}
        </h3>
        <p className="mt-0.5 flex items-center gap-2 text-[11px]">
          <span className="text-zinc-500">{detail.state.replace("_", " ")}</span>
          {detail.rating > 0 && <Stars rating={detail.rating} />}
          {detail.rejected && (
            <span className="rounded bg-rose-500/90 px-1 text-[10px] font-semibold text-rose-50">
              rejected
            </span>
          )}
          {detail.needs_review && (
            <span className="rounded bg-amber-400/90 px-1 text-[10px] font-semibold text-amber-950">
              needs review
            </span>
          )}
        </p>
      </section>

      <section aria-label="Capture">
        <h4 className="mb-1 text-[10px] uppercase tracking-wide text-zinc-500">Capture</h4>
        {nothingKnown ? (
          <p className="text-[11px] text-zinc-500">
            No camera information — this file carries no EXIF, or it was stripped.
          </p>
        ) : (
          <>
            <Row label="Camera" value={detail.camera} />
            <Row label="Lens" value={detail.lens} />
            <Row label="Exposure" value={exposure || null} />
            <Row label="ISO" value={detail.iso === null ? null : String(detail.iso)} />
            <Row label="Focal" value={formatFocal(detail.focal_length)} />
            <Row label="Taken" value={formatCaptured(detail.captured_at)} />
          </>
        )}
      </section>

      <section aria-label="Score">
        <h4 className="mb-1 text-[10px] uppercase tracking-wide text-zinc-500">Score</h4>
        {detail.composite === null ? (
          <p className="text-[11px] text-zinc-500">
            Not scored — this build could not read the image data.
          </p>
        ) : (
          <>
            <p className="text-[11px] text-zinc-300">
              {BAND_LABEL[detail.band ?? "review"]} · {Math.round(detail.composite)}/100
            </p>
            <ul className="mt-1 space-y-0.5">
              {detail.terms.map(([label, value]) => (
                <li key={label} className="flex gap-2 text-[10px]">
                  <span className="w-16 shrink-0 text-zinc-500">{label}</span>
                  <span className="tabular-nums text-zinc-400">
                    {Math.round(value)}
                    <span className="text-zinc-600">th</span>
                  </span>
                </li>
              ))}
            </ul>
            <p className="mt-1 text-[10px] leading-tight text-zinc-500">
              Percentiles are within this photograph&apos;s shoot, so 80 means better than
              80% of the frames taken alongside it.
            </p>
          </>
        )}
      </section>

      <section aria-label="Files">
        <h4 className="mb-1 text-[10px] uppercase tracking-wide text-zinc-500">
          Files ({detail.files.length})
        </h4>
        <ul className="space-y-0.5">
          {detail.files.map((f) => (
            <li key={f.path} className="text-[10px]">
              <span className="truncate font-mono text-zinc-400" title={f.path}>
                {f.name}
              </span>
              <span className="pl-2 text-zinc-600">{bytes(f.size_bytes)}</span>
            </li>
          ))}
        </ul>
      </section>
    </aside>
  );
}
