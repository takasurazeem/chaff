//! Extracting the camera-rendered JPEG that most raw files carry inside them.
//!
//! # Why this is the fast path, not an optimisation
//!
//! A raw file contains two things: the sensor readout, and a JPEG the camera already
//! rendered. Drawing a grid cell from the sensor readout means demosaicing a 45 MP frame
//! — hundreds of milliseconds and hundreds of megabytes — to produce something that will
//! be displayed at 256 pixels. Drawing it from the embedded JPEG means lifting out a
//! ready-made image in a few milliseconds.
//!
//! Measured on the real corpus, **7 of 12 raw files carry one**, and the largest is
//! 1952x552. That is the difference between a grid that appears immediately and one that
//! takes minutes to fill.
//!
//! # How the preview is found
//!
//! By scanning for JPEG markers rather than by parsing each container format. Raw
//! containers are TIFF-based (CR2, NEF, DNG, ARW), ISO-BMFF (CR3), or entirely bespoke
//! (RAF, MRW), and each stores previews in its own place. A JPEG stream is
//! self-describing — `FFD8` then a chain of length-prefixed segments to `FFD9` — so
//! looking for one works across every container without a format-specific parser for
//! each.
//!
//! The risk is a false positive: three bytes in raw sensor data that happen to look like
//! a JPEG start. That is handled by *validating* rather than by hoping — a candidate is
//! only accepted if its segment chain walks cleanly to a start-of-frame marker with
//! plausible dimensions. Random bytes do not produce a well-formed JPEG header.
//!
//! # The caller must validate by decoding
//!
//! **Extraction is a fast path, not a guarantee.** A structurally perfect JPEG header
//! says nothing about the entropy-coded data behind it, and real files contain previews
//! that are truncated or corrupt. The corpus has one: a Blackmagic DNG whose embedded
//! preview has a clean 1952x552 header and entropy data that neither this crate nor
//! Pillow can decode.
//!
//! So this module deliberately does **not** decode. It does no I/O beyond reading the
//! file and pulls in no image decoder, which keeps it fast and keeps the decoder out of
//! the extraction path. The caller decodes, and on failure falls through to a real
//! decode of the raw sensor data — the same path taken when there is no preview at all.
//!
//! Validating here instead would mean decoding a 1952x552 JPEG (~50 ms) inside what is
//! supposed to be the cheap path, to answer a question the caller is about to answer
//! anyway when it resizes.
//!
//! # The thumbnail trap
//!
//! Some containers embed a small thumbnail rather than a usable preview. The corpus
//! contains one at **64x48**. Accepting it would produce a grid of blurry mush and,
//! worse, would look like the extraction working. Previews below a usable size are
//! reported as [`PreviewSource::TooSmall`] so the caller falls through to a real decode.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PreviewError {
    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is {size} bytes, beyond the {limit}-byte scan limit")]
    TooLarge { path: PathBuf, size: u64, limit: u64 },
}

/// Refuse to scan anything larger than this.
///
/// A raw file is tens of megabytes; this is a guard against being pointed at a video or
/// a disk image, where reading the whole thing into memory to look for a JPEG would be
/// both pointless and expensive.
pub const MAX_SCAN_BYTES: u64 = 256 * 1024 * 1024;

/// Default minimum long edge for a preview to be worth using.
///
/// 512 pixels. A grid cell is 256, so anything at or above this can fill a cell and
/// survive a modest zoom. Below it the result is visibly soft and a real decode is
/// better even though it costs far more.
pub const DEFAULT_MIN_LONG_EDGE: u32 = 512;

/// A camera-rendered JPEG lifted out of a raw container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedPreview {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl EmbeddedPreview {
    pub fn long_edge(&self) -> u32 {
        self.width.max(self.height)
    }

    pub fn pixel_count(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

/// What a raw file offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewSource {
    /// A usable camera-rendered JPEG. No decode required.
    Embedded(EmbeddedPreview),
    /// A preview exists but is too small to render from. The caller should decode.
    ///
    /// Distinct from `NeedsDecode` because the reasons differ: here the container has a
    /// preview and it is inadequate, there it has none at all. Collapsing them would hide
    /// which raw formats are worth adding a fast path for.
    TooSmall { width: u32, height: u32 },
    /// No embedded preview. A full decode is required.
    NeedsDecode,
}

