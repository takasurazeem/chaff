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
  photoId: number;
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
  opId: string;
  moved: number;
  bytes: number;
  warnings: string[];
}

export interface TrashOperationView {
  opId: string;
  at: number;
  date: string;
  reason: string;
  files: number;
  bytes: number;
  purged: boolean;
}

export interface RestoreView {
  opId: string;
  restored: number;
  alreadyPresent: number;
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
