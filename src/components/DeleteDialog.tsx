/**
 * The confirmation before anything moves.
 *
 * This is the safety-critical surface of the whole application: everything in
 * `chaff-core::trash` exists to make what happens *after* this dialog correct, and this
 * dialog exists to make sure the user knows what they are agreeing to.
 *
 * Four things it must show, each because leaving it out causes a specific bad outcome:
 *
 * 1. **Both halves of every pair**, by name. "3 photographs" hides the fact that a
 *    photograph is two files, and a user who expected the RAW to stay would be wrong.
 * 2. **The total size**, because emptying a trash is a decision people make about disk.
 * 3. **Incomplete photographs**, because "move this photograph" reads differently when it
 *    is already missing a half — the user may be looking for a file that is not there.
 * 4. **Refusals, and no way to proceed past them.** A disabled button with a reason beats
 *    an enabled button and an error afterwards.
 */
import type { DeletePlanView } from "../api";

function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

interface Props {
  plan: DeletePlanView;
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export function DeleteDialog({ plan, busy, onConfirm, onCancel }: Props) {
  const blocked = plan.refusals.length > 0;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-6"
      role="dialog"
      aria-modal="true"
      aria-labelledby="delete-title"
    >
      <div className="flex max-h-full w-full max-w-2xl flex-col rounded-lg border border-zinc-700 bg-zinc-900 shadow-2xl">
        <header className="border-b border-zinc-800 px-4 py-3">
          <h2 id="delete-title" className="text-sm font-semibold">
            {blocked ? "Cannot move these" : "Move to the trash?"}
          </h2>
          <p className="mt-1 text-xs text-zinc-400">
            {plan.photographs} photograph{plan.photographs === 1 ? "" : "s"} ·{" "}
            {plan.files} file{plan.files === 1 ? "" : "s"} · {bytes(plan.bytes)}
          </p>
        </header>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
          {blocked && (
            <ul className="mb-3 space-y-1 rounded border border-rose-900 bg-rose-950/40 p-2 text-xs text-rose-300">
              {plan.refusals.map((r) => (
                <li key={r}>{r}</li>
              ))}
            </ul>
          )}

          {plan.warnings.map((w) => (
            <p
              key={w}
              className="mb-2 rounded border border-amber-900 bg-amber-950/40 p-2 text-xs text-amber-300"
            >
              {w}
            </p>
          ))}

          {plan.incomplete > 0 && (
            <p className="mb-3 rounded border border-amber-900 bg-amber-950/40 p-2 text-xs text-amber-300">
              {plan.incomplete} of these {plan.incomplete === 1 ? "is" : "are"} already
              missing a partner file. Only what is listed below will move.
            </p>
          )}

          <ul className="space-y-2">
            {plan.candidates.map((c) => (
              <li key={c.photoId} className="rounded border border-zinc-800 bg-zinc-950/50 p-2">
                <div className="flex items-baseline justify-between gap-2">
                  <span className="truncate font-mono text-xs text-zinc-300">{c.stem}</span>
                  <span className="shrink-0 text-[10px] tabular-nums text-zinc-500">
                    {bytes(c.bytes)}
                  </span>
                </div>
                <ul className="mt-1 space-y-0.5">
                  {c.files.map((f) => (
                    <li
                      key={f.path}
                      className="truncate pl-3 font-mono text-[10px] text-zinc-500"
                      title={f.path}
                    >
                      {f.name}
                    </li>
                  ))}
                </ul>
              </li>
            ))}
          </ul>

          <p className="mt-4 text-[11px] leading-relaxed text-zinc-500">
            These files move to <code className="text-zinc-400">.cull-trash</code> inside the
            folder you opened. Nothing is deleted — you can put them back from the trash at
            any time, and emptying the trash is a separate, explicit action.
          </p>
        </div>

        <footer className="flex items-center justify-end gap-2 border-t border-zinc-800 px-4 py-3">
          <button
            type="button"
            onClick={onCancel}
            disabled={busy}
            className="rounded bg-zinc-800 px-3 py-1.5 text-xs hover:bg-zinc-700 disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={onConfirm}
            disabled={busy || blocked || plan.files === 0}
            className="rounded bg-rose-700 px-3 py-1.5 text-xs font-medium hover:bg-rose-600 disabled:opacity-40"
            title={blocked ? "This selection cannot be moved" : undefined}
          >
            {busy ? "Moving…" : "Move to trash"}
          </button>
        </footer>
      </div>
    </div>
  );
}