// Note: there is no `Unusable` variant, and that is deliberate. Whether a preview's
// entropy data decodes is a question this module cannot answer without a decoder, and
// adding a variant it cannot populate would imply a guarantee it does not make.

impl PreviewSource {
    pub fn embedded(&self) -> Option<&EmbeddedPreview> {
        match self {
            PreviewSource::Embedded(p) => Some(p),
            _ => None,
        }
    }
}

/// Find the largest usable embedded JPEG in `path`.
///
/// Only I/O failures are errors. A file with no preview is a normal, common outcome and
/// is reported as such.
pub fn extract(path: &Path, min_long_edge: u32) -> Result<PreviewSource, PreviewError> {
    let meta = std::fs::metadata(path).map_err(|source| PreviewError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if meta.len() > MAX_SCAN_BYTES {
        return Err(PreviewError::TooLarge {
            path: path.to_path_buf(),
            size: meta.len(),
            limit: MAX_SCAN_BYTES,
        });
    }

    let data = std::fs::read(path).map_err(|source| PreviewError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    Ok(best_preview(&data, min_long_edge))
}

/// Every usable embedded preview, largest first.
///
/// **The caller should try these in order and fall back on a decode failure.** Returning
/// only the best one is not enough, and the corpus shows exactly why: a Canon CR2 holds
/// three embedded JPEGs — a 160x120 thumbnail, a good 1936x1288 preview, and a spurious
/// 1944x1296 stream whose "end of image" lies in the raw sensor data at the very end of
/// the file. The spurious one is marginally *larger*, so a selector ranking purely by
/// pixel count picks the broken stream over the working one.
///
/// No amount of header validation fixes that reliably, because the spurious stream's
/// header is well formed. What fixes it is offering the alternatives and letting the
/// decoder — which is the only thing that can actually tell — reject the bad one.
pub fn candidates(data: &[u8], min_long_edge: u32) -> Vec<EmbeddedPreview> {
    let mut out: Vec<EmbeddedPreview> = find_jpegs(data)
        .into_iter()
        .filter(|span| span.width.max(span.height) >= min_long_edge)
        .map(|span| EmbeddedPreview {
            jpeg: data[span.start..span.end].to_vec(),
            width: span.width,
            height: span.height,
        })
        .collect();

    // Largest first, and by a stable tiebreak so the order never depends on where in the
    // file a stream happened to sit.
    out.sort_by(|a, b| {
        b.pixel_count().cmp(&a.pixel_count()).then(a.jpeg.len().cmp(&b.jpeg.len()))
    });
    out
}

/// The extraction itself, over bytes. Pure, so it can be tested without files.
pub fn best_preview(data: &[u8], min_long_edge: u32) -> PreviewSource {
    let candidates = find_jpegs(data);

    let mut best: Option<EmbeddedPreview> = None;
    let mut largest_rejected: Option<(u32, u32)> = None;

    for span in candidates {
        let (w, h) = (span.width, span.height);

        if w.max(h) < min_long_edge {
            // Keep the largest rejected one so the caller can report *why* it fell
            // through rather than just that it did.
            let better = match largest_rejected {
                Some((pw, ph)) => (w as u64 * h as u64) > (pw as u64 * ph as u64),
                None => true,
            };
            if better {
                largest_rejected = Some((w, h));
            }
            continue;
        }

        let better = match &best {
            Some(b) => (w as u64 * h as u64) > b.pixel_count(),
            None => true,
        };
        if better {
            best = Some(EmbeddedPreview {
                jpeg: data[span.start..span.end].to_vec(),
                width: w,
                height: h,
            });
        }
    }

    match (best, largest_rejected) {
        (Some(p), _) => PreviewSource::Embedded(p),
        (None, Some((w, h))) => PreviewSource::TooSmall { width: w, height: h },
        (None, None) => PreviewSource::NeedsDecode,
    }
}

/// Convenience: the single largest preview, or `None`.
pub fn largest(data: &[u8], min_long_edge: u32) -> Option<EmbeddedPreview> {
    candidates(data, min_long_edge).into_iter().next()
}

/// What a walk of a JPEG stream found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JpegSpan {
    start: usize,
    /// Offset one past the final `FFD9`.
    end: usize,
    width: u32,
    height: u32,
}

/// Byte ranges of every JPEG stream in `data`.
///
/// Each candidate is walked properly to find its true end. **Scanning for the first
/// `FFD9` is not good enough**, and the failure is not exotic: many cameras embed a JPEG
/// thumbnail *inside* the APP1/EXIF segment, so the thumbnail's own `FFD9` appears within
/// the first few kilobytes of the outer stream.
///
/// The real corpus has exactly this. A Raspberry Pi raw declares an APP1 segment of
/// 25,608 bytes, and the first `FFD9` sits at byte 2,640 — inside that payload. A naive
/// scan truncates the stream there, the header walk then fails, and the file is reported
/// as having no preview at all. Silently, and on a large share of real cameras.
fn find_jpegs(data: &[u8]) -> Vec<JpegSpan> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 3 < data.len() {
        match data[i..].windows(3).position(|w| w == [0xFF, 0xD8, 0xFF]) {
            Some(off) => {
                let start = i + off;
                match walk_jpeg(data, start) {
                    Some(span) => {
                        out.push(span);
                        i = span.end.max(start + 3);
                    }
                    None => i = start + 3,
                }
            }
            None => break,
        }
    }
    out
}

