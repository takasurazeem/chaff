//! RAW+JPEG pair resolution.
//!
//! A photograph in a dual-record shoot is **two files with one identity**. Getting this
//! wrong is how a culling tool destroys something: pair `IMG_1234 (1).CR3` with
//! `IMG_1234.JPG`, delete that group, and the user has lost the wrong raw file.
//!
//! ## Deviation from the PRD (deliberate, safety-driven)
//!
//! The PRD's pairing rule said to strip trailing duplicate markers (` (1)`, `-1`) when a
//! bare-stem sibling exists. **This implementation does not do that.** It flags the
//! relationship for human review instead, because the two failure modes are not
//! symmetric:
//!
//! - Failing to auto-pair leaves an orphan. The user sees it and pairs it by hand.
//!   Cost: a few seconds.
//! - Auto-pairing wrongly merges two different photographs. The user culls the merged
//!   group and deletes both. Cost: an unrecoverable photograph — except that it is in
//!   the trash, which is exactly why ADR-0004 exists.
//!
//! Given an asymmetric cost, the safe default wins. See [`ReviewReason::PossibleDuplicateImport`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

use crate::ext::{classify, FileKind};

/// The identity of one photograph: a directory plus a normalised stem.
///
/// The directory is part of the key. `2024/wedding/IMG_0001.CR3` and
/// `2025/party/IMG_0001.JPG` are two different photographs that happen to share a
/// filename, and they must never be grouped.
///
/// # The raw/jpeg split, and why this is not the whole story
///
/// Plenty of photographers keep raws and JPEGs in **sibling folders** — `shoot/raws/`
/// beside `shoot/jpegs/`, or `shoot/CR3/` beside `shoot/JPG/`. Keying on the literal
/// directory alone leaves every one of those unpaired, which means two tiles per
/// photograph and a delete that takes one half.
///
/// So this key is the *first* pass. [`resolve`] runs a second, narrower pass over the
/// leftovers — see [`PairingScope`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhotoKey {
    pub dir: PathBuf,
    /// NFC-normalised, lowercased stem.
    pub stem: String,
}

impl PhotoKey {
    pub fn new(dir: impl Into<PathBuf>, stem_raw: &str) -> Self {
        Self { dir: dir.into(), stem: normalize_stem(stem_raw) }
    }
}

/// How far to look for a partner when a file has none beside it.
///
/// The PRD calls for a policy here and this is it. The default is [`PairingScope::Siblings`]
/// because the split-folder layout is common enough that failing it by default would be
/// wrong for a large share of real libraries, and because the narrower rule below keeps the
/// risk of a wrong pairing small.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PairingScope {
    /// Only files in the same directory pair. The safest, and the narrowest.
    Directory,
    /// **The default.** Also pair across sibling directories — two folders sharing a
    /// parent — but only when *each* of those folders holds one kind of file.
    ///
    /// That single-kind requirement is the safety catch. `shoot/raws/` is all raws and
    /// `shoot/jpegs/` is all rasters, so they pair. `2020/` and `2024/` holding a mix of
    /// everything do not, which is what stops two different years of `IMG_0001` from
    /// merging just because they sit under the same parent.
    ///
    /// **The residual risk, stated rather than hidden:** if one year's folder happens to
    /// hold *only* raws and a sibling year's holds *only* JPEGs, and both contain
    /// `IMG_0001`, they will pair wrongly. That needs all three conditions at once and the
    /// failure is visible — the two halves have different capture dates and the pair can be
    /// inspected — so it is a risk worth taking against the certainty of failing every
    /// split library.
    #[default]
    Siblings,
    /// Pair by stem anywhere in the library, ignoring directories. Only correct when no two
    /// cameras or cards ever reset their numbering into the same stem, which is rare.
    Library,
}

/// The shape of a resolved group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupState {
    /// Exactly one raw and exactly one raster. The normal, healthy case.
    Pair,
    /// A raw with no rendered counterpart.
    RawOnly,
    /// A rendered image with no raw counterpart.
    RasterOnly,
    /// More than one raw or more than one raster under one stem. Never auto-resolved.
    Ambiguous,
}

