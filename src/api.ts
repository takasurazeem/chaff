/**
 * Typed wrappers over the Tauri commands.
 *
 * Every call goes through here so that the command names and argument shapes live in one
 * place. A typo in a command name fails at runtime with a string, which is the kind of
 * thing that reaches a user; keeping them here makes it one place to check.
 */
import { invoke } from "@tauri-apps/api/core";
import type { DecisionView, LibraryView, PhotoView, ThumbnailView, ThumbSize } from "./types";

/**
 * How far along an index run is.
 *
 * Tagged by `phase`, matching the Rust enum. `scanning` carries a running count because
 * the size of a tree is not known until the walk finishes; `scoring` is determinate.
 */
export type IndexProgress =
  | { phase: "scanning"; files: number }
  | { phase: "scoring"; done: number; total: number; current: string }
  | { phase: "ranking"; photographs: number };

/**
 * Subscribe to index progress.
 *
 * Events rather than polling, because `openLibrary` is one blocking call and there is no
 * state for the frontend to read while it runs. Returns an unsubscribe function.
 */
/**
 * How far a face or tag pass has got.
 *
 * **`total` can be zero** — the scan phase of an index genuinely does not know how many files
 * there are until the walk finishes — so a caller shows an indeterminate indicator rather than
 * dividing by it.
 */
export interface PassProgress {
  /** `faces` or `tagging`, so one listener serves both. */
  stage: string;
  done: number;
  total: number;
}

export async function onPassProgress(
  handler: (p: PassProgress) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  return listen<PassProgress>("chaff://pass-progress", (e) => handler(e.payload));
}

export async function onIndexProgress(
  handler: (p: IndexProgress) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  return listen<IndexProgress>("chaff://index-progress", (e) => handler(e.payload));
}

/**
 * Index a folder and score it.
 *
 * Long-running — a first index of a large library is minutes — and the Rust side
 * dispatches it onto a blocking thread, so the window stays responsive while it runs.
 */
export async function openLibrary(path: string): Promise<LibraryView> {
  return invoke<LibraryView>("open_library", { path });
}

/** What a face pass did. */
export interface FacePassReport {
  detected_files: number;
  faces_found: number;
  embedded: number;
  unreadable: number;
  people: number;
  /** The model's licence, shown where the feature is switched on. */
  licence: string;
  elapsed_ms: number;
}

/**
 * Run the face pass: detect, embed, group.
 *
 * Long-running, and resumable — each file is committed as it is processed.
 */
/**
 * Ask the running pass to stop.
 *
 * **Nothing is lost.** Both passes commit each file as they go and the work list is the catalog
 * rather than a list in memory, so stopping leaves the catalog consistent and the next pass
 * resumes from where this one stopped.
 *
 * Returns as soon as the flag is set; the pass checks it between photographs and stops on its
 * own, which is why this is a flag and not a kill.
 */
export async function cancelPass(): Promise<void> {
  return invoke("cancel_pass");
}

export async function runFacePass(libraryId: number): Promise<FacePassReport> {
  return invoke<FacePassReport>("run_face_pass", { libraryId });
}

/** A suggested person: a group of faces that might be one individual. */
export interface PersonView {
  id: number;
  name: string | null;
  confirmed: boolean;
  faces: number;
  photos: number;
}

/** Every suggested person, most photographs first. */
export async function listPeople(libraryId: number): Promise<PersonView[]> {
  return invoke<PersonView[]>("list_people", { libraryId });
}

/**
 * Name a person. **Naming confirms the group** — a group a human has put a name to is a
 * decision, and the next clustering pass leaves it alone.
 */
export async function namePerson(personId: number, name: string | null): Promise<void> {
  return invoke<void>("name_person", { personId, name });
}

/** Merge one group into another, moving every face. Both end up confirmed. */
export async function mergePeople(fromId: number, intoId: number): Promise<number> {
  return invoke<number>("merge_people", { fromId, intoId });
}