/// Walk a JPEG from `start`, returning its true extent and frame dimensions.
///
/// Follows the segment chain — length-prefixed segments until start-of-scan, then
/// entropy-coded data until the real end-of-image, skipping byte stuffing (`FF00`) and
/// restart markers (`FFD0`-`FFD7`).
///
/// Also handles **fill bytes**: the specification permits any number of `FF` bytes before
/// a marker, and some writers emit them.
fn walk_jpeg(data: &[u8], start: usize) -> Option<JpegSpan> {
    if start + 2 > data.len() || data[start] != 0xFF || data[start + 1] != 0xD8 {
        return None;
    }
    let mut i = start + 2;
    let mut dims: Option<(u32, u32)> = None;

    loop {
        if i + 1 >= data.len() {
            return None;
        }
        if data[i] != 0xFF {
            return None; // desynchronised
        }

        // Fill bytes: skip every consecutive 0xFF.
        let mut j = i;
        while j < data.len() && data[j] == 0xFF {
            j += 1;
        }
        if j >= data.len() {
            return None;
        }
        let marker = data[j];
        let after_marker = j + 1;

        if marker == 0xD9 {
            return None; // end of image before any frame header
        }
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i = after_marker;
            continue;
        }

        if after_marker + 2 > data.len() {
            return None;
        }
        let len = u16::from_be_bytes([data[after_marker], data[after_marker + 1]]) as usize;
        if len < 2 {
            return None;
        }

        if marker == 0xDA {
            // Start of scan. Entropy-coded data runs until the real end-of-image.
            if dims.is_none() {
                return None; // no frame header seen, so this is not a usable stream
            }
            let mut k = after_marker + len;
            loop {
                if k + 1 >= data.len() {
                    return None; // ran out before finding an end marker
                }
                if data[k] == 0xFF {
                    let n = data[k + 1];
                    if n == 0xD9 {
                        let (width, height) = dims?;
                        return Some(JpegSpan { start, end: k + 2, width, height });
                    }
                    if n == 0x00 || (0xD0..=0xD7).contains(&n) {
                        k += 2; // stuffed byte or restart marker: still scan data
                        continue;
                    }
                    // Another marker mid-scan (a second scan, or DNL). Continue walking.
                    break;
                }
                k += 1;
            }
            i = k;
            continue;
        }

        let is_sof = (0xC0..=0xCF).contains(&marker)
            && marker != 0xC4 // define Huffman table
            && marker != 0xC8 // JPEG extension
            && marker != 0xCC; // define arithmetic coding table
        if is_sof {
            // FFxx, length(2), precision(1), height(2), width(2), Nf(1), then 3 bytes per
            // component.
            if after_marker + 7 > data.len() {
                return None;
            }
            let h = u16::from_be_bytes([data[after_marker + 3], data[after_marker + 4]]) as u32;
            let w = u16::from_be_bytes([data[after_marker + 5], data[after_marker + 6]]) as u32;
            let nf = data[after_marker + 7] as usize;

            // A frame header must describe 1 to 4 components, and its declared length must
            // match: 8 + 3 per component. Both checks matter, and the component count is
            // the one that caught a real file.
            //
            // The corpus contains a Canon CR2 with three embedded JPEGs. The largest by
            // pixel count — 1944x1296, marginally beating a good 1936x1288 preview — is
            // spurious: its "end of image" is at the very end of the file, in the raw
            // sensor data, and its frame header declares **zero components**. Nothing
            // about its dimensions looked wrong, so a selector that ranked purely by
            // pixel count chose the broken stream over the working one.
            if !(1..=4).contains(&nf) {
                return None;
            }
            let expected_len = 8 + 3 * nf;
            if len != expected_len {
                return None;
            }
            if w == 0 || h == 0 {
                return None;
            }
            dims = Some((w, h));
        }

        i = after_marker + len;
    }
}

