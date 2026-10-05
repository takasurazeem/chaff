//! The face pass: detect, embed, group.
//!
//! # Why it is a separate pass and not part of indexing
//!
//! Indexing decodes every photograph to score it, and face detection decodes the same
//! photographs again. Running both in one pass would be twice the work for the same pixels,
//! and running faces *inside* indexing would make the index depend on ONNX Runtime — which
//! is the whole reason `chaff-faces` is a separate crate.
//!
//! So it is a pass the user starts, it reports what it did, and it is **resumable**: each
//! file is committed as it is processed, so closing the window halfway through loses
//! nothing. That matters because the first pass over a library downloads a 38 MB model and
//! then runs a network over several thousand images.
//!
//! # What it does not do
//!
//! It does not name anyone. It produces groups, and a group is a suggestion — confirming or
//! correcting it is the review UI (#46).

use std::path::Path;

use chaff_core::catalog::store;
use chaff_core::rusqlite::Connection;
use chaff_core::thumb;
use chaff_faces::{cluster, cosine, ClusteringConfig, Detector, ModelStore, Recogniser, YuNet, SFACE};
use serde::Serialize;

/// What a face pass did.
#[derive(Debug, Clone, Serialize)]
pub struct FacePassReport {
    pub detected_files: usize,
    pub faces_found: usize,
    pub embedded: usize,
    /// Files whose image data could not be read. A raw format this build has no decoder for.
    pub unreadable: usize,
    pub people: usize,
    /// The model's licence, so the UI can show it where the feature is switched on.
    pub licence: String,
    pub elapsed_ms: u128,
}

/// Where downloaded models live.
///
/// The app data directory, not the library: the library is the user's and is not ours to
/// write into. Not beside the source either, which may be read-only in a packaged build.
pub fn model_store(app_data: &Path) -> ModelStore {
    ModelStore::new(app_data.join("models"))
}

/// Run a face pass over a library.
pub fn run(
    conn: &mut Connection,
    app_data: &Path,
    library_id: i64,
    now: i64,
    on_progress: &mut dyn FnMut(usize, usize),
) -> Result<FacePassReport, String> {
    let started = std::time::Instant::now();

    let detector_path = YuNet::bundled()
        .ok_or("the face detector model is missing from this build")?;
    let detector = YuNet::from_file(&detector_path).map_err(|e| e.to_string())?;

    // Fetched on first use, verified before it is written. See `chaff_faces::models`.
    let store = model_store(app_data);
    let recogniser_path = store
        .ensure(&SFACE, |_, _| {})
        .map_err(|e| format!("could not fetch the face recognition model: {e}"))?;
    let recogniser = Recogniser::from_file(&recogniser_path).map_err(|e| e.to_string())?;

    let mut report = FacePassReport {
        detected_files: 0,
        faces_found: 0,
        embedded: 0,
        unreadable: 0,
        people: 0,
        licence: SFACE.licence.to_string(),
        elapsed_ms: 0,
    };

    // --- Detection -----------------------------------------------------------
    let pending = store::files_needing_faces(conn, library_id, detector.name())
        .map_err(|e| e.to_string())?;
    let total = pending.len();

    for (i, file) in pending.iter().enumerate() {
        on_progress(i, total);

        let Ok(img) = thumb::decode_source(Path::new(&file.path)) else {
            report.unreadable += 1;
            continue;
        };
        let rgb = img.to_rgb8();
        let Ok(found) = detector.detect(rgb.as_raw(), rgb.width(), rgb.height()) else {
            report.unreadable += 1;
            continue;
        };

        let rows: Vec<store::FaceRow> = found
            .iter()
            .map(|f| store::FaceRow {
                file_id: file.id,
                x: f.x as f64,
                y: f.y as f64,
                width: f.width as f64,
                height: f.height as f64,
                confidence: f.confidence as f64,
            })
            .collect();

        // The landmarks as raw little-endian f32, in the order the model emits them. Kept
        // because alignment needs them, and re-running detection to recover them would mean
        // re-running the model over the whole library.
        let landmarks: Vec<Vec<u8>> = found
            .iter()
            .map(|f| {
                f.landmarks
                    .iter()
                    .flat_map(|p| [p[0].to_le_bytes(), p[1].to_le_bytes()])
                    .flatten()
                    .collect()
            })
            .collect();

        store::replace_faces(
            conn,
            &store::FaceDetection {
                file_id: file.id,
                faces: &rows,
                landmarks: &landmarks,
                size: file.size_bytes,
                mtime: file.mtime_ns,
                detector: detector.name(),
                now,
            },
        )
        .map_err(|e| e.to_string())?;

        report.detected_files += 1;
        report.faces_found += found.len();
    }
    on_progress(total, total);

    // --- Embeddings ----------------------------------------------------------
    let needing = store::faces_needing_embeddings(conn, library_id, recogniser_model())
        .map_err(|e| e.to_string())?;
    for (face_id, photo_id) in &needing {
        let Some((_, _, landmarks)) = face_geometry(conn, *face_id)? else { continue };
        let Some(path) = primary_file_for_photo(conn, *photo_id)? else { continue };
        let Ok(img) = thumb::decode_source(Path::new(&path)) else { continue };
        let rgb = img.to_rgb8();

        let Some(face) = detection_from(landmarks) else { continue };
        let Ok(vector) = recogniser.embed(rgb.as_raw(), rgb.width(), rgb.height(), &face) else {
            continue;
        };

        store::upsert_embedding(conn, *face_id, &vector, recogniser_model(), now)
            .map_err(|e| e.to_string())?;
        report.embedded += 1;
    }

    // --- Grouping ------------------------------------------------------------
    let faces = store::faces_with_embeddings(conn, library_id, recogniser_model())
        .map_err(|e| e.to_string())?;
    let vectors: Vec<Vec<f32>> = faces.iter().map(|(_, _, v)| v.clone()).collect();
    let config = ClusteringConfig::default();

    let groups: Vec<Vec<i64>> = cluster(&vectors, &config)
        .into_iter()
        .map(|c| c.members.iter().map(|i| faces[*i].0).collect())
        .collect();

    report.people = store::replace_people(conn, library_id, &groups, now).map_err(|e| e.to_string())?;
    report.elapsed_ms = started.elapsed().as_millis();

    // The licence is logged where the feature runs, not only where it is chosen. Faces are
    // biometric data and which model read them is part of the record.
    log::info!(
        "face pass: {} files, {} faces, {} embedded, {} unreadable, {} groups, {:.1}s ({} {})",
        report.detected_files,
        report.faces_found,
        report.embedded,
        report.unreadable,
        report.people,
        report.elapsed_ms as f64 / 1000.0,
        SFACE.file,
        SFACE.licence,
    );

    Ok(report)
}

