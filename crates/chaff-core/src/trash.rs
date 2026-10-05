//! Two-phase deletion: the only code in Chaff that can remove a photograph.
//!
//! # The shape of the guarantee
//!
//! **Reject is a flag. Only trash moves files. Only purge unlinks them.**
//!
//! Those are three separate operations and only the last is destructive, which is what
//! makes the tool safe to use: being wrong about a rejection costs a glance, and being
//! wrong about a deletion costs a photograph only after the user has separately said
//! "empty the trash".
//!
//! # Why the manifest is written before the move
//!
//! Every operation appends a JSONL line and **fsyncs it** before a single file moves. If
//! the process dies in between, the manifest describes a file that is still where it was
//! — which [`Trash::restore`] detects and treats as a no-op. The reverse order would leave
//! files in the trash that nothing has a record of, which is the one state this design
//! cannot recover from.
//!
//! # Why every file is re-hashed immediately before it moves
//!
//! The catalog's hash was computed when the library was indexed. In between, an editor may
//! have rewritten a sidecar, or a sync client may have replaced a file. Moving a file on
//! the strength of a stale fingerprint is moving something the user did not ask about, so
//! a mismatch aborts the whole operation rather than proceeding with the rest.
//!
//! # Why the moves are transactional
//!
//! A pair is one photograph. Half a pair in the trash is exactly the orphaned-half problem
//! the product exists to solve, created by the product. So: verify everything, then move,
//! and if any move fails, move back the ones that succeeded.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TrashError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("refused: {0}")]
    Refused(#[from] Refusal),
    #[error(
        "the contents of {path} have changed since it was indexed, so this operation was \
         abandoned before moving anything. Expected {expected}, found {found}."
    )]
    Changed { path: PathBuf, expected: String, found: String },
    #[error(
        "{path} has no recorded hash, so it cannot be verified. It was not part of the plan \
         you were shown — something added it between the confirmation and the move."
    )]
    Unverified { path: PathBuf },
    #[error("no trashed operation with id {0}")]
    UnknownOperation(String),
    #[error("the manifest at {path} could not be parsed at line {line}: {source}")]
    BadManifest {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

/// Why an operation was refused.
///
/// Each variant exists because the corresponding mistake is plausible and expensive. They
/// are separate rather than one "unsafe path" error so the message can say what is
/// actually wrong.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Refusal {
    #[error(
        "{path} is not inside the library at {root}. Chaff only ever moves files it was \
         given, and only from the folder you opened."
    )]
    OutsideLibrary { path: PathBuf, root: PathBuf },

    #[error("{path} is a filesystem root. Refusing to operate on a whole volume.")]
    FilesystemRoot { path: PathBuf },

    #[error("{path} is inside the trash folder itself.")]
    InsideTrash { path: PathBuf },

    #[error(
        "{root} looks like a camera card — it contains DCIM and MISC. Import it first; \
         removing files from a card is not what this is for."
    )]
    CameraCard { root: PathBuf },

    #[error("{path} is not writable.")]
    ReadOnly { path: PathBuf },

    #[error(
        "{path} is present, which means another application has this library open. Close \
         it and try again — Chaff will not move files out from under an editor."
    )]
    EditorLock { path: PathBuf },

    #[error("{path} is a symbolic link resolving to {resolved}, outside the library.")]
    SymlinkEscape { path: PathBuf, resolved: PathBuf },
}

/// Editor lock files that mean "do not touch this library".
///
/// Lightroom's catalog and write-ahead log, and darktable's library lock. Presence means
/// another application is mid-write, and moving files under it produces a catalog that
/// references files that no longer exist.
const EDITOR_LOCKS: &[&str] = &[
    ".lrcat-wal",
    ".lrcat.lock",
    ".lrcat-journal",
    "library.db.lock",
    ".darktable.lock",
];

/// A file scheduled to move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedMove {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub size: i64,
    /// The hash the catalog recorded, when it has one.
    pub expected_hash: Option<String>,
}

/// What an operation will do, before it does anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashPlan {
    pub op_id: String,
    pub date: String,
    pub moves: Vec<PlannedMove>,
    /// Conditions worth telling the user about that are not refusals.
    pub warnings: Vec<Warning>,
}

impl TrashPlan {
    pub fn total_bytes(&self) -> i64 {
        self.moves.iter().map(|m| m.size).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }
}

/// A condition that does not stop the operation but must be said out loud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// The file and the trash folder are on different volumes.
    ///
    /// A rename across volumes is not atomic — it is a copy followed by a delete, which is
    /// a categorically different risk from the same-volume case.
    CrossVolume { source: PathBuf, destination: PathBuf },
    /// A file that was expected to exist does not.
    Missing { path: PathBuf },
}

/// What actually happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashReceipt {
    pub op_id: String,
    pub moved: usize,
    pub bytes: i64,
    pub warnings: Vec<Warning>,
}

/// One recorded operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub op_id: String,
    pub at: i64,
    pub action: String,
    pub reason: String,
    pub files: Vec<PlannedMove>,
}

/// The result of restoring an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    pub op_id: String,
    pub restored: usize,
    pub already_present: usize,
    pub missing: Vec<PathBuf>,
}