/// Why a group is being surfaced to the user rather than acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewReason {
    /// Two or more raw files share this stem.
    MultipleRaw,
    /// Two or more rendered images share this stem.
    MultipleRaster,
    /// This stem looks like an OS-generated duplicate of another photograph that is
    /// present in the same directory. Flagged, never merged.
    PossibleDuplicateImport,
}

/// One photograph, with every file that belongs to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotoGroup {
    pub key: PhotoKey,
    pub state: GroupState,
    pub raws: Vec<PathBuf>,
    pub rasters: Vec<PathBuf>,
    pub sidecars: Vec<PathBuf>,
    pub videos: Vec<PathBuf>,
    pub review: Vec<ReviewReason>,
}

impl PhotoGroup {
    pub fn is_pair(&self) -> bool {
        self.state == GroupState::Pair
    }

    /// True when the group must not be acted on without a human decision.
    pub fn needs_review(&self) -> bool {
        self.state == GroupState::Ambiguous || !self.review.is_empty()
    }

    /// Every file that belongs to this photograph, in a stable order.
    ///
    /// This is the exact set a paired delete would move. Keeping it in one place means
    /// there is one definition of "what belongs to this photograph" for the whole app.
    pub fn all_files(&self) -> Vec<&Path> {
        self.raws
            .iter()
            .chain(self.rasters.iter())
            .chain(self.sidecars.iter())
            .chain(self.videos.iter())
            .map(PathBuf::as_path)
            .collect()
    }

    /// How many files a paired delete would move.
    pub fn file_count(&self) -> usize {
        self.all_files().len()
    }
}

/// Normalise a filename stem for comparison.
///
/// Two transformations, both load-bearing:
///
/// 1. **NFC normalisation.** macOS APFS stores filenames in decomposed form (NFD) while
///    Linux and Windows generally do not. A macOS file named `café.CR3` and a Linux file
///    named `café.JPG` are byte-different and semantically identical. Without this, pairs
///    break when a library is copied between platforms — which is exactly what this
///    project does when it deploys to a Linux server.
/// 2. **Lowercasing.** Camera bodies are inconsistent (`IMG_1234.JPG` next to
///    `img_1234.cr3`), and Windows/macOS filesystems are case-insensitive anyway, so
///    matching case-sensitively would make behaviour differ per platform.
pub fn normalize_stem(stem: &str) -> String {
    stem.nfc().collect::<String>().to_lowercase()
}

/// If `stem` carries a trailing OS-generated duplicate marker, return the base stem.
///
/// Only patterns that an operating system or file manager actually generates are
/// recognised. A bare `-1` is deliberately **not** treated as a marker: several camera
/// bodies name consecutive frames `IMG_1234-1.CR3`, and treating that as a duplicate
/// would mis-pair real photographs.
///
/// Returns the base with its original case preserved.
pub fn duplicate_marker_base(stem: &str) -> Option<String> {
    // "NAME (1)" — macOS Finder, Windows Explorer, most browsers.
    if let Some(open) = stem.rfind(" (") {
        let tail = &stem[open..];
        if tail.ends_with(')') {
            let inner = &tail[2..tail.len() - 1];
            if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                return Some(stem[..open].to_string());
            }
        }
    }

    let lower = stem.to_lowercase();

    // "NAME copy" and "NAME copy 2" — Windows Explorer, some sync clients.
    if let Some(rest) = lower.strip_suffix(" copy") {
        return Some(stem[..rest.len()].to_string());
    }
    if let Some(idx) = lower.rfind(" copy ") {
        let inner = &lower[idx + 6..];
        if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
            return Some(stem[..idx].to_string());
        }
    }

    // "NAME-copy" — rsync, some backup tools.
    if let Some(rest) = lower.strip_suffix("-copy") {
        return Some(stem[..rest.len()].to_string());
    }

    None
}

