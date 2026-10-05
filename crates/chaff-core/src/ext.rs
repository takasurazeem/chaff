//! File-kind classification by extension.
//!
//! Pure logic. No I/O, no Tauri. Classification is by extension only — we never open
//! the file here. Anything that needs to read a file lives in the decoder, not here.

use std::path::Path;

/// What a file in a photo library is, as far as culling is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileKind {
    /// A camera raw file. One half of a pair.
    Raw,
    /// A rendered image (JPEG, HEIC, PNG, ...). The other half of a pair.
    Raster,
    /// Metadata that belongs to a photograph (`xmp`, `acr`, ...).
    Sidecar,
    /// A video container. Indexed but not paired.
    Video,
    /// Anything else — ignored, never deleted, never paired.
    Other,
}

/// Camera raw extensions we recognise.
///
/// Deliberately generous: adding an extension here is cheap and a missed raw file is
/// a photograph the user thinks is missing.
pub const RAW_EXTS: &[&str] = &[
    "3fr", "ari", "arq", "arw", "cam", "cr2", "cr3", "crw", "dcr", "dng", "erf", "fff",
    "gpr", "iiq", "kdc", "lri", "mdc", "mef", "mos", "mrw", "nef", "nrw", "orf", "ori",
    "pef", "raf", "raw", "rw2", "rwl", "sr2", "srf", "srw", "sti", "x3f",
];
//
// The list above was found to be incomplete by the real corpus, not by reasoning, and
// the gap was substantial: **nine extensions and 54 files in the CC0 catalogue alone**
// were unrecognised, every one of them a real camera's raw format.
//
//     .ori  19  Olympus ORF variant
//     .gpr  17  GoPro
//     .fff  11  Hasselblad
//     .cam   2  Casio
//     .arq   1  Sony ARQ
//     .ari   1  ARRI
//     .lri   1  Light L16
//     .mdc   1  Minolta
//     .sti   1  Samsung
//
// A missed raw extension is not a cosmetic problem: the file classifies as `Other`,
// never enters a group, never appears in the grid, and the photographer simply never
// sees that photograph. It is the quietest way this product can fail. Synthetic
// fixtures could not have found it — only a real archive could.
//
// `.tif` and `.tiff` are deliberately NOT here even though 20 files in that archive are
// TIFF-wrapped raws (Kodak DCS and similar). The overwhelming majority of TIFF files in
// a hobbyist's library are rendered images — scans, exports, HDR merges — so treating
// the extension as raw would misclassify the common case to serve the rare one. A
// TIFF-wrapped raw is a documented exception rather than a silent one.

/// Rendered-image extensions. One of these plus a raw makes a pair.
pub const RASTER_EXTS: &[&str] = &[
    "avif", "heic", "heif", "jpe", "jpeg", "jpg", "png", "tif", "tiff", "webp",
];

/// Metadata extensions that travel with their parent photograph.
/// Files that belong to a photograph without being one.
///
/// These travel with their parent on a move, and go to the trash with it. A sidecar left
/// behind is an edit stranded from the photograph it describes — recoverable, but only if
/// the user notices.
///
/// The list is deliberately long rather than minimal. Recognising a sidecar costs nothing;
/// failing to recognise one means a delete silently orphans it, and the user finds out
/// months later when they reopen an editor and their adjustments are gone.
///
/// The last four were added after looking at a real library, which held 353 `.rrdata`
/// (RapidRAW), 22 `.pp3` (RawTherapee) and 2 `.arp` files that the first list missed.
pub const SIDECAR_EXTS: &[&str] = &[
    // Adobe and camera makers
    "xmp", "aap", "acr", "thm", // Editors
    "rrdata", // RapidRAW
    "pp3",    // RawTherapee
    "arp",    // ON1 / Artstudio
    "on1",    // ON1 Photo RAW
    "dop",    // DxO PhotoLab
    "cos",    // Capture One
    "dtstyle", // darktable style
];

/// Video containers. Indexed, never paired with a still.
pub const VIDEO_EXTS: &[&str] = &["avi", "m4v", "mkv", "mov", "mp4"];

/// Lowercased extension, or `None` when the path has no extension or it is not UTF-8.
pub fn extension_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Classify a path by extension.
/// Would the catalog index a file with this extension?
///
/// The watcher asks this before caring about an event: a `.txt` appearing in a library is
/// not a reason to re-index anything.
pub fn is_indexable_extension(ext: &str) -> bool {
    let e = ext.trim_start_matches('.').to_ascii_lowercase();
    RAW_EXTS.contains(&e.as_str())
        || RASTER_EXTS.contains(&e.as_str())
        || SIDECAR_EXTS.contains(&e.as_str())
        || VIDEO_EXTS.contains(&e.as_str())
}

