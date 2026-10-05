//! EXIF extraction.
//!
//! # What this is for
//!
//! Three fields carry real product weight, and they are the reason this module exists:
//!
//! * **`captured_at`** — burst grouping. Consecutive frames of one moment are seconds
//!   apart; two photographs taken a week apart are not, whatever their filenames say.
//! * **`model`** — a burst is one camera. Two bodies shooting the same scene produce
//!   near-identical frames that are *not* a burst, and the body is what separates them.
//! * **`exposure_time`, `f_number`, `iso`** — exposure-bracket detection. A bracket is
//!   deliberate variation and must never be culled down to one frame the way a burst is.
//!
//! # Timezone: EXIF has no timezone, and that is fine here
//!
//! `DateTimeOriginal` is local wall-clock time at capture with no offset. Converting it
//! to a true epoch second is therefore impossible without guessing the photographer's
//! location and the date, which would be wrong twice a year in most of the world.
//!
//! So the value is stored as if it were UTC, and **only differences between values are
//! used**. Burst grouping asks "are these two frames 1.5 seconds apart?", which is
//! correct under any fixed offset. A DST transition inside a single burst would be
//! wrong, and cannot happen — bursts last seconds.
//!
//! Anything user-facing that shows an absolute time must read it back as wall-clock,
//! not as a converted instant.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExifError {
    #[error("could not open {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// What a read attempt found.
///
/// The three cases are kept distinct on purpose. "No EXIF" and "this container is not
/// one I can read" are different facts, and collapsing them into an empty result makes a
/// reader that never works on Canon CR3 look exactly like a photograph exported without
/// metadata.
#[derive(Debug, Clone, PartialEq)]
pub enum ExifRead {
    Parsed(Box<ExifData>),
    /// The file was read and holds no EXIF — a stripped export, a screenshot, a scan.
    Absent,
    /// The container is not one the reader understands. Some RAW families store
    /// metadata in their own structures rather than in an EXIF block.
    Unsupported,
}

impl ExifRead {
    pub fn data(&self) -> Option<&ExifData> {
        match self {
            ExifRead::Parsed(d) => Some(d),
            _ => None,
        }
    }
}

/// The fields Chaff cares about.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExifData {
    /// `DateTimeOriginal` as "epoch" seconds with the local-time caveat in the module
    /// docs: differences are meaningful, absolute instants are not.
    pub captured_at: Option<i64>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<u32>,
    pub f_number: Option<f64>,
    pub exposure_time: Option<f64>,
    pub focal_length: Option<f64>,
    pub orientation: Option<u16>,
}

impl ExifData {
    /// True when nothing at all was found. A file can be read successfully and still
    /// yield this.
    pub fn is_empty(&self) -> bool {
        *self == ExifData::default()
    }

    /// A camera identity for burst grouping: body first, falling back to make.
    ///
    /// Two different bodies of the same make are correctly distinguished; a body with no
    /// model recorded falls back rather than being treated as unknown-and-therefore-
    /// different, which would split a real burst in two.
    pub fn camera_key(&self) -> Option<String> {
        // Trim first, then reject empties. A camera that writes `Model=""` is common,
        // and treating that as a key would put every such frame in a shoot named after
        // the empty string — merging frames from different bodies into one sequence.
        let clean = |v: &Option<String>| -> Option<String> {
            v.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        };
        clean(&self.model).or_else(|| clean(&self.make))
    }

    /// Heuristic exposure-bracket detection from the three exposure axes.
    ///
    /// Deliberately conservative: it reports that the *sibling set* looks like a bracket,
    /// not that any single frame is one. Detecting a bracket needs the neighbours, and
    /// the caller has them.
    pub fn exposure_signature(&self) -> Option<(i64, i64)> {
        let iso = self.iso? as i64;
        // Time is stored as a rational; use the reciprocal in microseconds so that
        // longer exposures sort greater and the value stays integral.
        let shutter_us = self.exposure_time.map(|t| (t * 1_000_000.0) as i64)?;
        Some((iso, shutter_us))
    }
}

