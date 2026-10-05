/**
 * Suggested people.
 *
 * ## A group is a suggestion, and says so
 *
 * Nothing here is a name. A group is a set of faces the recogniser thinks might be one
 * person, and the wording throughout reflects that — "12 groups", not "12 people". The
 * distinction is not pedantry: treating a cluster as fact is how a stranger's face ends up
 * under someone's name.
 *
 * ## Why the count is photographs, not faces
 *
 * "38 faces" tells you how many times the detector fired. "12 photographs" tells you how
 * much of your library this person appears in, which is the question someone actually has
 * when deciding whether a group is worth naming.
 */
import { useCallback, useEffect, useState } from "react";
import { listPeople, runFacePass, type FacePassReport, type PersonView } from "../api";

interface Props {
  libraryId: number;
  /** Called after a pass, so the grid can pick up new face counts. */
  onChanged: () => void;
  /** Show only the photographs this person appears in. */
  onSelectPerson: (personId: number | null) => void;
  selectedPerson: number | null;
}

export function PeoplePanel({ libraryId, onChanged, onSelectPerson, selectedPerson }: Props) {
  const [people, setPeople] = useState<PersonView[]>([]);
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<FacePassReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setPeople(await listPeople(libraryId));
    } catch (e) {
      setError(String(e));
    }
  }, [libraryId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const run = async () => {
    setBusy(true);
    setError(null);
    setReport(null);
    try {
      const r = await runFacePass(libraryId);
      setReport(r);
      await refresh();
      onChanged();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section aria-label="Suggested people" className="border-t border-zinc-800 px-2 py-2">
      <div className="mb-1 flex items-center gap-2">
        <h3 className="text-[10px] uppercase tracking-wide text-zinc-500">People</h3>
        <button
          type="button"
          onClick={() => void run()}
          disabled={busy}
          className="ml-auto min-h-6 rounded bg-zinc-800 px-2 py-0.5 text-[11px] hover:bg-zinc-700 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400 disabled:opacity-50"
          title="Find faces and group the ones that look alike"
        >
          {busy ? "Looking…" : people.length > 0 ? "Find again" : "Find people"}
        </button>
      </div>

      {error && (
        <p role="alert" className="mb-1 text-[11px] text-rose-400">
          {error}
        </p>
      )}

      {report && (
        <p role="status" className="mb-1 text-[10px] leading-tight text-zinc-400">
          {report.faces_found.toLocaleString()} faces in{" "}
          {report.detected_files.toLocaleString()} files · {report.people} group
          {report.people === 1 ? "" : "s"}
          {report.unreadable > 0 && ` · ${report.unreadable} unreadable`}
          {" · "}
          <span title={`Model licence: ${report.licence}`} className="text-zinc-500">
            {report.licence}
          </span>
        </p>
      )}

      {people.length === 0 && !busy && !report && (
        <p className="text-[11px] leading-tight text-zinc-500">
          Not run yet. Faces are found and grouped on this machine; nothing is uploaded.
        </p>
      )}

      <ul className="space-y-0.5">
        {people.map((p) => (
          <li key={p.id}>
            <button
              type="button"
              onClick={() => onSelectPerson(selectedPerson === p.id ? null : p.id)}
              aria-pressed={selectedPerson === p.id}
              className={[
                "flex w-full min-h-6 items-center gap-2 rounded px-1.5 text-left text-[11px]",
                selectedPerson === p.id
                  ? "bg-sky-500/20 text-sky-100"
                  : "text-zinc-300 hover:bg-zinc-800",
                "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400",
              ].join(" ")}
              title={`${p.faces} faces across ${p.photos} photographs — a suggestion, not a name`}
            >
              <span className="truncate">{p.name ?? `Group ${p.id}`}</span>
              <span className="ml-auto shrink-0 tabular-nums text-zinc-500">
                {p.photos.toLocaleString()}
              </span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