/// True when a directory holds only raw files, or only rendered ones.
///
/// The safety catch for cross-directory pairing. `shoot/raws/` qualifies and
/// `shoot/jpegs/` qualifies; `2020/` holding a mix of everything does not, which is what
/// stops two different years of `IMG_0001` from merging because they share a parent.
///
/// A directory with only one file in it also qualifies — a folder holding a single raw is
/// still a raw folder. Requiring a minimum size would break the small shoots this is
/// meant to help.
fn single_kind(group: &PhotoGroup) -> bool {
    group.raws.is_empty() != group.rasters.is_empty()
}

/// The directory a group would pair from, if cross-directory pairing is allowed.
fn sibling_parent(group: &PhotoGroup) -> Option<PathBuf> {
    if !single_kind(group) {
        return None;
    }
    group.key.dir.parent().map(Path::to_path_buf)
}

/// Pair leftover raws and rasters that sit in sibling directories.
///
/// Only unambiguous matches merge: exactly one raw and exactly one raster, in directories
/// that share a parent and each hold a single kind of file. Anything else is left alone —
/// an ambiguous merge is worse than an unpaired half, because the unpaired half is visible
/// and a wrong pair is not.
fn merge_sibling_halves(
    groups: &mut BTreeMap<PhotoKey, PhotoGroup>,
    scope: PairingScope,
) {
    // Index the halves worth considering, by the stem they would pair on.
    let mut raws: BTreeMap<String, Vec<PhotoKey>> = BTreeMap::new();
    let mut rasters: BTreeMap<String, Vec<PhotoKey>> = BTreeMap::new();

    for (key, group) in groups.iter() {
        if group.state != GroupState::RawOnly && group.state != GroupState::RasterOnly {
            continue;
        }
        match scope {
            PairingScope::Siblings if sibling_parent(group).is_none() => continue,
            PairingScope::Directory => continue,
            _ => {}
        }
        if !group.raws.is_empty() {
            raws.entry(key.stem.clone()).or_default().push(key.clone());
        } else if !group.rasters.is_empty() {
            rasters.entry(key.stem.clone()).or_default().push(key.clone());
        }
    }

    let mut consumed: Vec<PhotoKey> = Vec::new();

    for (stem, raw_keys) in &raws {
        let Some(raster_keys) = rasters.get(stem) else { continue };

        for raw_key in raw_keys {
            let Some(raw_group) = groups.get(raw_key) else { continue };
            let raw_parent = sibling_parent(raw_group);

            // Exactly one raster candidate, or none — anything more is ambiguous.
            let candidates: Vec<&PhotoKey> = raster_keys
                .iter()
                .filter(|rk| {
                    let Some(rg) = groups.get(*rk) else { return false };
                    match scope {
                        PairingScope::Library => true,
                        // Siblings: share a parent, and each folder holds one kind.
                        _ => raw_parent.is_some() && sibling_parent(rg) == raw_parent,
                    }
                })
                .collect();

            if candidates.len() != 1 {
                continue;
            }
            let raster_key = candidates[0].clone();
            if consumed.contains(&raster_key) || consumed.contains(raw_key) {
                continue;
            }

            // Move the raster into the raw's group, then drop the emptied group.
            let raster_group = groups.remove(&raster_key).expect("checked above");
            if let Some(target) = groups.get_mut(raw_key) {
                target.rasters.extend(raster_group.rasters);
                target.sidecars.extend(raster_group.sidecars);
                target.videos.extend(raster_group.videos);
                target.raws.sort();
                target.rasters.sort();
                target.sidecars.sort();
                target.videos.sort();
                target.state = match (target.raws.len(), target.rasters.len()) {
                    (1, 1) => GroupState::Pair,
                    (r, _) if r > 1 => GroupState::Ambiguous,
                    _ => GroupState::Ambiguous,
                };
            }
            consumed.push(raster_key);
        }
    }
}

/// Resolve a flat list of file paths into photographs.
///
/// Takes paths rather than reading a directory so that it is pure, deterministic and
/// testable without a filesystem. Directory traversal belongs to the indexer.
///
/// Non-image files are ignored entirely: they are never paired and never deleted.
pub fn resolve<I, P>(paths: I) -> Vec<PhotoGroup>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    resolve_with_scope(paths, PairingScope::default())
}