/**
 * Move faces out of a group into a new one.
 *
 * Returns the new group's id, or `null` when the split would empty the source — a person
 * with no faces is not a group.
 */
export async function splitPerson(personId: number, faceIds: number[]): Promise<number | null> {
  return invoke<number | null>("split_person", { personId, faceIds });
}

/** Discard a grouping, keeping the faces. */
export async function deletePerson(personId: number): Promise<void> {
  return invoke<void>("delete_person", { personId });
}

/** A face the clustering could not place confidently. */
export interface AmbiguousFaceView {
  // **snake_case, because that is what Rust emits.** Tauri v2 converts command
  // *arguments* to snake_case for Rust, but return values are serialised as the struct is
  // written — and there is no `rename_all`. Declaring these camelCase made them
  // `undefined` at runtime, and `tsc` cannot see it because it only checks its own side.
  face_id: number;
  photo_id: number;
  person_id: number | null;
  /** Similarity to the group it is in, and to the nearest group it is not in. */
  own: number;
  other: number;
}

/** Faces sitting between two groups, worst margin first. */
export async function ambiguousFaces(
  libraryId: number,
  margin?: number,
  limit?: number,
): Promise<AmbiguousFaceView[]> {
  return invoke<AmbiguousFaceView[]>("ambiguous_faces", { libraryId, margin, limit });
}

/** The faces in a group, for undoing a merge. */
export async function personFaces(personId: number): Promise<number[]> {
  return invoke<number[]>("person_faces", { personId });
}

/** The photographs a person appears in. */
export async function personPhotos(personId: number): Promise<number[]> {
  return invoke<number[]>("person_photos", { personId });
}

/** How many faces were found in each photograph. */
export async function faceCounts(libraryId: number): Promise<Record<string, number>> {
  return invoke<Record<string, number>>("face_counts", { libraryId });
}

/** What a sidecar write did. */
export interface SidecarReport {
  written: number;
  skipped: number;
  failed: number;
}

/**
 * Write decisions into XMP sidecars.
 *
 * Opt-in, and only for photographs that carry a decision. Merges rather than replaces, so
 * everything Lightroom, darktable or digiKam put in those files survives.
 */
export async function writeSidecars(libraryId: number): Promise<SidecarReport> {
  return invoke<SidecarReport>("write_sidecars", { libraryId });
}

/** Whether the library watcher is running. */
export interface WatchView {
  running: boolean;
  /** Paths seen since the last re-index. */
  seen: number;
  busy: boolean;
}

/** Start watching a library for external changes. Idempotent. */
export async function startWatching(libraryRoot: string): Promise<WatchView> {
  return invoke<WatchView>("start_watching", { libraryRoot });
}

/** Stop watching. */
export async function stopWatching(): Promise<void> {
  return invoke<void>("stop_watching");
}

/** Whether the watcher is running. */
export async function watchStatus(): Promise<WatchView> {
  return invoke<WatchView>("watch_status");
}

/** One tag on a photograph. */
export interface PhotoTagView {
  name: string;
  confidence: number;
  /** Which model claimed it. Shown, because two models disagree. */
  model: string;
}

/** What a tagging pass did. */
export interface TagPassReport {
  tagged: number;
  remaining: number;
  unreadable: number;
  failed: number;
  tags: number;
  prompt_tokens: number;
  completion_tokens: number;
  elapsed_ms: number;
  /** Set when the endpoint stopped answering, so the UI can say "stopped", not "finished". */
  stopped_because: string | null;
}

/**
 * What an endpoint says about itself.
 *
 * `vision_works` and `schema_enforced` are the fields that matter: a server can be up with
 * a text-only model, or up with a vision model that ignores the schema, and "the port is
 * open" does not distinguish either from working.
 */
export interface EndpointReport {
  reachable: boolean;
  healthy: boolean;
  models: string[];
  vision_works: boolean;
  schema_enforced: boolean;
  reasoning_tokens_wasted: number;
  seconds_per_photo: number;
  /** One line the user can act on. */
  verdict: string;
}

