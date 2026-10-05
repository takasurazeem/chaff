/**
 * Tags, and the pass that produces them.
 *
 * ## Every tag carries its model
 *
 * Two models disagree, and a re-tag by a second one keeps the first's tags rather than
 * replacing them. Showing which model claimed a tag is what makes that honest rather than
 * confusing — without it, a photograph with both "beach" and "shore" looks like a bug.
 *
 * ## The self-test answers the useful question
 *
 * "Is the server up?" is the least useful thing to know. The failures that happen are a
 * server up with a *text* model loaded, or one whose model ignores the schema. Each has a
 * different fix, and a port check sends you looking in the wrong place.
 */
import { useCallback, useEffect, useState } from "react";
import { diagnoseEndpoint, listTags, runTagPass, type EndpointReport, type TagOutcome } from "../api";

interface Props {
  libraryId: number;
  onChanged: () => void;
  onSelectTag: (tag: string | null) => void;
  selectedTag: string | null;
}

export function TagPanel({ libraryId, onChanged, onSelectTag, selectedTag }: Props) {
  const [tags, setTags] = useState<Array<[string, number]>>([]);
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<TagOutcome | null>(null);
  const [diagnosis, setDiagnosis] = useState<EndpointReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setTags(await listTags(libraryId));
    } catch (e) {
      setError(String(e));
    }
  }, [libraryId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const test = async () => {
    setBusy(true);
    setError(null);
    try {
      setDiagnosis(await diagnoseEndpoint());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const run = async () => {
    setBusy(true);
    setError(null);
    setOutcome(null);
    try {
      const r = await runTagPass(libraryId, 200);
      setOutcome(r);
      await refresh();
      onChanged();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section aria-label="Tags" className="border-t border-zinc-800 px-2 py-2">
      <div className="mb-1 flex items-center gap-1">
        <h3 className="text-[10px] uppercase tracking-wide text-zinc-500">Tags</h3>
        <button
          type="button"
          onClick={() => void test()}
          disabled={busy}
          className="ml-auto min-h-6 rounded px-1.5 py-0.5 text-[10px] text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200 focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400 disabled:opacity-50"
          title="Check the vision endpoint and report what actually works"
        >
          test
        </button>
        <button
          type="button"
          onClick={() => void run()}
          disabled={busy}
          className="min-h-6 rounded bg-zinc-800 px-2 py-0.5 text-[11px] hover:bg-zinc-700 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400 disabled:opacity-50"
          title="Tag 200 photographs with the configured vision model"
        >
          {busy ? "Working…" : tags.length > 0 ? "Tag more" : "Tag"}
        </button>
      </div>

      {error && (
        <p role="alert" className="mb-1 text-[11px] text-rose-400">
          {error}
        </p>
      )}

      {diagnosis && (
        <p
          className={[
            "mb-1 rounded px-1.5 py-1 text-[10px] leading-tight",
            diagnosis.vision_works && diagnosis.schema_enforced
              ? "bg-emerald-500/10 text-emerald-300"
              : "bg-amber-500/10 text-amber-300",
          ].join(" ")}
        >
          {diagnosis.verdict}
        </p>
      )}

      {outcome && (
        <p className="mb-1 text-[10px] leading-tight text-zinc-400">
          {/* **Which tagger ran, named.** "Tagged 200 photographs" with no model named is a
              claim the user cannot check — and with two very different taggers behind one
              button, it is the first thing they need to know. */}
          <span
            className={outcome.kind === "remote" ? "text-emerald-400" : "text-sky-400"}
            title={
              outcome.kind === "remote"
                ? `Tagged by ${outcome.model} over HTTP`
                : `Tagged by ${outcome.model} on this machine, against ${outcome.vocabulary} phrases. No server needed; no description, because CLIP cannot write one.`
            }
          >
            {outcome.kind === "remote" ? "vision model" : "CLIP, on CPU"}
          </span>{" "}
          · {outcome.report.tagged.toLocaleString()} tagged ·{" "}
          {outcome.report.tags.toLocaleString()} tags
          {outcome.kind === "remote" && ` · ${outcome.report.completion_tokens.toLocaleString()} tokens`}
          {outcome.kind === "remote" && outcome.report.failed > 0 && ` · ${outcome.report.failed} failed`}
          {outcome.kind === "remote" && outcome.report.remaining > 0 &&
            ` · ${outcome.report.remaining.toLocaleString()} to go`}
          {outcome.kind === "remote" && outcome.report.stopped_because && (
            // "Stopped" and "finished" are different, and a library that is a third tagged
            // must not read as complete.
            <span className="text-amber-400"> · stopped: {outcome.report.stopped_because}</span>
          )}
        </p>
      )}

      {tags.length === 0 && !busy && (
        <p className="text-[11px] leading-tight text-zinc-500">
          No tags yet. With a vision endpoint configured, tagging sends a downscaled copy to
          it; without one it runs CLIP on this machine. Either way the original and its GPS
          never leave.
        </p>
      )}

      <ul className="max-h-64 space-y-0.5 overflow-y-auto">
        {tags.slice(0, 100).map(([name, count]) => (
          <li key={name}>
            <button
              type="button"
              onClick={() => onSelectTag(selectedTag === name ? null : name)}
              aria-pressed={selectedTag === name}
              className={[
                "flex w-full min-h-6 items-center gap-2 rounded px-1.5 text-left text-[11px]",
                selectedTag === name
                  ? "bg-sky-500/20 text-sky-100"
                  : "text-zinc-300 hover:bg-zinc-800",
                "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400",
              ].join(" ")}
            >
              <span className="truncate">{name}</span>
              <span className="ml-auto shrink-0 tabular-nums text-zinc-500">
                {count.toLocaleString()}
              </span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