/// How a recogniser's output is keyed in the catalog.
///
/// The filename, not "SFace": two versions of one model are different recognisers, and
/// reusing one's embeddings for the other would silently mix two definitions of identity.
fn recogniser_model() -> &'static str {
    SFACE.file
}

/// The landmarks stored for a face, and the file it was found in.
fn face_geometry(
    conn: &Connection,
    face_id: i64,
) -> Result<Option<(i64, i64, Vec<f32>)>, String> {
    let mut stmt = conn
        .prepare("SELECT file_id, landmarks FROM face WHERE id = ?1")
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query(chaff_core::rusqlite::params![face_id]).map_err(|e| e.to_string())?;
    let Some(row) = rows.next().map_err(|e| e.to_string())? else { return Ok(None) };

    let file_id: i64 = row.get(0).map_err(|e| e.to_string())?;
    let blob: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
    let landmarks = blob
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Ok(Some((file_id, 0, landmarks)))
}

/// Rebuild a `Detection` from stored landmarks.
///
/// The box is not stored with the landmarks here because alignment needs only the five
/// points; the box is recovered from them closely enough for a warp.
fn detection_from(landmarks: Vec<f32>) -> Option<chaff_faces::Detection> {
    if landmarks.len() != 10 {
        return None;
    }
    let mut points = [[0f32; 2]; 5];
    for (i, p) in points.iter_mut().enumerate() {
        *p = [landmarks[i * 2], landmarks[i * 2 + 1]];
    }
    let xs: Vec<f32> = points.iter().map(|p| p[0]).collect();
    let ys: Vec<f32> = points.iter().map(|p| p[1]).collect();
    let min_x = xs.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_x = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let min_y = ys.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_y = ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

    Some(chaff_faces::Detection {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
        confidence: 1.0,
        landmarks: points,
    })
}

/// The file a photograph's pixels come from, preferring the raw.
fn primary_file_for_photo(conn: &Connection, photo_id: i64) -> Result<Option<String>, String> {
    let files = store::files_for_photo(conn, photo_id).map_err(|e| e.to_string())?;
    Ok(files
        .iter()
        .find(|f| f.role == "raw")
        .or_else(|| files.iter().find(|f| f.role == "raster"))
        .map(|f| f.path.clone()))
}

/// Cosine similarity between two faces, for the review UI.
pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    cosine(a, b)
}
