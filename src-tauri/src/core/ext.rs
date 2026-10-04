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
    "3fr", "arw", "cr2", "cr3", "crw", "dcr", "dng", "erf", "iiq", "kdc", "mef", "mos",
    "mrw", "nef", "nrw", "orf", "pef", "raf", "raw", "rw2", "rwl", "sr2", "srf", "srw",
    "x3f",
];

/// Rendered-image extensions. One of these plus a raw makes a pair.
pub const RASTER_EXTS: &[&str] = &[
    "avif", "heic", "heif", "jpe", "jpeg", "jpg", "png", "tif", "tiff", "webp",
];

/// Metadata extensions that travel with their parent photograph.
pub const SIDECAR_EXTS: &[&str] = &["aap", "acr", "thm", "xmp"];

/// Video containers. Indexed, never paired with a still.
pub const VIDEO_EXTS: &[&str] = &["avi", "m4v", "mkv", "mov", "mp4"];

/// Lowercased extension, or `None` when the path has no extension or it is not UTF-8.
pub fn extension_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Classify a path by extension.
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