/// The result of purging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeReceipt {
    pub operations: usize,
    pub removed: usize,
    pub bytes: i64,
}

/// The trash folder for one library.
#[derive(Debug, Clone)]
pub struct Trash {
    root: PathBuf,
    dir: PathBuf,
    manifest: PathBuf,
}

impl Trash {
    /// The folder name, inside the library. Fixed by ADR-0004.
    pub const DIR_NAME: &'static str = ".cull-trash";

    /// Open the trash for a library, creating it if needed.
    ///
    /// Refuses up front when the library itself is dangerous — a camera card, or a folder
    /// another application has open — rather than discovering it per file.
    pub fn open(library_root: &Path) -> Result<Self, TrashError> {
        let root = library_root.to_path_buf();
        let dir = root.join(Self::DIR_NAME);

        if let Some(lock) = editor_lock(&root) {
            return Err(TrashError::Refused(Refusal::EditorLock { path: lock }));
        }
        if looks_like_camera_card(&root) {
            return Err(TrashError::Refused(Refusal::CameraCard { root }));
        }

        std::fs::create_dir_all(&dir).map_err(|source| TrashError::Io { path: dir.clone(), source })?;

        // Writability is checked once, here, rather than per file. A read-only mount makes
        // every subsequent move fail, and failing at the door with a clear reason beats
        // failing halfway through a pair.
        let probe = dir.join(".write-probe");
        match std::fs::write(&probe, b"") {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
            }
            Err(_) => return Err(TrashError::Refused(Refusal::ReadOnly { path: dir })),
        }

        Ok(Self { manifest: dir.join("MANIFEST.jsonl"), root, dir })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Check whether a path may be moved at all.
    ///
    /// Separated from [`Self::plan`] so the UI can ask about a selection before building a
    /// plan, and so the rules are testable one at a time.
    pub fn check(&self, path: &Path) -> Result<(), Refusal> {
        // A filesystem root has no parent. Checked before canonicalisation because `..`
        // on a root is itself.
        if path.parent().is_none() {
            return Err(Refusal::FilesystemRoot { path: path.to_path_buf() });
        }

        // A file that is not there cannot be canonicalised. Falling through to the
        // symlink check would report it as unreadable, which is wrong twice over: it is
        // not a refusal at all, and the caller has a `Missing` warning for exactly this
        // case. A lexical containment check still catches a missing path outside the
        // library, and `plan` reports the absence.
        if !path.exists() {
            if !path.starts_with(&self.root) {
                return Err(Refusal::OutsideLibrary {
                    path: path.to_path_buf(),
                    root: self.root.clone(),
                });
            }
            return Ok(());
        }

        // Resolve symlinks before deciding anything. A link inside the library pointing
        // outside it would otherwise pass every textual check and then move a file the
        // user never offered.
        let resolved = path
            .canonicalize()
            .map_err(|_| Refusal::ReadOnly { path: path.to_path_buf() })?;
        let resolved_root = self
            .root
            .canonicalize()
            .map_err(|_| Refusal::ReadOnly { path: self.root.clone() })?;

        if path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false)
            && !resolved.starts_with(&resolved_root)
        {
            return Err(Refusal::SymlinkEscape { path: path.to_path_buf(), resolved });
        }

        if !resolved.starts_with(&resolved_root) {
            return Err(Refusal::OutsideLibrary {
                path: path.to_path_buf(),
                root: self.root.clone(),
            });
        }

        let resolved_trash = self
            .dir
            .canonicalize()
            .unwrap_or_else(|_| self.dir.clone());
        if resolved.starts_with(&resolved_trash) {
            return Err(Refusal::InsideTrash { path: path.to_path_buf() });
        }

