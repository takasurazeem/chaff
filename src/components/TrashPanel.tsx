/**
 * The trash: what is in it, and how to get it back.
 *
 * Restore comes first in reading order and Empty is set apart, because the two are not
 * symmetric. Restoring is reversible; emptying is not, and a panel that presents them as
 * two buttons of equal weight invites the wrong one.
 *
 * The manifest record survives a purge, so an emptied operation stays listed and marked.
 * "What did I delete, and when" has to remain answerable after the bytes are gone.
 */
import { useCallback, useEffect, useState } from "react";
import { listTrash, purgeTrash, restoreTrash, type TrashOperationView } from "../api";

function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

interface Props {
  libraryRoot: string;
  onClose: () => void;
  /** Called after a restore or a purge, so the grid can reload. */
  onChanged: () => void;
}

export function TrashPanel({ libraryRoot, onClose, onChanged }: Props) {
  const [ops, setOps] = useState<TrashOperationView[]>([]);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [confirmingPurge, setConfirmingPurge] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setOps(await listTrash(libraryRoot));
    } catch (e) {
      setNote(String(e));
    }
  }, [libraryRoot]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const restore = async (opId: string) => {
    setBusy(true);
    setNote(null);
    try {
      const r = await restoreTrash(libraryRoot, opId);
      const parts = [`${r.restored} file${r.restored === 1 ? "" : "s"} restored`];
      if (r.alreadyPresent > 0) parts.push(`${r.alreadyPresent} were already back`);
      if (r.blocked.length > 0) {
        parts.push(`${r.blocked.length} could not be restored because something is there now`);
      }
      setNote(parts.join("; "));
      await refresh();
      onChanged();
    } catch (e) {
      setNote(String(e));
    } finally {
      setBusy(false);
    }
  };

  const purge = async (opId: string) => {
    setBusy(true);
    setNote(null);
    try {
      const r = await purgeTrash(libraryRoot, [opId]);
      setNote(`Permanently removed ${r.removed} file${r.removed === 1 ? "" : "s"} (${bytes(r.bytes)})`);
      setConfirmingPurge(null);
      await refresh();
    } catch (e) {
      setNote(String(e));
    } finally {
      setBusy(false);
    }
  };

  const live = ops.filter((o) => !o.purged);
  const totalBytes = live.reduce((n, o) => n + o.bytes, 0);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-6"
      role="dialog"
      aria-modal="true"
      aria-labelledby="trash-title"
    >
      <div className="flex max-h-full w-full max-w-2xl flex-col rounded-lg border border-zinc-700 bg-zinc-900 shadow-2xl">
        <header className="flex items-baseline justify-between border-b border-zinc-800 px-4 py-3">
          <h2 id="trash-title" className="text-sm font-semibold">
            Trash
          </h2>
          <span className="text-xs text-zinc-500">
            {live.length} operation{live.length === 1 ? "" : "s"} · {bytes(totalBytes)}
          </span>
        </header>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
          {note && (
            <p className="mb-3 rounded border border-zinc-700 bg-zinc-800/60 p-2 text-xs text-zinc-300">
              {note}
            </p>
          )}

          {ops.length === 0 && (
            <p className="py-8 text-center text-sm text-zinc-500">
              Nothing has been moved to the trash.
            </p>
          )}

          <ul className="space-y-2">
            {ops.map((o) => (
              <li
                key={o.opId}
                className="flex items-center justify-between gap-3 rounded border border-zinc-800 bg-zinc-950/50 p-2"
              >
                <div className="min-w-0">
                  <div className="flex items-baseline gap-2">
                    <span className="font-mono text-xs text-zinc-300">{o.date}</span>
                    <span className="text-[10px] text-zinc-500">
                      {o.files} file{o.files === 1 ? "" : "s"} · {bytes(o.bytes)}
                    </span>
                    {o.purged && (
                      <span className="rounded bg-zinc-800 px-1 text-[10px] text-zinc-500">
                        emptied
                      </span>
                    )}
                  </div>
                  <p className="truncate text-[10px] text-zinc-600" title={o.reason}>
                    {o.reason}
                  </p>
                </div>

                {!o.purged && (
                  <div className="flex shrink-0 gap-2">
                    <button
                      type="button"
                      onClick={() => void restore(o.opId)}
                      disabled={busy}
                      className="rounded bg-zinc-800 px-2 py-1 text-xs hover:bg-zinc-700 disabled:opacity-50"
                    >
                      Put back
                    </button>
                    {confirmingPurge === o.opId ? (
                      <button
                        type="button"
                        onClick={() => void purge(o.opId)}
                        disabled={busy}
                        className="rounded bg-rose-700 px-2 py-1 text-xs hover:bg-rose-600 disabled:opacity-50"
                        title={`Permanently remove ${o.files} file(s), ${bytes(o.bytes)}`}
                      >
                        Delete forever ({bytes(o.bytes)})
                      </button>
                    ) : (
                      <button
                        type="button"
                        onClick={() => setConfirmingPurge(o.opId)}
                        disabled={busy}
                        className="rounded bg-zinc-800 px-2 py-1 text-xs text-rose-400 hover:bg-zinc-700 disabled:opacity-50"
                      >
                        Empty
                      </button>
                    )}
                  </div>
                )}
              </li>
            ))}
          </ul>
        </div>

        <footer className="flex justify-end border-t border-zinc-800 px-4 py-3">
          <button
            type="button"
            onClick={onClose}
            className="rounded bg-zinc-800 px-3 py-1.5 text-xs hover:bg-zinc-700"
          >
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}