pub fn classify(path: &Path) -> FileKind {
    match extension_lower(path).as_deref() {
        None => FileKind::Other,
        Some(ext) => {
            if RAW_EXTS.contains(&ext) {
                FileKind::Raw
            } else if RASTER_EXTS.contains(&ext) {
                FileKind::Raster
            } else if SIDECAR_EXTS.contains(&ext) {
                FileKind::Sidecar
            } else if VIDEO_EXTS.contains(&ext) {
                FileKind::Video
            } else {
                FileKind::Other
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn kind(s: &str) -> FileKind {
        classify(&PathBuf::from(s))
    }

    #[test]
    fn classifies_raw_extensions() {
        for p in [
            "a.CR3", "a.cr2", "a.NEF", "a.arw", "a.RAF", "a.raf", "a.ORF", "a.rw2",
            "a.DNG", "a.pef", "a.3fr", "a.x3f", "a.srw", "a.RWL",
        ] {
            assert_eq!(kind(p), FileKind::Raw, "{p} should be Raw");
        }
    }

    #[test]
    fn recognises_every_raw_extension_the_real_archive_contains() {
        // Found by the corpus, not by reasoning. All nine were missing, covering 54 files
        // in the CC0 catalogue. A missed raw extension makes a photograph invisible:
        // classified as `Other`, never grouped, never shown. See the note on RAW_EXTS.
        for ext in ["ori", "gpr", "fff", "cam", "arq", "ari", "lri", "mdc", "sti"] {
            let path = PathBuf::from(format!("IMG_0001.{ext}"));
            assert_eq!(
                classify(&path),
                FileKind::Raw,
                ".{ext} is a real camera raw format and must classify as Raw"
            );
            assert_eq!(
                classify(&PathBuf::from(format!("IMG_0001.{}", ext.to_uppercase()))),
                FileKind::Raw,
                ".{ext} must classify as Raw in upper case too"
            );
        }
    }

    #[test]
    fn tiff_is_treated_as_a_rendered_image_not_a_raw() {
        // Deliberate, documented exception: 20 files in the CC0 archive are TIFF-wrapped
        // raws, but the overwhelming majority of TIFFs in a hobbyist's library are
        // rendered images. Classifying the extension as raw would misclassify the common
        // case to serve the rare one.
        assert_eq!(kind("scan.tif"), FileKind::Raster);
        assert_eq!(kind("scan.TIFF"), FileKind::Raster);
    }

    #[test]
    fn classifies_raster_extensions() {
        for p in ["a.JPG", "a.jpeg", "a.png", "a.HEIC", "a.tif", "a.tiff", "a.avif", "a.webp"] {
            assert_eq!(kind(p), FileKind::Raster, "{p} should be Raster");
        }
    }

    #[test]
    fn classifies_sidecar_and_video() {
        assert_eq!(kind("a.XMP"), FileKind::Sidecar);
        assert_eq!(kind("a.xmp"), FileKind::Sidecar);
        assert_eq!(kind("a.ACR"), FileKind::Sidecar);
        assert_eq!(kind("a.MP4"), FileKind::Video);
        assert_eq!(kind("a.mov"), FileKind::Video);
    }

    #[test]
    fn mixed_case_extension_is_recognised() {
        // Cameras and OSes disagree about case constantly. This must never matter.
        assert_eq!(kind("IMG_1234.Cr3"), FileKind::Raw);
        assert_eq!(kind("IMG_1234.jPeG"), FileKind::Raster);
    }

    #[test]
    fn unknown_and_extensionless_are_other() {
        assert_eq!(kind("notes.txt"), FileKind::Other);
        assert_eq!(kind("README"), FileKind::Other);
        assert_eq!(kind("archive.tar.gz"), FileKind::Other);
        // A dotfile is not an extension-having image.
        assert_eq!(kind(".hidden"), FileKind::Other);
    }

    #[test]
    fn extension_is_lowercased_not_the_whole_path() {
        // Guards against lowercasing the path, which would break on case-sensitive
        // filesystems whose directory names carry meaning (e.g. a "Raw/" vs "raw/").
        assert_eq!(extension_lower(&PathBuf::from("/Pictures/My Raw/IMG.CR3")).unwrap(), "cr3");
    }
}

#[cfg(test)]
mod sidecar_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn every_sidecar_a_real_library_held_is_recognised() {
        // Taken from an actual library, which had 353 `.rrdata`, 22 `.pp3` and 2 `.arp`
        // files that the first list did not know about. Unrecognised means `Other`, which
        // means skipped — and a skipped sidecar is one orphaned by the next delete.
        for name in [
            "IMG_0001.xmp",
            "IMG_0001.RRDATA",
            "IMG_0001.pp3",
            "IMG_0001.arp",
            "IMG_0001.on1",
            "IMG_0001.dop",
            "IMG_0001.cos",
            "IMG_0001.aap",
            "IMG_0001.acr",
            "IMG_0001.thm",
        ] {
            assert_eq!(
                classify(Path::new(name)),
                FileKind::Sidecar,
                "{name} must travel with its photograph"
            );
        }
    }

    #[test]
    fn a_sidecar_is_never_mistaken_for_a_photograph() {
        // The distinction matters: a photograph is what gets paired, scored and deleted.
        // A sidecar rides along.
        for name in ["a.xmp", "a.rrdata", "a.pp3"] {
            assert_ne!(classify(Path::new(name)), FileKind::Raw);
            assert_ne!(classify(Path::new(name)), FileKind::Raster);
        }
    }

    #[test]
    fn the_sidecar_list_has_no_duplicates_and_no_dots() {
        // A duplicate is harmless; an entry written as ".xmp" would never match, because
        // `classify` compares against the extension without its dot. That is a silent
        // failure, so it is asserted rather than trusted.
        let mut seen = std::collections::BTreeSet::new();
        for ext in SIDECAR_EXTS {
            assert!(!ext.starts_with('.'), "{ext} must not carry its dot");
            assert_eq!(ext.to_lowercase(), *ext, "{ext} must be lowercase");
            assert!(seen.insert(*ext), "{ext} is listed twice");
        }
    }
}