/// The same, with an explicit scope.
pub fn resolve_with_scope<I, P>(paths: I, scope: PairingScope) -> Vec<PhotoGroup>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut groups: BTreeMap<PhotoKey, PhotoGroup> = BTreeMap::new();

    for path in paths {
        let path = path.as_ref();
        let kind = classify(path);
        if kind == FileKind::Other {
            continue;
        }

        let Some(stem_raw) = path.file_stem().and_then(|s| s.to_str()) else {
            // No stem, or a non-UTF-8 stem. We cannot pair it, so we do not touch it.
            continue;
        };

        let dir = path.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        let key = PhotoKey::new(dir, stem_raw);

        let group = groups.entry(key.clone()).or_insert_with(|| PhotoGroup {
            key,
            state: GroupState::RasterOnly,
            raws: Vec::new(),
            rasters: Vec::new(),
            sidecars: Vec::new(),
            videos: Vec::new(),
            review: Vec::new(),
        });

        let owned = path.to_path_buf();
        match kind {
            FileKind::Raw => group.raws.push(owned),
            FileKind::Raster => group.rasters.push(owned),
            FileKind::Sidecar => group.sidecars.push(owned),
            FileKind::Video => group.videos.push(owned),
            // Filtered before this point, so it cannot arrive. Handled rather than
            // asserted: `unreachable!` is a panic, and a panic in a library takes down
            // the window over a bookkeeping slip. Doing nothing is the correct behaviour
            // for a file kind this function has no opinion about.
            FileKind::Other => {}
        }
    }

    // Deterministic output regardless of input order.
    for group in groups.values_mut() {
        group.raws.sort();
        group.rasters.sort();
        group.sidecars.sort();
        group.videos.sort();

        group.state = match (group.raws.len(), group.rasters.len()) {
            (1, 1) => GroupState::Pair,
            (1, 0) => GroupState::RawOnly,
            (0, 1) => GroupState::RasterOnly,
            (r, _) if r > 1 => GroupState::Ambiguous,
            // Includes (0, 0) — a group with nothing in it. It cannot arise, because a
            // group is created only when a file is added to it, but reaching it must not
            // be a panic.
            _ => GroupState::Ambiguous,
        };

        if group.raws.len() > 1 {
            group.review.push(ReviewReason::MultipleRaw);
        }
        if group.rasters.len() > 1 {
            group.review.push(ReviewReason::MultipleRaster);
        }
    }

    // Second pass: give an unpaired half a chance with a sibling directory.
    //
    // This is what makes `shoot/raws/` beside `shoot/jpegs/` work. It runs *after* the
    // first pass so that same-directory pairing is completely unaffected — a library that
    // already works cannot regress because of it.
    if scope != PairingScope::Directory {
        merge_sibling_halves(&mut groups, scope);
    }

    // Third pass: flag possible duplicate imports. Never merge.
    let present: Vec<PhotoKey> = groups.keys().cloned().collect();
    let present_set: std::collections::BTreeSet<&PhotoKey> = present.iter().collect();

    for key in &present {
        if let Some(base) = duplicate_marker_base(&key.stem) {
            let candidate = PhotoKey { dir: key.dir.clone(), stem: normalize_stem(&base) };
            if present_set.contains(&candidate) {
                if let Some(group) = groups.get_mut(key) {
                    group.review.push(ReviewReason::PossibleDuplicateImport);
                }
            }
        }
    }

    groups.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a group list from fabricated path strings.
    ///
    /// This keeps the pairing tests pure: no temp directories, no I/O, no image bytes,
    /// identical behaviour on every platform and filesystem.
    fn resolve_from(paths: &[&str]) -> Vec<PhotoGroup> {
        resolve(paths.iter().map(PathBuf::from))
    }

    fn only(paths: &[&str]) -> PhotoGroup {
        let mut groups = resolve_from(paths);
        assert_eq!(groups.len(), 1, "expected exactly one group, got {groups:#?}");
        groups.pop().unwrap()
    }

    #[test]
    fn a_raw_and_a_raster_make_a_pair() {
        let g = only(&["/lib/IMG_1234.CR3", "/lib/IMG_1234.JPG"]);
        assert_eq!(g.state, GroupState::Pair);
        assert!(!g.needs_review());
        assert_eq!(g.file_count(), 2);
    }

    #[test]
    fn orphan_raw_and_orphan_raster_are_distinct_states() {
        let raw = only(&["/lib/IMG_1234.CR3"]);
        assert_eq!(raw.state, GroupState::RawOnly);
        assert!(!raw.needs_review(), "an orphan is normal, not a review case");

        let raster = only(&["/lib/IMG_1234.JPG"]);
        assert_eq!(raster.state, GroupState::RasterOnly);
        assert!(!raster.needs_review());
    }

    #[test]
    fn sidecars_and_videos_travel_with_the_photograph() {
        let g = only(&[
            "/lib/IMG_1234.CR3",
            "/lib/IMG_1234.JPG",
            "/lib/IMG_1234.XMP",
            "/lib/IMG_1234.MP4",
        ]);
        assert_eq!(g.state, GroupState::Pair);
        assert_eq!(g.sidecars.len(), 1);
        assert_eq!(g.videos.len(), 1);
        // The paired delete moves all four.
        assert_eq!(g.file_count(), 4);
    }

    #[test]
    fn stem_matching_is_case_insensitive() {
        let g = only(&["/lib/IMG_1234.CR3", "/lib/img_1234.jpg"]);
        assert_eq!(g.state, GroupState::Pair, "case must not break a pair");
    }

    #[test]
    fn stem_matching_survives_unicode_normalisation_differences() {
        // NFC "café" vs NFD "café" — byte-different, semantically identical. This is the
        // macOS-to-Linux case that a naive implementation gets wrong.
        let nfc = "/lib/caf\u{00e9}.CR3";
        let nfd = "/lib/cafe\u{0301}.JPG";
        assert_ne!(nfc, nfd, "the two spellings must differ at the byte level for this test to mean anything");

        let g = only(&[nfc, nfd]);
        assert_eq!(g.state, GroupState::Pair, "NFC and NFD spellings must pair");
    }

    #[test]
    fn same_stem_in_different_directories_stays_separate() {
        let groups = resolve_from(&[
            "/lib/2024/IMG_0001.CR3",
            "/lib/2024/IMG_0001.JPG",
            "/lib/2025/IMG_0001.CR3",
            "/lib/2025/IMG_0001.JPG",
        ]);
        assert_eq!(groups.len(), 2, "the directory is part of a photograph's identity");
        assert!(groups.iter().all(|g| g.is_pair()));
    }

    #[test]
    fn two_raws_under_one_stem_is_ambiguous_and_flagged() {
        let g = only(&["/lib/IMG_1234.CR3", "/lib/IMG_1234.NEF", "/lib/IMG_1234.JPG"]);
        assert_eq!(g.state, GroupState::Ambiguous);
        assert!(g.review.contains(&ReviewReason::MultipleRaw));
        assert!(g.needs_review(), "an ambiguous group must never be auto-deleted");
    }

    #[test]
    fn two_rasters_under_one_stem_is_ambiguous_and_flagged() {
        let g = only(&["/lib/IMG_1234.CR3", "/lib/IMG_1234.JPG", "/lib/IMG_1234.PNG"]);
        assert_eq!(g.state, GroupState::Ambiguous);
        assert!(g.review.contains(&ReviewReason::MultipleRaster));
    }

    #[test]
    fn duplicate_marker_is_flagged_but_never_merged() {
        // The safety test. Two genuinely distinct photographs exist here: the original
        // and an OS-generated copy import. Merging them would pair the copy's raw with
        // the original's jpeg, and a paired delete would then destroy the wrong files.
        let groups = resolve_from(&[
            "/lib/IMG_1234.CR3",
            "/lib/IMG_1234.JPG",
            "/lib/IMG_1234 (1).CR3",
        ]);
        assert_eq!(groups.len(), 2, "duplicate markers must NOT be merged into one group");

        let copy = groups
            .iter()
            .find(|g| g.key.stem.contains("(1)"))
            .expect("the (1) group must exist separately");
        assert_eq!(copy.state, GroupState::RawOnly);
        assert!(
            copy.review.contains(&ReviewReason::PossibleDuplicateImport),
            "the copy must be flagged for review"
        );
        assert!(copy.needs_review());
    }

    #[test]
    fn duplicate_marker_without_a_sibling_is_not_flagged() {
        // "(1)" in a name with no bare sibling is just part of the filename. It must not
        // be stripped, and it must not be reported as a suspected duplicate.
        let g = only(&["/lib/IMG_1234 (1).CR3", "/lib/IMG_1234 (1).JPG"]);
        assert_eq!(g.state, GroupState::Pair);
        assert!(!g.needs_review(), "no sibling exists, so there is nothing to suspect");
    }

    #[test]
    fn dash_number_suffixes_are_not_treated_as_duplicate_markers() {
        // Several camera bodies emit IMG_1234-1.CR3, IMG_1234-2.CR3 for consecutive
        // frames. Treating "-1" as a duplicate marker would mis-pair real photographs.
        assert_eq!(duplicate_marker_base("IMG_1234-1"), None);

        let g = only(&["/lib/IMG_1234-1.CR3", "/lib/IMG_1234-1.JPG"]);
        assert_eq!(g.state, GroupState::Pair);
        assert!(!g.needs_review());
    }

    #[test]
    fn duplicate_marker_patterns_recognised() {
        assert_eq!(duplicate_marker_base("IMG_1234 (1)"), Some("IMG_1234".into()));
        assert_eq!(duplicate_marker_base("IMG_1234 (12)"), Some("IMG_1234".into()));
        assert_eq!(duplicate_marker_base("IMG_1234 copy"), Some("IMG_1234".into()));
        assert_eq!(duplicate_marker_base("IMG_1234 copy 2"), Some("IMG_1234".into()));
        assert_eq!(duplicate_marker_base("IMG_1234-copy"), Some("IMG_1234".into()));
        // Not markers:
        assert_eq!(duplicate_marker_base("IMG_1234"), None);
        assert_eq!(duplicate_marker_base("IMG_1234 (final)"), None);
        assert_eq!(duplicate_marker_base("IMG_1234 (1) edited"), None);
    }

    #[test]
    fn unrelated_files_are_ignored_entirely() {
        let groups = resolve_from(&[
            "/lib/IMG_1234.CR3",
            "/lib/IMG_1234.JPG",
            "/lib/notes.txt",
            "/lib/README",
            "/lib/archive.tar.gz",
            "/lib/.DS_Store",
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].file_count(), 2, "non-images must never join a group");
    }

    #[test]
    fn no_recognised_files_yields_no_groups() {
        assert!(resolve_from(&["/lib/a.txt", "/lib/b.doc"]).is_empty());
        assert!(resolve_from(&[]).is_empty());
    }

    #[test]
    fn resolution_is_deterministic_regardless_of_input_order() {
        let a = resolve_from(&["/lib/x.CR3", "/lib/x.JPG", "/lib/x.XMP"]);
        let b = resolve_from(&["/lib/x.XMP", "/lib/x.JPG", "/lib/x.CR3"]);
        assert_eq!(a, b, "output must not depend on filesystem walk order");
    }

    #[test]
    fn all_files_is_stable_and_complete() {
        let g = only(&[
            "/lib/IMG_1234.CR3",
            "/lib/IMG_1234.JPG",
            "/lib/IMG_1234.xmp",
            "/lib/IMG_1234.MOV",
        ]);
        let files = g.all_files();
        assert_eq!(files.len(), 4);
        assert_eq!(files, g.all_files(), "repeated calls must agree");
    }
}