/// Frame dimensions of a standalone JPEG buffer, for callers that already hold one.
///
/// Used by tests and by the decode path; the extraction path gets its dimensions from the
/// same walk that finds the stream's extent, so the two can never disagree.
pub fn jpeg_dimensions(seg: &[u8]) -> Option<(u32, u32)> {
    let span = walk_jpeg(seg, 0)?;
    Some((span.width, span.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but structurally complete JPEG: SOI, APP0, SOF0, SOS, data, EOI.
    ///
    /// The SOS segment is not optional. An earlier version of this helper stopped at the
    /// frame header, which no real JPEG does — a decoder reaches the entropy-coded data
    /// through start-of-scan, and a walker that requires it was rejecting every fixture
    /// in this file. The walker was right and the helper was not a JPEG.
    fn tiny_jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8]; // SOI
        // APP0, length 4 (2 for the length field + 2 payload)
        v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        // SOF0. Length 11 = length(2) + precision(1) + height(2) + width(2) + Nf(1)
        // + 3 bytes per component. Omitting the component spec, as an earlier version of
        // this helper did, makes the segment shorter than its declared length and the
        // walker desynchronises exactly where it should.
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.push(0x01); // one component
        v.extend_from_slice(&[0x01, 0x11, 0x00]); // id, sampling factors, quant table
        // SOS, length 8, then a few bytes of entropy-coded data
        v.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        v.extend_from_slice(&[0x01, 0x02, 0x03]);
        v.extend_from_slice(&[0xFF, 0xD9]); // EOI
        v
    }

    #[test]
    fn a_well_formed_header_yields_its_dimensions() {
        assert_eq!(jpeg_dimensions(&tiny_jpeg(1600, 1067)), Some((1600, 1067)));
        assert_eq!(jpeg_dimensions(&tiny_jpeg(64, 48)), Some((64, 48)));
    }

    #[test]
    fn a_header_that_never_reaches_a_frame_marker_is_rejected() {
        // Truncated before SOF0: this is what a coincidental FFD8FF in sensor data looks
        // like, and accepting it would produce a broken thumbnail.
        let full = tiny_jpeg(100, 100);
        // Cut after APP0 but before SOF0.
        let truncated = &full[..8];
        assert_eq!(jpeg_dimensions(truncated), None);
    }

    #[test]
    fn a_desynchronised_chain_is_rejected() {
        let mut bad = vec![0xFF, 0xD8];
        bad.extend_from_slice(&[0x00, 0x11, 0x22, 0x33]); // not a marker
        bad.extend_from_slice(&[0xFF, 0xD9]);
        assert_eq!(jpeg_dimensions(&bad), None);
    }

    #[test]
    fn a_frame_header_declaring_zero_components_is_rejected() {
        // The check that caught the real Canon CR2. A frame header with Nf = 0 is not a
        // picture, and its dimensions look perfectly plausible.
        let mut jpeg = tiny_jpeg(1000, 800);
        // SOF0 sits after SOI (2) + APP0 (6); its Nf byte is 9 bytes into the segment.
        let sof = jpeg.windows(2).position(|w| w == [0xFF, 0xC0]).expect("SOF0") + 9;
        assert_eq!(jpeg[sof], 0x01, "the helper writes one component");
        jpeg[sof] = 0x00;
        assert_eq!(jpeg_dimensions(&jpeg), None, "zero components is not a frame");
    }

    #[test]
    fn a_frame_header_whose_length_disagrees_with_its_component_count_is_rejected() {
        let mut jpeg = tiny_jpeg(1000, 800);
        // Claim four components while supplying one: 8 + 3*4 = 20, not 11.
        let sof = jpeg.windows(2).position(|w| w == [0xFF, 0xC0]).expect("SOF0") + 2;
        jpeg[sof] = 0x00;
        jpeg[sof + 1] = 20;
        assert_eq!(jpeg_dimensions(&jpeg), None);
    }

    #[test]
    fn zero_dimensions_are_rejected() {
        assert_eq!(jpeg_dimensions(&tiny_jpeg(0, 100)), None);
        assert_eq!(jpeg_dimensions(&tiny_jpeg(100, 0)), None);
    }

    #[test]
    fn a_preview_is_found_inside_unrelated_bytes() {
        // The realistic case: a JPEG buried in a container full of other data.
        let mut container = vec![0xABu8; 4096];
        let jpeg = tiny_jpeg(1600, 1067);
        container.extend_from_slice(&jpeg);
        container.extend_from_slice(&[0xCDu8; 4096]);

        match best_preview(&container, DEFAULT_MIN_LONG_EDGE) {
            PreviewSource::Embedded(p) => {
                assert_eq!((p.width, p.height), (1600, 1067));
                assert_eq!(p.jpeg, jpeg, "the extracted bytes must be exactly the stream");
            }
            other => panic!("expected an embedded preview, got {other:?}"),
        }
    }

    #[test]
    fn the_largest_preview_wins() {
        // Real containers hold several: a small thumbnail and a larger preview. Using the
        // thumbnail would produce a grid of blurry mush.
        let mut container = Vec::new();
        container.extend_from_slice(&tiny_jpeg(160, 120));
        container.extend_from_slice(&[0x00; 512]);
        container.extend_from_slice(&tiny_jpeg(1600, 1067));
        container.extend_from_slice(&[0x00; 512]);
        container.extend_from_slice(&tiny_jpeg(800, 600));

        let p = best_preview(&container, DEFAULT_MIN_LONG_EDGE).embedded().cloned().unwrap();
        assert_eq!((p.width, p.height), (1600, 1067));
    }

    #[test]
    fn a_thumbnail_only_container_reports_too_small_rather_than_using_it() {
        // The corpus contains a real one at 64x48. Accepting it would produce a grid of
        // mush and look like the extraction working.
        let container = tiny_jpeg(64, 48);
        match best_preview(&container, DEFAULT_MIN_LONG_EDGE) {
            PreviewSource::TooSmall { width, height } => {
                assert_eq!((width, height), (64, 48));
            }
            other => panic!("expected TooSmall, got {other:?}"),
        }
    }

    #[test]
    fn the_largest_rejected_thumbnail_is_the_one_reported() {
        let mut container = Vec::new();
        container.extend_from_slice(&tiny_jpeg(32, 24));
        container.extend_from_slice(&[0x00; 64]);
        container.extend_from_slice(&tiny_jpeg(320, 240));

        match best_preview(&container, DEFAULT_MIN_LONG_EDGE) {
            PreviewSource::TooSmall { width, height } => {
                assert_eq!((width, height), (320, 240), "report the best one that failed");
            }
            other => panic!("expected TooSmall, got {other:?}"),
        }
    }

    #[test]
    fn a_container_with_no_jpeg_needs_a_decode() {
        assert_eq!(best_preview(&[0u8; 4096], DEFAULT_MIN_LONG_EDGE), PreviewSource::NeedsDecode);
        assert_eq!(best_preview(&[], DEFAULT_MIN_LONG_EDGE), PreviewSource::NeedsDecode);
    }

    #[test]
    fn a_truncated_stream_is_ignored_rather_than_extracted() {
        // Start of image with no end. A truncated stream cannot be decoded, and
        // extracting it would produce a broken thumbnail indistinguishable from a
        // corrupt file.
        let mut container = vec![0u8; 256];
        container.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        assert_eq!(best_preview(&container, DEFAULT_MIN_LONG_EDGE), PreviewSource::NeedsDecode);
    }

    #[test]
    fn a_coincidental_marker_sequence_is_not_mistaken_for_a_preview() {
        // Three bytes that look like a JPEG start, followed by an end marker, with no
        // valid header between them. This is the false positive the header walk exists to
        // reject.
        let mut container = vec![0x5Au8; 1024];
        container.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0x11, 0x22, 0x33, 0xFF, 0xD9]);
        assert_eq!(best_preview(&container, DEFAULT_MIN_LONG_EDGE), PreviewSource::NeedsDecode);
    }

    #[test]
    fn a_thumbnail_nested_inside_the_exif_segment_does_not_truncate_the_outer_stream() {
        // The real-world failure. Cameras embed a JPEG thumbnail inside APP1/EXIF, so the
        // thumbnail's own FFD9 appears within the first few kilobytes of the outer
        // stream. Scanning for the first FFD9 cuts the outer stream short, the header
        // walk then fails, and the file is reported as having no preview — silently, and
        // on a large share of real cameras.
        //
        // The corpus contains exactly this: a raw whose APP1 segment declares 25,608
        // bytes and whose first FFD9 sits at byte 2,640, inside the payload.
        let inner = tiny_jpeg(160, 120);

        // APP1 whose payload contains a complete nested JPEG plus padding.
        let mut payload = inner.clone();
        payload.extend_from_slice(&[0u8; 64]);
        let mut app1 = vec![0xFF, 0xE1];
        let len = (payload.len() + 2) as u16;
        app1.extend_from_slice(&len.to_be_bytes());
        app1.extend_from_slice(&payload);

        let mut outer = vec![0xFF, 0xD8];
        outer.extend_from_slice(&app1);
        // Then the outer stream's own frame header and scan data.
        outer.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        outer.extend_from_slice(&2000u16.to_be_bytes()); // height
        outer.extend_from_slice(&3000u16.to_be_bytes()); // width
        outer.push(0x01);
        outer.extend_from_slice(&[0x01, 0x11, 0x00]);
        outer.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        outer.extend_from_slice(&[0x12, 0x34, 0x56, 0xFF, 0x00, 0x78]); // entropy + stuffing
        outer.extend_from_slice(&[0xFF, 0xD9]);

        match best_preview(&outer, DEFAULT_MIN_LONG_EDGE) {
            PreviewSource::Embedded(p) => {
                assert_eq!(
                    (p.width, p.height),
                    (3000, 2000),
                    "the OUTER stream must be found, not the nested thumbnail"
                );
                assert_eq!(p.jpeg, outer, "and its extent must be the whole stream");
            }
            other => panic!("expected the outer stream, got {other:?}"),
        }
    }

    #[test]
    fn restart_markers_and_stuffed_bytes_inside_scan_data_do_not_end_the_stream() {
        // FFD0-FFD7 are restart markers and FF00 is a stuffed byte; neither is an
        // end-of-image, and treating either as one truncates the stream.
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        jpeg.extend_from_slice(&600u16.to_be_bytes());
        jpeg.extend_from_slice(&800u16.to_be_bytes());
        jpeg.push(0x01);
        jpeg.extend_from_slice(&[0x01, 0x11, 0x00]);
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        jpeg.extend_from_slice(&[0x11, 0xFF, 0x00, 0x22]); // stuffed FF
        jpeg.extend_from_slice(&[0xFF, 0xD0, 0x33, 0x44]); // restart marker
        jpeg.extend_from_slice(&[0xFF, 0xD7, 0x55]); // another restart
        jpeg.extend_from_slice(&[0xFF, 0xD9]);

        let p = best_preview(&jpeg, DEFAULT_MIN_LONG_EDGE).embedded().cloned().unwrap();
        assert_eq!((p.width, p.height), (800, 600));
        assert_eq!(p.jpeg.len(), jpeg.len(), "the whole stream must be captured");
    }

    #[test]
    fn fill_bytes_before_a_marker_are_tolerated() {
        // The specification permits any number of FF bytes before a marker.
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend_from_slice(&[0xFF, 0xFF, 0xFF]); // fill
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        // Above DEFAULT_MIN_LONG_EDGE: at 500x400 this was correctly classified as
        // TooSmall and the test's `.unwrap()` panicked on a right answer.
        jpeg.extend_from_slice(&600u16.to_be_bytes());
        jpeg.extend_from_slice(&800u16.to_be_bytes());
        jpeg.push(0x01);
        jpeg.extend_from_slice(&[0x01, 0x11, 0x00]);
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        jpeg.extend_from_slice(&[0x01, 0x02]);
        jpeg.extend_from_slice(&[0xFF, 0xD9]);

        let p = best_preview(&jpeg, DEFAULT_MIN_LONG_EDGE).embedded().cloned().unwrap();
        assert_eq!((p.width, p.height), (800, 600));
    }

    #[test]
    fn candidates_are_offered_largest_first() {
        let mut container = Vec::new();
        container.extend_from_slice(&tiny_jpeg(160, 120));
        container.extend_from_slice(&[0x00; 64]);
        container.extend_from_slice(&tiny_jpeg(1600, 1067));
        container.extend_from_slice(&[0x00; 64]);
        container.extend_from_slice(&tiny_jpeg(800, 600));

        let c = candidates(&container, DEFAULT_MIN_LONG_EDGE);
        let dims: Vec<(u32, u32)> = c.iter().map(|p| (p.width, p.height)).collect();
        assert_eq!(dims, vec![(1600, 1067), (800, 600)]);
    }

    #[test]
    fn candidates_below_the_threshold_are_excluded_entirely() {
        let mut container = Vec::new();
        container.extend_from_slice(&tiny_jpeg(64, 48));
        container.extend_from_slice(&[0x00; 64]);
        container.extend_from_slice(&tiny_jpeg(1600, 1067));

        let c = candidates(&container, DEFAULT_MIN_LONG_EDGE);
        assert_eq!(c.len(), 1, "the thumbnail must not be offered as a fallback");
        assert_eq!((c[0].width, c[0].height), (1600, 1067));
    }

    #[test]
    fn the_fallback_chain_survives_a_malformed_largest_stream() {
        // The Canon CR2 shape: a good preview and a marginally larger spurious one. The
        // caller tries the largest, fails to decode it, and falls back to the next.
        let mut container = Vec::new();
        container.extend_from_slice(&tiny_jpeg(1936, 1288));
        container.extend_from_slice(&[0x00; 64]);
        // Structurally valid header, marginally larger, entropy data that is not real.
        let mut spurious = vec![0xFF, 0xD8];
        // Length 17 = 8 + 3 components x 3 bytes. Writing 11 here, as the first version of
        // this test did, makes the header self-inconsistent and the length check rejects
        // it before it can be offered as a candidate at all.
        spurious.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        spurious.extend_from_slice(&1296u16.to_be_bytes());
        spurious.extend_from_slice(&1944u16.to_be_bytes());
        spurious.push(0x03);
        spurious.extend_from_slice(&[0x01, 0x11, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00]);
        spurious.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        spurious.extend_from_slice(&[0xAB; 32]);
        spurious.extend_from_slice(&[0xFF, 0xD9]);
        container.extend_from_slice(&spurious);

        let c = candidates(&container, DEFAULT_MIN_LONG_EDGE);
        assert_eq!(c.len(), 2, "both must be offered");
        assert_eq!((c[0].width, c[0].height), (1944, 1296), "the larger comes first");
        assert_eq!(
            (c[1].width, c[1].height),
            (1936, 1288),
            "and the working one is still available to fall back to"
        );
    }

    #[test]
    fn extraction_is_deterministic() {
        let mut container = Vec::new();
        container.extend_from_slice(&tiny_jpeg(160, 120));
        container.extend_from_slice(&[0x00; 64]);
        container.extend_from_slice(&tiny_jpeg(1600, 1067));
        assert_eq!(
            best_preview(&container, DEFAULT_MIN_LONG_EDGE),
            best_preview(&container, DEFAULT_MIN_LONG_EDGE)
        );
    }

    #[test]
    fn a_scan_above_the_size_limit_is_refused() {
        // Guard against being pointed at a video or disk image, where reading the whole
        // thing into memory to look for a JPEG is pointless and expensive.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.bin");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(MAX_SCAN_BYTES + 1).unwrap();
        drop(f);

        assert!(matches!(extract(&path, DEFAULT_MIN_LONG_EDGE), Err(PreviewError::TooLarge { .. })));
    }

    #[test]
    fn reading_a_missing_file_is_an_error() {
        assert!(matches!(
            extract(Path::new("/definitely/not/here.CR3"), DEFAULT_MIN_LONG_EDGE),
            Err(PreviewError::Io { .. })
        ));
    }
}
