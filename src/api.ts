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
 * Index a folder and score it.
 *
 * Long-running — a first index of a large library is minutes — and the Rust side
 * dispatches it onto a blocking thread, so the window stays responsive while it runs.
 */
export async function openLibrary(path: string): Promise<LibraryView> {
  return invoke<LibraryView>("open_library", { path });
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

/** Force the thumbnail cache back inside its cap. */
export async function trimThumbnailCache(): Promise<number> {
  return invoke<number>("trim_thumbnail_cache");
}