/**
 * What tagged a library, and what it did.
 *
 * A tagged union rather than one shape with optional fields: **which tagger ran is the thing
 * the user most needs to know**, and an optional `model` lets a caller forget to show it.
 */
export type TagOutcome =
  | { kind: "remote"; model: string; report: TagPassReport }
  | { kind: "local"; model: string; vocabulary: number; report: ClipPassReport };

/** What a CLIP pass did. */
export interface ClipPassReport {
  tagged: number;
  unreadable: number;
  tags: number;
  elapsed_ms: number;
  vocabulary: number;
}

/**
 * Run a tagging pass.
 *
 * Uses a configured vision endpoint when there is one, and **CLIP on the CPU when there is
 * not**. `limit` bounds one call, so a library can be done in pieces.
 */
export async function runTagPass(libraryId: number, limit?: number): Promise<TagOutcome> {
  return invoke<TagOutcome>("run_tag_pass", { libraryId, limit });
}

/** Exercise the configured endpoint and report what actually works. */
export async function diagnoseEndpoint(): Promise<EndpointReport> {
  return invoke<EndpointReport>("diagnose_endpoint");
}

/** Every tag in a library, with counts, ranked by how many photographs carry it. */
export async function listTags(
  libraryId: number,
  model?: string,
): Promise<Array<[string, number]>> {
  return invoke<Array<[string, number]>>("list_tags", { libraryId, model });
}

/** The tags on one photograph. */
export async function photoTags(photoId: number): Promise<PhotoTagView[]> {
  return invoke<PhotoTagView[]>("photo_tags", { photoId });
}

/** The photographs carrying a tag. */
export async function photosWithTag(
  libraryId: number,
  tag: string,
  model?: string,
): Promise<number[]> {
  return invoke<number[]>("photos_with_tag", { libraryId, tag, model });
}

/** One file belonging to a photograph. */
export interface FileDetail {
  path: string;
  name: string;
  role: string;
  size_bytes: number;
}

/** Everything known about one photograph. */
export interface PhotoDetail {
  photo_id: number;
  stem: string;
  dir: string;
  state: string;
  needs_review: boolean;
  files: FileDetail[];
  camera: string | null;
  lens: string | null;
  iso: number | null;
  f_number: number | null;
  exposure_time: number | null;
  focal_length: number | null;
  captured_at: number | null;
  composite: number | null;
  band: string | null;
  terms: Array<[string, number]>;
  rating: number;
  rejected: boolean;
}

/**
 * Everything known about one photograph.
 *
 * One call, not several: a panel that fetches its EXIF, then its files, then its scores
 * arrives in three visible stages and the middle one looks like a bug.
 */
export async function photoDetail(photoId: number): Promise<PhotoDetail> {
  return invoke<PhotoDetail>("photo_detail", { photoId });
}

/** Every remembered value. */
export async function getSettings(): Promise<Record<string, string>> {
  return invoke<Record<string, string>>("get_settings");
}

/** Remember a value. */
export async function setSetting(key: string, value: string): Promise<void> {
  return invoke<void>("set_setting", { key, value });
}

/** One folder in a library, with how many photographs it holds. */
export interface DirectoryView {
  path: string;
  /** Photographs whose files sit directly in this folder. */
  direct: number;
  /** Photographs in this folder or any folder beneath it. */
  recursive: number;
}

/**
 * Every folder holding photographs, with counts.
 *
 * Flat, and turned into a tree here — the engine has no opinion about how a hierarchy is
 * displayed.
 */
export async function listDirectories(libraryId: number): Promise<DirectoryView[]> {
  return invoke<DirectoryView[]>("list_directories", { libraryId });
}

/** Every photograph in a library, with its score. Thumbnails are not included. */
export async function listPhotos(libraryId: number): Promise<PhotoView[]> {
  return invoke<PhotoView[]>("list_photos", { libraryId });
}

/**
 * Render or fetch one thumbnail.
 *
 * `null` is a normal answer, not a failure: it means the source could not be read, which
 * happens for raw formats this build has no decoder for. The grid shows a placeholder.
 */