/// Read EXIF from a file.
///
/// Only an I/O failure is an error. A file with no EXIF, or in a container the reader
/// does not understand, is a normal outcome and is reported as such.
pub fn read(path: &Path) -> Result<ExifRead, ExifError> {
    let file = File::open(path).map_err(|source| ExifError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::new(file);

    let exif = match exif::Reader::new().read_from_container(&mut reader) {
        Ok(e) => e,
        Err(exif::Error::NotSupported(_)) => return Ok(ExifRead::Unsupported),
        Err(other) => {
            // **A CR3 before giving up.**
            //
            // A Canon CR3 is an ISO base media file, and `kamadak-exif` accepts exactly two
            // brands — `mif1` and `msf1`. A CR3's is `crx `, so the reader rejects the file and
            // this arm used to return `Absent`: "this photograph has no EXIF".
            //
            // A Canon EOS RP user saw *"No camera information — this file carries no EXIF, or
            // it was stripped"* for **every** photograph. The metadata was there the whole
            // time; the container was not one the reader knew, and the two cases were
            // indistinguishable from the outside.
            match crate::cr3::read_exif(path) {
                Ok(Some(data)) => {
                    return Ok(ExifRead::Parsed(Box::new(data)));
                }
                // A CR3 with no metadata box really is absent.
                Ok(None) => return Ok(ExifRead::Absent),
                // Not a CR3, or one this cannot read. Now it is genuinely unsupported, and
                // saying so is what makes the next container of this kind findable.
                Err(_) => {
                    log::debug!(
                        "{}: {} — no EXIF reader for this container",
                        path.display(),
                        other
                    );
                    return Ok(ExifRead::Unsupported);
                }
            }
        }
    };

    Ok(map(&exif))
}

/// Map a parsed TIFF EXIF block into the engine's own type.
///
/// Extracted so a **CR3** goes through exactly the same tag mapping as a JPEG or a TIFF. The
/// container differs; the tags do not, and a second mapping is how two paths start disagreeing
/// about what `LensModel` means.
pub fn map(exif: &exif::Exif) -> ExifRead {
    let data = ExifData {
        captured_at: field_string(exif, exif::Tag::DateTimeOriginal)
            .or_else(|| field_string(exif, exif::Tag::DateTime))
            .and_then(|s| parse_exif_datetime(&s)),
        make: field_string(exif, exif::Tag::Make).map(clean_string),
        model: field_string(exif, exif::Tag::Model).map(clean_string),
        lens: field_string(exif, exif::Tag::LensModel)
            .or_else(|| field_string(exif, exif::Tag::LensMake))
            .map(clean_string),
        iso: field_u32(exif, exif::Tag::PhotographicSensitivity)
            .or_else(|| field_u32(exif, exif::Tag::ISOSpeed)),
        f_number: field_rational(exif, exif::Tag::FNumber),
        exposure_time: field_rational(exif, exif::Tag::ExposureTime),
        focal_length: field_rational(exif, exif::Tag::FocalLength),
        orientation: field_u32(exif, exif::Tag::Orientation).map(|v| v as u16),
    };

    if data.is_empty() { ExifRead::Absent } else { ExifRead::Parsed(Box::new(data)) }
}

/// Parse a raw TIFF EXIF block, as extracted from a CR3.
///
/// Returns `None` when the block is not readable or carries nothing — the caller treats that
/// as an absent-metadata CR3, which is a normal file.
pub fn parse_tiff_block(block: &[u8]) -> Option<ExifData> {
    let exif = exif::Reader::new().read_raw(block.to_vec()).ok()?;
    match map(&exif) {
        ExifRead::Parsed(d) => Some(*d),
        _ => None,
    }
}

/// Find a tag, preferring IFD0 but falling back to any IFD that carries it.
///
/// Searching only IFD0 loses real photographs. The Exif sub-IFD (0x8769) is where a
/// camera is *supposed* to put ISO, shutter and `DateTimeOriginal`, and real bodies also
/// scatter tags into maker notes and the interoperability IFD. A reader that looks in
/// one place reports "no metadata" for a file that is perfectly well formed — and the
/// failure is silent, because no metadata and unreadable metadata look identical from
/// the outside.
///
/// The IFD0 preference is kept so that when a tag genuinely appears twice, the primary
/// copy wins rather than whichever happened to be enumerated first.
fn field(exif: &exif::Exif, tag: exif::Tag) -> Option<&exif::Field> {
    exif.fields()
        .find(|f| f.tag == tag && f.ifd_num == exif::In::PRIMARY)
        .or_else(|| exif.fields().find(|f| f.tag == tag))
}

fn field_string(exif: &exif::Exif, tag: exif::Tag) -> Option<String> {
    let f = field(exif, tag)?;
    match &f.value {
        exif::Value::Ascii(parts) => {
            let bytes = parts.first()?;
            // EXIF ASCII values are NUL-padded. A trailing NUL in a camera model would
            // make `"Canon EOS R\0"` compare unequal to `"Canon EOS R"` and split a
            // burst in two.
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            Some(String::from_utf8_lossy(&bytes[..end]).to_string())
        }
        _ => Some(f.display_value().to_string()),
    }
}

fn field_u32(exif: &exif::Exif, tag: exif::Tag) -> Option<u32> {
    match &field(exif, tag)?.value {
        exif::Value::Short(v) => v.first().map(|x| *x as u32),
        exif::Value::Long(v) => v.first().copied(),
        exif::Value::Rational(v) => v.first().map(|r| r.to_f64() as u32),
        _ => None,
    }
}

/// Read a rational-valued tag.
///
/// ## The two-element integer case is not optional
///
/// Some writers — Pillow among them — store a rational as a pair of SHORTs rather than
/// as a RATIONAL. kamadak-exif faithfully reports that as `Short([1, 500])`.
///
/// Taking the first element, which is what a naive scalar fallback does, reads **1/500
/// of a second as one full second** and f/2.8 as f/28. That is not a rounding error: it
/// is a wrong number by a factor of 500, it silently breaks exposure-bracket detection,
/// and nothing about the result looks suspicious. Found by a real fixture, not by
/// reasoning.
///
/// A two-element array is therefore interpreted as numerator over denominator. That is
/// safe here because this function is only called for tags the EXIF specification
/// defines as rational; a tag that genuinely holds two unrelated SHORTs (YCbCr
/// subsampling, for instance) is not read through this path.
fn field_rational(exif: &exif::Exif, tag: exif::Tag) -> Option<f64> {
    fn pair_to_f64(n: f64, d: f64) -> Option<f64> {
        if d == 0.0 {
            None
        } else {
            Some(n / d)
        }
    }
    match &field(exif, tag)?.value {
        exif::Value::Rational(v) => v.first().map(|r| r.to_f64()),
        exif::Value::SRational(v) => v.first().map(|r| r.to_f64()),
        exif::Value::Short(v) if v.len() == 2 => pair_to_f64(v[0] as f64, v[1] as f64),
        exif::Value::Long(v) if v.len() == 2 => pair_to_f64(v[0] as f64, v[1] as f64),
        exif::Value::Short(v) => v.first().map(|x| *x as f64),
        _ => None,
    }
}

fn clean_string(s: String) -> String {
    s.trim().trim_matches('\0').trim().to_string()
}

/// Parse `"YYYY:MM:DD HH:MM:SS"` into seconds, treating the value as if it were UTC.
///
/// See the module docs: EXIF carries no timezone, so only differences between parsed
/// values are meaningful.
///
/// Lenient by design. Real cameras produce trailing NULs, a `"YYYY-MM-DD"` variant,
/// missing seconds, and subsecond suffixes. A photograph must not become unindexable
/// because its camera wrote a date slightly unusually.
pub fn parse_exif_datetime(raw: &str) -> Option<i64> {
    let s = raw.trim().trim_matches('\0').trim();
    if s.is_empty() {
        return None;
    }

    // Date and time separated by a space or a 'T'.
    let (date_part, time_part) = match s.split_once([' ', 'T']) {
        Some((d, t)) => (d, t),
        None => (s, "00:00:00"),
    };

    // Accept both ':' and '-' as date separators; both appear in the wild.
    let date_sep = if date_part.contains(':') { ':' } else { '-' };
    let mut d = date_part.split(date_sep);
    let year: i64 = d.next()?.trim().parse().ok()?;
    let month: u32 = d.next()?.trim().parse().ok()?;
    let day: u32 = d.next()?.trim().parse().ok()?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // A zero year is what an unset real-time clock writes. Treat it as absent rather
    // than as 1900, which would sort an entire shoot to the bottom of the library.
    if year <= 0 {
        return None;
    }

    let mut t = time_part.split(':');
    let hour: i64 = t.next().unwrap_or("0").trim().parse().unwrap_or(0);
    let minute: i64 = t.next().unwrap_or("0").trim().parse().unwrap_or(0);
    // Seconds may carry a subsecond suffix, and may be absent entirely.
    let second_raw = t.next().unwrap_or("0").trim();
    let second: i64 = second_raw
        .split(['.', ','])
        .next()
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);

    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }

    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