        Ok(())
    }

    /// Work out what an operation would do. Changes nothing.
    pub fn plan(
        &self,
        files: &[PathBuf],
        hashes: &std::collections::HashMap<PathBuf, String>,
        now: i64,
    ) -> Result<TrashPlan, TrashError> {
        let date = civil_date(now);
        let op_id = format!("{date}-{:06}", now.rem_euclid(1_000_000));

        let mut moves = Vec::new();
        let mut warnings = Vec::new();
        // Destinations claimed by this plan, so two files cannot be given the same one.
        let mut claimed: BTreeSet<PathBuf> = BTreeSet::new();

        for file in files {
            self.check(file)?;

            let Ok(md) = std::fs::metadata(file) else {
                // A file that vanished between selection and planning. A warning, not an
                // error: the user selected it, and it is no longer there to move.
                warnings.push(Warning::Missing { path: file.clone() });
                continue;
            };

            let relative = file.strip_prefix(&self.root).unwrap_or(file);
            let mut destination = self.dir.join(&date).join(relative);

            // Two files can share a relative path across operations on the same day. A
            // suffix keeps the layout browsable, where embedding the operation id would
            // make the trash a wall of opaque folders.
            let mut n = 2;
            while destination.exists() || claimed.contains(&destination) {
                destination = suffixed(&self.dir.join(&date).join(relative), n);
                n += 1;
            }
            claimed.insert(destination.clone());

            if different_volume(file, &self.dir) {
                warnings.push(Warning::CrossVolume {
                    source: file.clone(),
                    destination: destination.clone(),
                });
            }

            moves.push(PlannedMove {
                source: file.clone(),
                destination,
                size: md.len() as i64,
                expected_hash: hashes.get(file).cloned(),
            });
        }

        Ok(TrashPlan { op_id, date, moves, warnings })
    }

    /// Perform a plan.
    ///
    /// Re-hashes every file, appends and fsyncs the manifest, then moves. Rolls back on
    /// any failure.
    pub fn commit(
        &self,
        plan: &TrashPlan,
        reason: &str,
        now: i64,
    ) -> Result<TrashReceipt, TrashError> {
        if plan.is_empty() {
            return Ok(TrashReceipt {
                op_id: plan.op_id.clone(),
                moved: 0,
                bytes: 0,
                warnings: plan.warnings.clone(),
            });
        }

        // 1. Verify everything before moving anything. A mismatch aborts the whole
        //    operation, because proceeding would move a file the user was not shown.
        for m in &plan.moves {
            // **A file the map does not cover is refused, not skipped.**
            //
            // This was `else { continue }`, and it was the primitive that made every caller's
            // omission silent. `DeleteSession::commit` re-resolves its selection from the
            // catalog, so a file that *appeared* between the plan and the confirmation got no
            // hash — and was then moved **unverified**, with the module documenting a
            // guarantee that did not cover it.
            //
            // A caller that hands `plan` a hash map is stating which files it has verified.
            // Moving one outside that set is the exact thing this loop exists to prevent, so
            // the absence of a hash is a refusal like any other.
            let Some(expected) = &m.expected_hash else {
                return Err(TrashError::Unverified { path: m.source.clone() });
            };
            let actual = hash_file(&m.source)?;
            if &actual != expected {
                return Err(TrashError::Changed {
                    path: m.source.clone(),
                    expected: expected.clone(),
                    found: actual,
                });
            }
        }

        // 2. Record the intent, and make sure it is on disk before a file moves.
        let entry = ManifestEntry {
            op_id: plan.op_id.clone(),
            at: now,
            action: "trash".to_string(),
            reason: reason.to_string(),
            files: plan.moves.clone(),
        };
        self.append_manifest(&entry)?;

        // 3. Move. Track what succeeded so a failure can be undone.
        let mut done: Vec<&PlannedMove> = Vec::with_capacity(plan.moves.len());
        for m in &plan.moves {
            if let Some(parent) = m.destination.parent() {
                if let Err(source) = std::fs::create_dir_all(parent) {
                    self.rollback(&done);
                    return Err(TrashError::Io { path: parent.to_path_buf(), source });
                }
            }
            if let Err(source) = std::fs::rename(&m.source, &m.destination) {
                self.rollback(&done);
                return Err(TrashError::Io { path: m.source.clone(), source });
            }
            done.push(m);
        }

        Ok(TrashReceipt {
            op_id: plan.op_id.clone(),
            moved: done.len(),
            bytes: done.iter().map(|m| m.size).sum(),
            warnings: plan.warnings.clone(),
        })
    }

    /// Move back everything a failed operation had already moved.
    ///
    /// Best effort, and deliberately silent about its own failures: it is running on a
    /// path that is already returning an error, and the error the caller sees must be the
    /// original one rather than a rollback problem.
    fn rollback(&self, done: &[&PlannedMove]) {
        for m in done.iter().rev() {
            let _ = std::fs::rename(&m.destination, &m.source);
        }
    }

    /// Append one line to the manifest and fsync it.
    fn append_manifest(&self, entry: &ManifestEntry) -> Result<(), TrashError> {
        let line = serde_json::to_string(entry).map_err(|e| TrashError::BadManifest {
            path: self.manifest.clone(),
            line: 0,
            source: e,
        })?;

        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.manifest)
            .map_err(|source| TrashError::Io { path: self.manifest.clone(), source })?;
        writeln!(f, "{line}").map_err(|source| TrashError::Io { path: self.manifest.clone(), source })?;
        // The fsync is the guarantee. Without it the line can sit in the page cache while
        // files are already moving, which is precisely the state this ordering exists to
        // prevent.
        f.sync_all().map_err(|source| TrashError::Io { path: self.manifest.clone(), source })?;
        Ok(())
    }

    /// Every recorded operation, oldest first.
    pub fn manifest(&self) -> Result<Vec<ManifestEntry>, TrashError> {
        let Ok(text) = std::fs::read_to_string(&self.manifest) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let entry = serde_json::from_str(line).map_err(|source| TrashError::BadManifest {
                path: self.manifest.clone(),
                line: i + 1,
                source,
            })?;
            out.push(entry);
        }
        Ok(out)
    }

    /// Move an operation's files back where they came from.
    ///
    /// Verifies each file's contents before writing it back. A mismatch is reported rather
    /// than overwritten: the file at the destination is what the user had, and replacing it
    /// with something else because a restore was requested would be its own data loss.
    pub fn restore(&self, op_id: &str) -> Result<RestoreReport, TrashError> {
        let entry = self
            .manifest()?
            .into_iter()
            .find(|e| e.op_id == op_id)
            .ok_or_else(|| TrashError::UnknownOperation(op_id.to_string()))?;

        let mut restored = 0;
        let mut already_present = 0;
        let mut missing = Vec::new();

        for m in &entry.files {
            // **The manifest is a plaintext file inside the library.** Anything that can
            // write to the library can edit it — the user, an editor, a sync client — so a
            // `source` path in it is a claim, not a fact.
            //
            // `purge` already re-checks its destination before unlinking. This is the
            // symmetric check, and it was missing: a hand-edited manifest could move a
            // trashed file to an arbitrary absolute path. Not arbitrary *content*, but a
            // violation of "never touch anything outside the library" all the same.
            let source_ok = m
                .source
                .canonicalize()
                .ok()
                .zip(self.root.canonicalize().ok())
                .map(|(src, root)| src.starts_with(&root))
                // The source does not exist yet — `canonicalize` fails on a missing path —
                // so fall back to a lexical containment test against the root.
                .unwrap_or_else(|| m.source.starts_with(&self.root));

            if !source_ok {
                log::error!(
                    "refusing to restore {}: the manifest names a destination outside the \
                     library at {}",
                    m.destination.display(),
                    m.source.display()
                );
                missing.push(m.source.clone());
                continue;
            }

            if !m.destination.is_file() {
                // Either it was purged, or it never moved because the operation failed
                // after the manifest was written.
                if m.source.is_file() {
                    already_present += 1;
                } else {
                    missing.push(m.destination.clone());
                }
                continue;
            }

            // Verify before writing back, when a hash was recorded.
            if let Some(expected) = &m.expected_hash {
                let actual = hash_file(&m.destination)?;
                if &actual != expected {
                    return Err(TrashError::Changed {
                        path: m.destination.clone(),
                        expected: expected.clone(),
                        found: actual,
                    });
                }
            }

            if m.source.exists() {
                // Something is already there. Report it rather than overwrite.
                missing.push(m.source.clone());
                continue;
            }
            if let Some(parent) = m.source.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|source| TrashError::Io { path: parent.to_path_buf(), source })?;
            }
            std::fs::rename(&m.destination, &m.source)
                .map_err(|source| TrashError::Io { path: m.source.clone(), source })?;
            restored += 1;
        }

        Ok(RestoreReport { op_id: op_id.to_string(), restored, already_present, missing })
    }

    /// **The only place in Chaff that unlinks a file.**
    ///
    /// Deliberately takes explicit operation ids rather than "everything": a purge that
    /// decides for itself what to remove is a purge that can be wrong about it.
    pub fn purge(&self, op_ids: &[String]) -> Result<PurgeReceipt, TrashError> {
        let mut receipt = PurgeReceipt { operations: 0, removed: 0, bytes: 0 };

        for op_id in op_ids {
            let entry = self
                .manifest()?
                .into_iter()
                .find(|e| e.op_id == *op_id)
                .ok_or_else(|| TrashError::UnknownOperation(op_id.clone()))?;
            receipt.operations += 1;

            for m in &entry.files {
                if !m.destination.is_file() {
                    continue;
                }
                // A last check that the path really is inside the trash. Unlinking is
                // irreversible and this is the one function that does it, so it does not
                // take the manifest's word for where a file lives.
                let resolved = m.destination.canonicalize().unwrap_or_else(|_| m.destination.clone());
                let trash = self.dir.canonicalize().unwrap_or_else(|_| self.dir.clone());
                if !resolved.starts_with(&trash) {
                    continue;
                }

                match std::fs::remove_file(&m.destination) {
                    Ok(()) => {
                        receipt.removed += 1;
                        receipt.bytes += m.size;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(TrashError::Io { path: m.destination.clone(), source })
                    }
                }
            }

            // The receipt, appended after the fact. A purge that crashed halfway leaves
            // the manifest intact, so what remains is still restorable.
            let _ = self.append_manifest(&ManifestEntry {
                op_id: format!("{}-purged", entry.op_id),
                at: entry.at,
                action: "purge".to_string(),
                reason: format!("purged {} file(s)", entry.files.len()),
                files: Vec::new(),
            });
        }

        Ok(receipt)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// BLAKE3 of a file's contents.
pub fn hash_file(path: &Path) -> Result<String, TrashError> {
    let data = std::fs::read(path).map_err(|source| TrashError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(blake3::hash(&data).to_hex().to_string())
}

/// `NAME.ext` -> `NAME~2.ext`.
fn suffixed(path: &Path, n: u32) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().to_string());
    let name = match ext {
        Some(e) => format!("{stem}~{n}.{e}"),
        None => format!("{stem}~{n}"),
    };
    path.with_file_name(name)
}