export async function photoThumbnail(
  photoId: number,
  size: ThumbSize = "grid",
): Promise<ThumbnailView | null> {
  return invoke<ThumbnailView | null>("photo_thumbnail", { photoId, size });
}

/** The explanation for one photograph's score, rebuilt from stored terms. */
export async function photoExplanation(photoId: number): Promise<string[]> {
  return invoke<string[]>("photo_explanation", { photoId });
}

/**
 * Record what the user decided.
 *
 * Returns the **previous** decision, which is what an undo stack is built from. The
 * frontend updates its own state optimistically and pushes this value, so undo restores
 * exactly what was there rather than guessing.
 */
export async function setDecision(
  photoId: number,
  rating: number,
  rejected: boolean,
): Promise<DecisionView> {
  return invoke<DecisionView>("set_decision", { photoId, rating, rejected });
}

/**
 * The Capability Report for this machine.
 *
 * Plain text, because it is shown to a person and pasted into bug reports. Probing spawns
 * a vendor tool (`nvidia-smi`, `system_profiler`), so it is not fetched on every render.
 */
export async function capabilities(endpoints: string[] = []): Promise<string> {
  return invoke<string>("capabilities", { endpoints });
}

// ---------------------------------------------------------------------------
// Deleting
// ---------------------------------------------------------------------------
export interface DeleteFileView {
  name: string;
  path: string;
  bytes: number;
}

export interface DeleteCandidateView {
  // **snake_case, because that is what Rust sends.** See `tools/check_view_fields.py`.
  photo_id: number;
  stem: string;
  files: DeleteFileView[];
  bytes: number;
  /** True when this photograph has only one of its two halves. */
  incomplete: boolean;
}

export interface DeletePlanView {
  candidates: DeleteCandidateView[];
  photographs: number;
  files: number;
  bytes: number;
  incomplete: number;
  warnings: string[];
  missing: number;
  /** Non-empty means the operation is impossible; the UI must not offer to proceed. */
  refusals: string[];
}

export interface DeleteReceiptView {
  op_id: string;
  moved: number;
  bytes: number;
  warnings: string[];
}

export interface TrashOperationView {
  op_id: string;
  at: number;
  date: string;
  reason: string;
  files: number;
  bytes: number;
  purged: boolean;
}

export interface RestoreView {
  op_id: string;
  restored: number;
  already_present: number;
  blocked: string[];
}

export interface PurgeView {
  operations: number;
  removed: number;
  bytes: number;
}

/** What a delete would move. Changes nothing — this is what the user confirms. */
export async function planDelete(
  libraryRoot: string,
  photoIds: number[],
): Promise<DeletePlanView> {
  return invoke<DeletePlanView>("plan_delete", { libraryRoot, photoIds });
}

/**
 * Move the selection to the trash.
 *
 * Re-resolves from the photograph ids on the Rust side and re-hashes every file, so
 * nothing the webview sends can name a file the engine did not choose itself.
 */
export async function commitDelete(
  libraryRoot: string,
  photoIds: number[],
  reason: string,
): Promise<DeleteReceiptView> {
  return invoke<DeleteReceiptView>("commit_delete", { libraryRoot, photoIds, reason });
}

export async function listTrash(libraryRoot: string): Promise<TrashOperationView[]> {
  return invoke<TrashOperationView[]>("list_trash", { libraryRoot });
}

export async function restoreTrash(libraryRoot: string, opId: string): Promise<RestoreView> {
  return invoke<RestoreView>("restore_trash", { libraryRoot, opId });
}

/** Empty the trash. The only call in this application that unlinks a file. */
export async function purgeTrash(libraryRoot: string, opIds: string[]): Promise<PurgeView> {
  return invoke<PurgeView>("purge_trash", { libraryRoot, opIds });
}

/** Force the thumbnail cache back inside its cap. */
export async function trimThumbnailCache(): Promise<number> {
  return invoke<number>("trim_thumbnail_cache");
}