///
/// Howard Hinnant's `days_from_civil`, which is exact for all dates in range and avoids
/// a date-library dependency for one function. Shifted to a 0000-03-01 era so that leap
/// days land at the end of the cycle and no special case is needed for February.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // March = 0
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {

    #[test]
    fn an_unreadable_container_is_unsupported_not_absent() {
        // **The distinction that hid a real bug.**
        //
        // A Canon CR3 was rejected by the reader, and the rejection was mapped to `Absent` —
        // "this photograph has no EXIF". A user with an EOS RP saw *"No camera information"*
        // for every photograph while the metadata sat in the file.
        //
        // "This file has no metadata" and "I cannot read this file" are different answers. The
        // first is normal and the second is a bug to report, and conflating them makes the
        // second invisible.
        let dir = tempfile::tempdir().unwrap();

        // A file whose container nothing here understands.
        let odd = dir.path().join("mystery.xyz");
        std::fs::write(&odd, b"NOTACONTAINERATALL").unwrap();
        assert!(
            matches!(read(&odd), Ok(ExifRead::Unsupported)),
            "an unknown container must say so, not claim there is no metadata"
        );

        // **And not `Absent`.** That is the assertion that matters: `Absent` means "this
        // photograph has no metadata", which is a claim about the *file*. `Unsupported` means
        // "this program cannot read it", which is a claim about *us* — and only the second is
        // actionable. Mapping the first onto the second is what made every Canon CR3 in a
        // library look like a stripped JPEG.
        assert!(
            !matches!(read(&odd), Ok(ExifRead::Absent)),
            "an unreadable container must never be reported as a file with no metadata"
        );

        // A file that genuinely has no EXIF block at all — a plain TIFF with an empty IFD —
        // is `Absent`, and that is a normal photograph rather than a bug.
        let empty_tiff = dir.path().join("empty.tif");
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II*\0");
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes()); // no entries
        tiff.extend_from_slice(&0u32.to_le_bytes());
        std::fs::write(&empty_tiff, &tiff).unwrap();
        assert!(
            matches!(read(&empty_tiff), Ok(ExifRead::Absent)),
            "a readable container with no tags is absent, not unsupported"
        );
    }

    #[test]
    fn a_cr3_is_read_rather_than_rejected() {
        // The whole point. A CR3 is an ISO base media file whose `ftyp` brand is `crx `, and
        // the `exif` crate accepts exactly `mif1` and `msf1` — so it rejects every CR3 ever
        // written, and the rejection looked like absent metadata.
        use std::io::Write;

        fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
            out
        }

        // A minimal but real TIFF EXIF block: header, one IFD, one entry — Make = "Canon".
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II*\0");
        tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD at offset 8
        tiff.extend_from_slice(&1u16.to_le_bytes()); // one entry
        tiff.extend_from_slice(&0x010Fu16.to_le_bytes()); // Tag::Make
        tiff.extend_from_slice(&2u16.to_le_bytes()); // ASCII
        tiff.extend_from_slice(&6u32.to_le_bytes()); // count
        tiff.extend_from_slice(&26u32.to_le_bytes()); // value offset
        tiff.extend_from_slice(&0u32.to_le_bytes()); // next IFD
        tiff.extend_from_slice(b"Canon\0");

        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"crx ");
        ftyp.extend_from_slice(&[0, 0, 0, 1]);
        ftyp.extend_from_slice(b"crx isom");

        let mut uuid_body = crate::cr3::CANON_UUID.to_vec();
        uuid_body.extend_from_slice(&boxed(b"CMT1", &tiff));

        let mut cr3 = boxed(b"ftyp", &ftyp);
        cr3.extend_from_slice(&boxed(b"uuid", &uuid_body));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("IMG_3161.CR3");
        std::fs::File::create(&path).unwrap().write_all(&cr3).unwrap();

        match read(&path) {
            Ok(ExifRead::Parsed(d)) => {
                assert_eq!(d.make.as_deref(), Some("Canon"), "the make must come through");
            }
            other => panic!("a CR3 must be read, got {other:?}"),
        }
    }

    use super::*;

    #[test]
    fn parses_a_canonical_exif_datetime() {
        // 2026-10-04 12:00:00 UTC
        let got = parse_exif_datetime("2026:10:04 12:00:00").unwrap();
        assert_eq!(got, 1_791_115_200);
    }

    #[test]
    fn the_epoch_itself_round_trips() {
        assert_eq!(parse_exif_datetime("1970:01:01 00:00:00"), Some(0));
    }

    #[test]
    fn known_reference_points_are_exact() {
        // Independently checkable anchors, so an off-by-one in the era arithmetic cannot
        // hide behind self-consistency.
        assert_eq!(parse_exif_datetime("2000:01:01 00:00:00"), Some(946_684_800));
        assert_eq!(parse_exif_datetime("2024:02:29 00:00:00"), Some(1_709_164_800)); // leap day
        assert_eq!(parse_exif_datetime("1969:12:31 23:59:59"), Some(-1));
    }

    #[test]
    fn leap_years_are_handled_across_a_century() {
        // 1900 was not a leap year; 2000 was. The algorithm's era arithmetic is exactly
        // where this is easy to get wrong.
        let feb28_1900 = parse_exif_datetime("1900:02:28 00:00:00").unwrap();
        let mar01_1900 = parse_exif_datetime("1900:03:01 00:00:00").unwrap();
        assert_eq!(mar01_1900 - feb28_1900, 86_400, "1900 had no 29 February");

        let feb28_2000 = parse_exif_datetime("2000:02:28 00:00:00").unwrap();
        let mar01_2000 = parse_exif_datetime("2000:03:01 00:00:00").unwrap();
        assert_eq!(mar01_2000 - feb28_2000, 2 * 86_400, "2000 did have 29 February");
    }

    #[test]
    fn tolerates_the_variations_cameras_actually_produce() {
        let expected = parse_exif_datetime("2026:10:04 12:00:00").unwrap();
        // Trailing NUL, which is extremely common.
        assert_eq!(parse_exif_datetime("2026:10:04 12:00:00\0"), Some(expected));
        // Surrounded by whitespace.
        assert_eq!(parse_exif_datetime("  2026:10:04 12:00:00  "), Some(expected));
        // Dash separators.
        assert_eq!(parse_exif_datetime("2026-10-04 12:00:00"), Some(expected));
        // No time part at all -> midnight, which is 12 hours before the 12:00 anchor.
        assert_eq!(parse_exif_datetime("2026:10:04"), Some(expected - 43_200));
        // Subsecond suffix.
        assert_eq!(parse_exif_datetime("2026:10:04 12:00:00.123"), Some(expected));
        // Missing seconds.
        assert_eq!(parse_exif_datetime("2026:10:04 12:00"), Some(expected));
    }

    #[test]
    fn rejects_nonsense_rather_than_inventing_a_date() {
        assert_eq!(parse_exif_datetime(""), None);
        assert_eq!(parse_exif_datetime("   "), None);
        assert_eq!(parse_exif_datetime("not a date"), None);
        assert_eq!(parse_exif_datetime("2026:13:04 12:00:00"), None, "month 13");
        assert_eq!(parse_exif_datetime("2026:00:04 12:00:00"), None, "month 0");
        assert_eq!(parse_exif_datetime("2026:10:32 12:00:00"), None, "day 32");
        assert_eq!(parse_exif_datetime("2026:10:04 25:00:00"), None, "hour 25");
    }

    #[test]
    fn an_unset_clock_is_treated_as_absent_not_as_1900() {
        // A camera with a flat battery writes 0000:00:00. Treating that as a real date
        // would sort the whole shoot to the bottom of the library and split every burst.
        assert_eq!(parse_exif_datetime("0000:00:00 00:00:00"), None);
        assert_eq!(parse_exif_datetime("    :  :     :  :  "), None);
    }

    #[test]
    fn camera_key_prefers_the_body_and_falls_back_to_the_make() {
        let mut d = ExifData::default();
        assert_eq!(d.camera_key(), None);

        d.make = Some("Canon".into());
        assert_eq!(d.camera_key().as_deref(), Some("Canon"));

        d.model = Some("Canon EOS R5".into());
        assert_eq!(d.camera_key().as_deref(), Some("Canon EOS R5"));

        // Two bodies of one make must not collapse into the same burst key.
        let other = ExifData {
            make: Some("Canon".into()),
            model: Some("Canon EOS R6".into()),
            ..Default::default()
        };
        assert_ne!(d.camera_key(), other.camera_key());
    }

    #[test]
    fn an_empty_model_falls_back_to_the_make_rather_than_becoming_a_key() {
        let d = ExifData {
            make: Some("Canon".into()),
            model: Some("   ".into()),
            ..Default::default()
        };
        assert_eq!(
            d.camera_key().as_deref(),
            Some("Canon"),
            "a whitespace model must not become a shoot key of its own"
        );

        let empty = ExifData {
            make: Some("".into()),
            model: Some("".into()),
            ..Default::default()
        };
        assert_eq!(empty.camera_key(), None, "nothing usable means no key, not an empty one");
    }

    #[test]
    fn camera_key_trims_padding_so_one_body_is_one_key() {
        let a = ExifData { model: Some("NIKON Z 6".into()), ..Default::default() };
        let b = ExifData { model: Some("NIKON Z 6   ".into()), ..Default::default() };
        assert_eq!(a.camera_key(), b.camera_key());
    }

    #[test]
    fn exposure_signature_needs_iso_and_shutter() {
        let mut d = ExifData::default();
        assert_eq!(d.exposure_signature(), None);
        d.iso = Some(400);
        assert_eq!(d.exposure_signature(), None, "shutter is still missing");
        d.exposure_time = Some(1.0 / 250.0);
        let (iso, shutter) = d.exposure_signature().unwrap();
        assert_eq!(iso, 400);
        assert_eq!(shutter, 4_000, "1/250s is 4000 microseconds");
    }

    #[test]
    fn an_empty_dataset_is_recognised() {
        assert!(ExifData::default().is_empty());
        let d = ExifData { iso: Some(100), ..Default::default() };
        assert!(!d.is_empty());
    }

    #[test]
    fn reading_a_missing_file_is_an_error_not_a_silent_absence() {
        // A file that cannot be opened is different from a file with no EXIF, and the
        // caller must be able to tell them apart.
        let err = read(Path::new("/definitely/not/here.jpg"));
        assert!(matches!(err, Err(ExifError::Io { .. })));
    }

    #[test]
    fn reading_a_non_image_reports_unsupported_rather_than_absent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.txt");
        std::fs::write(&p, b"just some text, certainly not an image container").unwrap();
        // Either verdict is defensible for arbitrary bytes; what must not happen is an
        // error, because a stray file in a library must never abort an index.
        assert!(matches!(read(&p), Ok(ExifRead::Unsupported | ExifRead::Absent)));
    }

    #[test]
    fn reads_full_exif_from_a_generated_fixture() {
        // The synthetic fixtures write Make, Model, DateTime, ISO, FNumber, ExposureTime
        // and FocalLength. The real corpus JPEGs carry only orientation, so these
        // fixtures are the primary substrate for the interesting fields.
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/images/sharp_a.jpg");
        if !path.is_file() {
            eprintln!("SKIP: run tools/fixtures/generate_synthetic.py first");
            return;
        }

        let read_result = read(&path).expect("read");
        let Some(d) = read_result.data() else {
            panic!("the generated fixture should carry EXIF, got {read_result:?}");
        };

        assert_eq!(d.make.as_deref(), Some("Chaff"));
        assert_eq!(d.model.as_deref(), Some("Chaff Test Body"));
        assert_eq!(d.iso, Some(400));
        assert_eq!(d.orientation, Some(1));
        assert!(
            d.captured_at.is_some(),
            "the fixture writes DateTime, which must parse: {d:?}"
        );
        assert_eq!(
            d.camera_key().as_deref(),
            Some("Chaff Test Body"),
            "burst grouping keys on the body"
        );
    }

    #[test]
    fn a_rational_stored_as_two_shorts_is_not_read_as_its_numerator() {
        // The bug this guards. Pillow writes ExposureTime as Short([1, 500]); reading the
        // first element gives one full second instead of 1/500, and the only symptom is
        // that bracket detection stops working.
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/images");

        // Both encodings, because both occur in the wild and they fail differently.
        for (name, what) in [
            ("exif_shortpair", "SHORT pair (Pillow's default, and some other tools)"),
            ("bracket_m1", "true RATIONAL (what a camera emits)"),
        ] {
            let path = base.join(format!("{name}.jpg"));
            if !path.is_file() {
                eprintln!("SKIP: run tools/fixtures/generate_synthetic.py first");
                return;
            }
            let read_result = read(&path).expect("read");
            let d = read_result.data().unwrap_or_else(|| panic!("{name} carries EXIF"));

            let t = d.exposure_time.unwrap_or_else(|| panic!("{name}: ExposureTime must be read"));
            assert!(
                (t - 1.0 / 500.0).abs() < 1e-9,
                "{name} ({what}): 1/500 s must read as 0.002, not as its numerator. Got {t}"
            );
            let f = d.f_number.unwrap_or_else(|| panic!("{name}: FNumber must be read"));
            assert!((f - 2.8).abs() < 1e-9, "{name} ({what}): f/2.8 must read as 2.8, got {f}");
        }
    }

    #[test]
    fn a_real_raw_file_parses_without_erroring() {
        // The real-world check. TIFF-based raws (DNG, NEF, CR2) carry a standard EXIF
        // block; non-TIFF families may not. Neither may error, and at least one real
        // format must actually yield data or the reader does not work in practice.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus/raw");
        if !dir.is_dir() {
            eprintln!("SKIP: run tools/fixtures/fetch_corpus.py --raw 12 first");
            return;
        }

        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .expect("read corpus")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        entries.sort();
        assert!(!entries.is_empty());

        let mut parsed = 0;
        for path in &entries {
            match read(path) {
                Ok(ExifRead::Parsed(d)) => {
                    parsed += 1;
                    eprintln!("  {} -> camera_key={:?}", path.file_name().unwrap().to_string_lossy(), d.camera_key());
                }
                Ok(other) => eprintln!("  {} -> {other:?}", path.file_name().unwrap().to_string_lossy()),
                Err(e) => panic!("reading {} failed: {e}", path.display()),
            }
        }
        assert!(
            parsed > 0,
            "no real RAW file yielded EXIF, so the reader does not work on real camera files"
        );
    }
}