/// True when two paths are on different filesystems.
///
/// Compares device ids on Unix. On other platforms it conservatively reports `false`,
/// because a false positive would warn about every operation and a warning that always
/// fires is a warning nobody reads.
#[cfg(unix)]
fn different_volume(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let dev = |p: &Path| -> Option<u64> {
        let probe = if p.is_dir() { p.to_path_buf() } else { p.parent()?.to_path_buf() };
        std::fs::metadata(probe).ok().map(|m| m.dev())
    };
    match (dev(a), dev(b)) {
        (Some(x), Some(y)) => x != y,
        _ => false,
    }
}

#[cfg(not(unix))]
fn different_volume(_a: &Path, _b: &Path) -> bool {
    false
}

/// The first editor lock present in a library, if any.
fn editor_lock(root: &Path) -> Option<PathBuf> {
    for name in EDITOR_LOCKS {
        let candidate = root.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// True when a folder has the shape of a camera card.
///
/// `DCIM` plus `MISC` is the layout every camera writes. Importing from a card is normal;
/// *deleting* from one is not what this tool is for, and the card is often the only copy.
fn looks_like_camera_card(root: &Path) -> bool {
    root.join("DCIM").is_dir() && root.join("MISC").is_dir()
}

/// `YYYY-MM-DD` for an epoch second, in UTC.
///
/// Howard Hinnant's `civil_from_days`, the inverse of the one in `exif.rs`. Used for the
/// trash folder name, where UTC is fine: the date groups operations, and an operation near
/// midnight landing in the adjacent folder is not a correctness problem.
pub fn civil_date(epoch_seconds: i64) -> String {
    let z = epoch_seconds.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use tempfile::tempdir;

    /// A library with a pair and an orphan, and a trash folder for it.
    fn library() -> (tempfile::TempDir, Trash, Vec<PathBuf>) {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("shoot")).unwrap();
        let files = vec![
            root.join("shoot/IMG_0001.CR3"),
            root.join("shoot/IMG_0001.JPG"),
            root.join("shoot/IMG_0001.XMP"),
        ];
        for (i, f) in files.iter().enumerate() {
            fs::write(f, format!("contents {i}")).unwrap();
        }
        let trash = Trash::open(root).unwrap();
        (dir, trash, files)
    }

    fn hashes(files: &[PathBuf]) -> HashMap<PathBuf, String> {
        files
            .iter()
            .map(|f| (f.clone(), hash_file(f).unwrap()))
            .collect()
    }

    // ---------------------------------------------------------------------
    // The one unlink call site
    // ---------------------------------------------------------------------
    #[test]
    fn remove_file_appears_in_exactly_two_places_and_only_one_is_destructive() {
        // **The strongest guarantee in this file, asserted structurally.**
        //
        // "Nothing is ever unlinked except by the purge path" is a claim about the whole
        // codebase, and a claim about a codebase is worth exactly as much as the thing
        // that enforces it. This reads the module's own source and counts.
        //
        // Two occurrences, both accounted for:
        //   1. `open` removes the zero-byte write probe it just created.
        //   2. `purge` removes a trashed photograph. This is the destructive one.
        //
        // A third occurrence fails this test, and whoever added it has to argue with a
        // comment explaining why the count is what it is.
        let source = include_str!("trash.rs");
        // Only the implementation. This test module contains `remove_file` calls of its
        // own — setting up fixtures, deleting a file to test the missing-file path — and
        // counting those made the first version of this assertion read five.
        let implementation = &source[..source.find("#[cfg(test)]").expect("a test module")];
        let occurrences = implementation.matches("remove_file(").count();
        assert_eq!(
            occurrences, 2,
            "expected exactly two `remove_file` calls — the write probe in `open` and the \
             destructive one in `purge`. Found {occurrences}. If a third was added \
             deliberately, update this test and the reasoning above rather than deleting it."
        );

        // And the destructive one must be inside `purge`.
        let purge_start = source.find("pub fn purge(").expect("purge exists");
        let after_purge = &source[purge_start..];
        let tests_start = after_purge.find("#[cfg(test)]").unwrap_or(after_purge.len());
        assert!(
            after_purge[..tests_start].contains("remove_file("),
            "the destructive unlink must live in `purge`"
        );
    }

    // ---------------------------------------------------------------------
    // Refusals
    // ---------------------------------------------------------------------
    #[test]
    fn a_file_outside_the_library_is_refused() {
        let (dir, trash, _) = library();
        let outside = tempdir().unwrap();
        let stranger = outside.path().join("someone-elses.jpg");
        fs::write(&stranger, b"x").unwrap();

        match trash.check(&stranger) {
            Err(Refusal::OutsideLibrary { .. }) => {}
            other => panic!("expected OutsideLibrary, got {other:?}"),
        }
        drop(dir);
    }

    #[test]
    fn a_filesystem_root_is_refused() {
        let (_dir, trash, _) = library();
        match trash.check(Path::new("/")) {
            Err(Refusal::FilesystemRoot { .. }) => {}
            other => panic!("expected FilesystemRoot, got {other:?}"),
        }
    }

    #[test]
    fn the_trash_folder_itself_is_refused() {
        // Moving the trash into the trash is how a recursive mess starts.
        let (_dir, trash, files) = library();
        let h = hashes(&files);
        let plan = trash.plan(&files, &h, 1_700_000_000).unwrap();
        trash.commit(&plan, "test", 1_700_000_000).unwrap();

        let inside = trash.dir().join("2023-11-14/shoot/IMG_0001.CR3");
        assert!(inside.is_file(), "the file should be in the trash now");
        match trash.check(&inside) {
            Err(Refusal::InsideTrash { .. }) => {}
            other => panic!("expected InsideTrash, got {other:?}"),
        }
    }

    #[test]
    fn a_camera_card_is_refused_outright() {
        // The card is often the only copy of the photograph. Importing from it is normal;
        // deleting from it is not what this tool is for.
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("DCIM/100CANON")).unwrap();
        fs::create_dir_all(root.join("MISC")).unwrap();
        fs::write(root.join("DCIM/100CANON/IMG_0001.CR3"), b"x").unwrap();

        match Trash::open(root) {
            Err(TrashError::Refused(Refusal::CameraCard { .. })) => {}
            other => panic!("expected CameraCard, got {other:?}"),
        }
    }

    #[test]
    fn a_library_with_an_editor_lock_is_refused_outright() {
        // Moving files under a running Lightroom produces a catalog referencing files
        // that no longer exist, which is worse than refusing.
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join(".lrcat-wal"), b"").unwrap();

        match Trash::open(root) {
            Err(TrashError::Refused(Refusal::EditorLock { .. })) => {}
            other => panic!("expected EditorLock, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_escaping_the_library_is_refused() {
        // The check that textual path comparison would miss entirely.
        let outside = tempdir().unwrap();
        let stranger = outside.path().join("not-yours.jpg");
        fs::write(&stranger, b"x").unwrap();

        let (dir, trash, _) = library();
        let link = dir.path().join("shoot/innocent-looking.jpg");
        std::os::unix::fs::symlink(&stranger, &link).unwrap();

        match trash.check(&link) {
            Err(Refusal::SymlinkEscape { .. }) => {}
            other => panic!("expected SymlinkEscape, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------
    // Planning
    // ---------------------------------------------------------------------
    #[test]
    fn planning_changes_nothing_on_disk() {
        let (_dir, trash, files) = library();
        let before: Vec<_> = files.iter().map(|f| fs::read(f).unwrap()).collect();

        let plan = trash.plan(&files, &hashes(&files), 1_700_000_000).unwrap();
        assert_eq!(plan.moves.len(), 3);

        for (f, b) in files.iter().zip(&before) {
            assert!(f.is_file(), "planning must not move anything");
            assert_eq!(&fs::read(f).unwrap(), b);
        }
        assert!(!trash.manifest().unwrap().iter().any(|e| e.op_id == plan.op_id));
    }

    #[test]
    fn destinations_are_dated_and_mirror_the_library_layout() {
        let (_dir, trash, files) = library();
        let plan = trash.plan(&files, &hashes(&files), 1_700_000_000).unwrap();

        // 2023-11-14 is the date for that epoch second.
        assert_eq!(plan.date, "2023-11-14");
        for m in &plan.moves {
            assert!(
                m.destination.starts_with(trash.dir().join("2023-11-14")),
                "destination {} is not under the dated trash",
                m.destination.display()
            );
            assert!(
                m.destination.to_string_lossy().contains("shoot/IMG_0001"),
                "the library layout must be preserved so a restore is obvious: {}",
                m.destination.display()
            );
        }
    }

    #[test]
    fn two_files_with_the_same_relative_path_do_not_collide() {
        // Trash a file, put a *different* file at the same path, trash that on the same
        // day. The second must not land on the first.
        //
        // The first version of this test restored between the two operations, which frees
        // the trash paths — so reusing them is correct and the test was asserting that
        // collisions are impossible rather than that they are handled.
        let (_dir, trash, files) = library();
        let h = hashes(&files);
        let first = trash.plan(&files, &h, 1_700_000_000).unwrap();
        trash.commit(&first, "test", 1_700_000_000).unwrap();

        // Different contents at the same paths.
        for (i, f) in files.iter().enumerate() {
            fs::write(f, format!("replaced {i}")).unwrap();
        }
        let second = trash.plan(&files, &hashes(&files), 1_700_000_000).unwrap();
        let a: BTreeSet<_> = first.moves.iter().map(|m| m.destination.clone()).collect();
        let b: BTreeSet<_> = second.moves.iter().map(|m| m.destination.clone()).collect();
        assert!(a.is_disjoint(&b), "the second operation must not reuse the first's paths");
    }

    #[test]
    fn a_file_that_vanished_before_planning_is_a_warning_not_an_error() {
        let (_dir, trash, files) = library();
        let h = hashes(&files);
        fs::remove_file(&files[2]).unwrap();

        let plan = trash.plan(&files, &h, 1_700_000_000).unwrap();
        assert_eq!(plan.moves.len(), 2, "the surviving files are still planned");
        assert!(plan.warnings.iter().any(|w| matches!(w, Warning::Missing { .. })));
    }

    #[test]
    fn an_empty_selection_plans_to_nothing() {
        let (_dir, trash, _) = library();
        let plan = trash.plan(&[], &HashMap::new(), 1_700_000_000).unwrap();
        assert!(plan.is_empty());
        assert_eq!(plan.total_bytes(), 0);
    }

    // ---------------------------------------------------------------------
    // Commit
    // ---------------------------------------------------------------------
    #[test]
    fn committing_moves_every_file_and_records_the_operation() {
        let (_dir, trash, files) = library();
        let plan = trash.plan(&files, &hashes(&files), 1_700_000_000).unwrap();
        let receipt = trash.commit(&plan, "user rejected", 1_700_000_000).unwrap();

        assert_eq!(receipt.moved, 3);
        assert!(receipt.bytes > 0);
        for f in &files {
            assert!(!f.exists(), "{} should have moved", f.display());
        }

        let manifest = trash.manifest().unwrap();
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].op_id, plan.op_id);
        assert_eq!(manifest[0].action, "trash");
        assert_eq!(manifest[0].reason, "user rejected");
        assert_eq!(manifest[0].files.len(), 3);
    }

    #[test]
    fn the_manifest_is_on_disk_before_any_file_moves() {
        // The ordering this whole design rests on. Asserted by checking the manifest
        // during the operation is impossible from outside, so instead: a plan whose move
        // fails must still have left a record, which is what makes the failure
        // recoverable.
        let (_dir, trash, files) = library();
        let mut plan = trash.plan(&files, &hashes(&files), 1_700_000_000).unwrap();

        // Make the last move impossible by pointing it at a directory that cannot be
        // created: a path whose parent is an existing *file*.
        let blocker = trash.dir().join("2023-11-14/shoot/IMG_0001.JPG");
        fs::create_dir_all(blocker.parent().unwrap()).unwrap();
        fs::write(&blocker, b"in the way").unwrap();
        plan.moves[2].destination = blocker.join("nested/deeper.jpg");

        let result = trash.commit(&plan, "test", 1_700_000_000);
        assert!(result.is_err(), "the impossible move must fail the operation");

        // The record survives even though the operation failed. That is what makes a
        // half-finished operation recoverable rather than mysterious.
        assert_eq!(
            trash.manifest().unwrap().len(),
            1,
            "the manifest must have been written before the moves were attempted"
        );
    }

    #[test]
    fn a_changed_file_aborts_the_whole_operation() {
        // The catalogue's hash was computed at index time. In between, something rewrote
        // the file. Moving it would be moving something the user was not shown.
        let (_dir, trash, files) = library();
        let plan = trash.plan(&files, &hashes(&files), 1_700_000_000).unwrap();

        fs::write(&files[1], b"rewritten by another application").unwrap();

        match trash.commit(&plan, "test", 1_700_000_000) {
            Err(TrashError::Changed { path, .. }) => assert_eq!(path, files[1]),
            other => panic!("expected Changed, got {other:?}"),
        }

        // And nothing moved — not even the files that were fine.
        for f in &files {
            assert!(f.is_file(), "{} must not have moved", f.display());
        }
    }

    #[test]
    fn a_failed_operation_rolls_back_the_moves_it_already_made() {
        // Half a pair in the trash is the orphaned-half problem the product exists to
        // solve, created by the product.
        let (_dir, trash, files) = library();
        let mut plan = trash.plan(&files, &hashes(&files), 1_700_000_000).unwrap();

        // The third destination's parent is a file, so creating it fails after two moves.
        let blocker = trash.dir().join("2023-11-14/shoot/IMG_0001.JPG");
        fs::create_dir_all(blocker.parent().unwrap()).unwrap();
        fs::write(&blocker, b"in the way").unwrap();
        plan.moves[2].destination = blocker.join("nested/deeper.jpg");

        assert!(trash.commit(&plan, "test", 1_700_000_000).is_err());

        // Everything is back where it started.
        for f in &files {
            assert!(f.is_file(), "{} must have been rolled back", f.display());
        }
    }

    #[test]
    fn committing_an_empty_plan_does_nothing_and_records_nothing() {
        let (_dir, trash, _) = library();
        let plan = trash.plan(&[], &HashMap::new(), 1_700_000_000).unwrap();
        let receipt = trash.commit(&plan, "test", 1_700_000_000).unwrap();
        assert_eq!(receipt.moved, 0);
        assert!(trash.manifest().unwrap().is_empty());
    }

    #[test]
    fn a_pair_moves_together_or_not_at_all() {
        // The product's headline promise, as a test.
        let (_dir, trash, files) = library();
        let pair = &files[..2]; // the CR3 and the JPG
        let h = hashes(&files);

        let plan = trash.plan(pair, &h, 1_700_000_000).unwrap();
        assert_eq!(plan.moves.len(), 2);
        trash.commit(&plan, "test", 1_700_000_000).unwrap();

        assert!(!files[0].exists() && !files[1].exists(), "both halves must move");
        assert!(files[2].exists(), "the sidecar was not selected and must stay");
    }

    // ---------------------------------------------------------------------
    // Restore
    // ---------------------------------------------------------------------
    #[test]
    fn restoring_puts_everything_back_byte_for_byte() {
        let (_dir, trash, files) = library();
        let before: Vec<Vec<u8>> = files.iter().map(|f| fs::read(f).unwrap()).collect();

        let plan = trash.plan(&files, &hashes(&files), 1_700_000_000).unwrap();
        trash.commit(&plan, "test", 1_700_000_000).unwrap();

        let report = trash.restore(&plan.op_id).unwrap();
        assert_eq!(report.restored, 3);
        assert!(report.missing.is_empty());

        for (f, b) in files.iter().zip(&before) {
            assert!(f.is_file());
            assert_eq!(&fs::read(f).unwrap(), b, "{} came back different", f.display());
        }
    }

    #[test]
    fn restoring_an_unknown_operation_is_an_error_not_a_silent_no_op() {
        let (_dir, trash, _) = library();
        match trash.restore("no-such-operation") {
            Err(TrashError::UnknownOperation(id)) => assert_eq!(id, "no-such-operation"),
            other => panic!("expected UnknownOperation, got {other:?}"),
        }
    }

    #[test]
    fn restoring_never_overwrites_something_already_there() {
        // The file at the source is what the user has now. Replacing it because a restore
        // was requested would be its own data loss.
        let (_dir, trash, files) = library();
        let plan = trash.plan(&files, &hashes(&files), 1_700_000_000).unwrap();
        trash.commit(&plan, "test", 1_700_000_000).unwrap();

        // Something reappears at the original path.
        fs::write(&files[0], b"a different photograph entirely").unwrap();

        let report = trash.restore(&plan.op_id).unwrap();
        assert_eq!(report.restored, 2, "the two unobstructed files come back");
        assert!(
            report.missing.contains(&files[0]),
            "the obstructed one must be reported, not overwritten"
        );
        assert_eq!(
            fs::read(&files[0]).unwrap(),
            b"a different photograph entirely",
            "and the file that was there must be untouched"
        );
    }

    #[test]
    fn restoring_detects_a_trashed_file_whose_contents_changed() {
        let (_dir, trash, files) = library();
        let plan = trash.plan(&files, &hashes(&files), 1_700_000_000).unwrap();
        trash.commit(&plan, "test", 1_700_000_000).unwrap();

        // Something edits a file while it is in the trash.
        fs::write(&plan.moves[0].destination, b"edited in the trash").unwrap();

        match trash.restore(&plan.op_id) {
            Err(TrashError::Changed { path, .. }) => {
                assert_eq!(path, plan.moves[0].destination);
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------
    // Purge
    // ---------------------------------------------------------------------
    #[test]
    fn purging_removes_the_files_and_leaves_a_record() {
        let (_dir, trash, files) = library();
        let plan = trash.plan(&files, &hashes(&files), 1_700_000_000).unwrap();
        trash.commit(&plan, "test", 1_700_000_000).unwrap();

        let receipt = trash.purge(std::slice::from_ref(&plan.op_id)).unwrap();
        assert_eq!(receipt.operations, 1);
        assert_eq!(receipt.removed, 3);
        assert!(receipt.bytes > 0);

        for m in &plan.moves {
            assert!(!m.destination.exists(), "purged files must be gone");
        }

        // The record survives the purge. "What did I delete, and when" must remain
        // answerable after the bytes are gone.
        let manifest = trash.manifest().unwrap();
        assert!(manifest.iter().any(|e| e.op_id == plan.op_id));
        assert!(manifest.iter().any(|e| e.action == "purge"));
    }

    #[test]
    fn purging_an_unknown_operation_is_an_error() {
        let (_dir, trash, _) = library();
        assert!(matches!(
            trash.purge(&["nope".to_string()]),
            Err(TrashError::UnknownOperation(_))
        ));
    }

    #[test]
    fn purging_one_operation_leaves_another_alone() {
        // Purge takes explicit ids rather than "everything", because a purge that decides
        // for itself what to remove is a purge that can be wrong about it.
        let (_dir, trash, files) = library();
        let h = hashes(&files);

        let a = trash.plan(&files[..1], &h, 1_700_000_000).unwrap();
        trash.commit(&a, "first", 1_700_000_000).unwrap();
        let b = trash.plan(&files[1..], &h, 1_700_000_100).unwrap();
        trash.commit(&b, "second", 1_700_000_100).unwrap();

        trash.purge(std::slice::from_ref(&a.op_id)).unwrap();

        for m in &a.moves {
            assert!(!m.destination.exists());
        }
        for m in &b.moves {
            assert!(m.destination.exists(), "the other operation must be untouched");
        }
        // And the other operation is still restorable.
        assert_eq!(trash.restore(&b.op_id).unwrap().restored, 2);
    }

    // ---------------------------------------------------------------------
    // Dates
    // ---------------------------------------------------------------------
    #[test]
    fn the_dated_folder_name_is_correct_for_known_instants() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(1_700_000_000), "2023-11-14");
        assert_eq!(civil_date(946_684_800), "2000-01-01");
        assert_eq!(civil_date(1_709_164_800), "2024-02-29");
        // The day before the epoch, which is where a naive floor-division goes wrong.
        assert_eq!(civil_date(-1), "1969-12-31");
        assert_eq!(civil_date(-86_400), "1969-12-31");
        assert_eq!(civil_date(-86_401), "1969-12-30");
    }

    #[test]
    fn the_whole_cycle_is_reversible() {
        // Trash, restore, trash again, purge. The full lifecycle in one test, because the
        // interesting failures live in the transitions.
        let (_dir, trash, files) = library();
        let before: Vec<Vec<u8>> = files.iter().map(|f| fs::read(f).unwrap()).collect();
        let h = hashes(&files);

        let first = trash.plan(&files, &h, 1_700_000_000).unwrap();
        trash.commit(&first, "cull", 1_700_000_000).unwrap();
        assert_eq!(trash.restore(&first.op_id).unwrap().restored, 3);
        for (f, b) in files.iter().zip(&before) {
            assert_eq!(&fs::read(f).unwrap(), b);
        }

        let second = trash.plan(&files, &h, 1_700_000_000).unwrap();
        trash.commit(&second, "cull again", 1_700_000_000).unwrap();
        trash.purge(std::slice::from_ref(&second.op_id)).unwrap();
        for f in &files {
            assert!(!f.exists());
        }
    }
}
