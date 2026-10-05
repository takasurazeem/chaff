//! Reading EXIF out of a Canon CR3.
//!
//! # Why this is not the `exif` crate's job
//!
//! A CR3 is an **ISO base media file** — the MP4 family — not a TIFF like every other raw.
//! `kamadak-exif` does handle ISO BMFF, and it accepts exactly two brands:
//!
//! ```text
//! static HEIF_BRANDS: &[[u8; 4]] = &[*b"mif1", *b"msf1"];
//! ```
//!
//! A CR3's `ftyp` brand is **`crx `**, Canon's own. So the reader rejects the file, and — this
//! is the part that hid the bug — the rejection was indistinguishable from a file that
//! genuinely has no EXIF. A user with a Canon EOS RP saw *"No camera information — this file
//! carries no EXIF, or it was stripped"* for every photograph, which is the opposite of true.
//!
//! # Where Canon puts it
//!
//! Inside a `uuid` box whose type is Canon's own, in a `CMT1` box holding a **standard TIFF
//! EXIF block**. So the job here is to find that box and hand the bytes to the TIFF reader,
//! which already works — the container is the only unusual part.
//!
//! # What this does not do
//!
//! It does not decode the raw. It reads a few kilobytes of metadata, which is why it can run
//! on every file during an index.

use std::io::{Read, Seek, SeekFrom};

/// Canon's UUID for the box holding `CMT1`.
///
/// `85c0b687-820f-11e0-8111-f4ce462b6a48`, as it appears on disk.
pub const CANON_UUID: [u8; 16] = [
    0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48,
];

/// The largest metadata box worth reading.
///
/// A CR3's metadata box is a few kilobytes. A limit stops a corrupt length field from asking
/// for a two-gigabyte allocation — a malformed file must be an error, not an out-of-memory.
const MAX_BOX: u64 = 4 * 1024 * 1024;

/// Is this a CR3?
///
/// The `ftyp` brand, which is the only reliable signal — the extension is a hint and a file
/// can be renamed.
pub fn is_cr3(head: &[u8]) -> bool {
    // `ftyp` at offset 4, then the major brand at 8.
    head.len() >= 12 && &head[4..8] == b"ftyp" && &head[8..12] == b"crx "
}

/// The TIFF EXIF block inside a CR3, if there is one.
///
/// Returns the bytes starting at the TIFF header, which is what `exif::Reader::read_raw`
/// expects — the container is unwrapped here so the TIFF reader can do the part it is good at.
pub fn exif_block<R: Read + Seek>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;

    // The boxes to walk, as `(offset, length)`. A stack rather than recursion: a nested box
    // structure from an untrusted file is how a parser ends up recursing until the stack goes.
    let mut stack = vec![(0u64, len)];

    while let Some((start, end)) = stack.pop() {
        let mut offset = start;
        while offset + 8 <= end {
            reader.seek(SeekFrom::Start(offset))?;

            let mut header = [0u8; 8];
            if reader.read_exact(&mut header).is_err() {
                break;
            }
            let size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
            let kind = &header[4..8];

            // A size of 1 means a 64-bit length follows; 0 means "to the end of the file".
            let (body_start, body_len) = match size {
                1 => {
                    let mut big = [0u8; 8];
                    reader.read_exact(&mut big)?;
                    (offset + 16, u64::from_be_bytes(big))
                }
                0 => (offset + 8, end - offset - 8),
                n => (offset + 8, n.saturating_sub(8)),
            };

            if body_len > MAX_BOX || body_start + body_len > end {
                // A length that runs past its parent is a corrupt file. Skipping it is right;
                // trusting it is how a parser reads out of bounds.
                break;
            }

            match kind {
                // Canon's metadata box: a `uuid` whose type is theirs, then `CMT1` inside.
                b"uuid" => {
                    let mut uuid = [0u8; 16];
                    reader.seek(SeekFrom::Start(body_start))?;
                    if reader.read_exact(&mut uuid).is_ok() && uuid == CANON_UUID {
                        let inner_start = body_start + 16;
                        let inner_end = body_start + body_len;
                        if let Some(block) = find_cmt1(reader, inner_start, inner_end)? {
                            return Ok(Some(block));
                        }
                    }
                }
                // `moov` and `meta` are containers; their children may hold the box.
                b"moov" | b"meta" | b"mdia" | b"minf" | b"stbl" => {
                    stack.push((body_start, body_start + body_len));
                }
                _ => {}
            }

            offset = body_start + body_len;
        }
    }
    Ok(None)
}

/// Find `CMT1` inside a Canon `uuid` box and return its TIFF block.
fn find_cmt1<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    end: u64,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut offset = start;
    while offset + 8 <= end {
        reader.seek(SeekFrom::Start(offset))?;
        let mut header = [0u8; 8];
        if reader.read_exact(&mut header).is_err() {
            break;
        }
        let size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let kind = &header[4..8];
        let body_start = offset + 8;
        let body_len = size.saturating_sub(8);
        if body_len > MAX_BOX || body_start + body_len > end {
            break;
        }

        if kind == b"CMT1" {
            let mut block = vec![0u8; body_len as usize];
            reader.seek(SeekFrom::Start(body_start))?;
            reader.read_exact(&mut block)?;
            return Ok(Some(block));
        }
        offset = body_start + body_len;
    }
    Ok(None)
}

/// Read the EXIF out of a CR3, mapped into the engine's own type.
///
/// Returns `Ok(None)` when the file is a CR3 with no metadata box, and `Err` when it is not a
/// CR3 at all — the caller distinguishes them, because "no metadata" and "cannot read this"
/// are different answers and conflating them is what hid this bug for the whole project.
pub fn read_exif(path: &std::path::Path) -> std::io::Result<Option<super::exif::ExifData>> {
    let mut file = std::fs::File::open(path)?;

    // The brand first, so a JPEG is not walked as a box structure.
    let mut head = [0u8; 12];
    if file.read_exact(&mut head).is_err() || !is_cr3(&head) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a CR3",
        ));
    }

    let Some(block) = exif_block(&mut file)? else {
        return Ok(None);
    };
    Ok(super::exif::parse_tiff_block(&block))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Build a box: 4-byte big-endian length, 4-byte type, then the body.
    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    /// A CR3 with a Canon metadata box holding `CMT1`.
    fn cr3_with(exif: &[u8]) -> Vec<u8> {
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"crx ");
        ftyp.extend_from_slice(&[0, 0, 0, 1]);
        ftyp.extend_from_slice(b"crx isom");

        let mut uuid_body = CANON_UUID.to_vec();
        uuid_body.extend_from_slice(&boxed(b"CMT1", exif));

        let mut out = boxed(b"ftyp", &ftyp);
        out.extend_from_slice(&boxed(b"uuid", &uuid_body));
        out
    }

    #[test]
    fn a_cr3_is_recognised_by_its_brand_not_its_extension() {
        // The extension is a hint; a file can be renamed. The `ftyp` brand is the signal.
        let cr3 = cr3_with(b"TIFF");
        assert!(is_cr3(&cr3));

        // A CR2 is TIFF-based and must not take this path.
        let cr2 = [0x49, 0x49, 0x2a, 0x00, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(!is_cr3(&cr2));

        // And neither must a HEIF, which the `exif` crate already handles.
        let mut heic = Vec::new();
        heic.extend_from_slice(&[0, 0, 0, 24]);
        heic.extend_from_slice(b"ftyp");
        heic.extend_from_slice(b"mif1");
        heic.extend_from_slice(&[0, 0, 0, 0]);
        assert!(!is_cr3(&heic));
    }

    #[test]
    fn the_exif_block_is_found_inside_the_canon_uuid_box() {
        // The whole job: unwrap the container so the TIFF reader can do the part it is good at.
        let exif = b"II*\0\x08\0\0\0";
        let cr3 = cr3_with(exif);

        let block = exif_block(&mut Cursor::new(&cr3)).unwrap().expect("a block");
        assert_eq!(block, exif);
    }

    #[test]
    fn a_cr3_with_no_canon_box_yields_nothing_rather_than_an_error() {
        // A file that is a CR3 but carries no metadata box is **absent**, not broken. The
        // difference matters: the first is normal, the second is a bug to report.
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"crx ");
        ftyp.extend_from_slice(&[0, 0, 0, 1]);
        ftyp.extend_from_slice(b"crx isom");
        let cr3 = boxed(b"ftyp", &ftyp);

        assert_eq!(exif_block(&mut Cursor::new(&cr3)).unwrap(), None);
    }

    #[test]
    fn a_uuid_box_that_is_not_canons_is_ignored() {
        // Other vendors use `uuid` too. Reading theirs as Canon's would produce EXIF from the
        // wrong place, which is worse than none.
        let mut body = vec![0xAAu8; 16];
        body[0] = 0x11;
        body.extend_from_slice(&boxed(b"CMT1", b"not canon"));

        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"crx ");
        ftyp.extend_from_slice(&[0, 0, 0, 1]);

        let mut cr3 = boxed(b"ftyp", &ftyp);
        cr3.extend_from_slice(&boxed(b"uuid", &body));
        assert_eq!(exif_block(&mut Cursor::new(&cr3)).unwrap(), None);
    }

    #[test]
    fn a_corrupt_length_does_not_read_out_of_bounds_or_allocate() {
        // **The property that matters for an untrusted file.** A length field claiming two
        // gigabytes must be rejected, not allocated — a malformed photograph must be an error
        // and not an out-of-memory.
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"crx ");
        ftyp.extend_from_slice(&[0, 0, 0, 1]);

        let mut cr3 = boxed(b"ftyp", &ftyp);
        // A `uuid` claiming a 4 GB body in a 40-byte file.
        cr3.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        cr3.extend_from_slice(b"uuid");
        cr3.extend_from_slice(&CANON_UUID);
        cr3.extend_from_slice(&boxed(b"CMT1", b"x"));

        // Must not panic, and must not find anything.
        assert_eq!(exif_block(&mut Cursor::new(&cr3)).unwrap(), None);
    }

    #[test]
    fn an_empty_file_is_an_error_or_nothing_never_a_panic() {
        assert!(exif_block(&mut Cursor::new(Vec::new())).is_ok());
        assert!(exif_block(&mut Cursor::new(b"crx ".to_vec())).is_ok());
    }

    #[test]
    fn the_boxes_are_found_through_a_nested_container() {
        // `moov` and `meta` are containers. A box that only looked at the top level would miss
        // a file that nests, and the failure would look like "no EXIF" again.
        let exif = b"II*\0\x08\0\0\0";
        let mut uuid_body = CANON_UUID.to_vec();
        uuid_body.extend_from_slice(&boxed(b"CMT1", exif));
        let inner = boxed(b"uuid", &uuid_body);
        let moov = boxed(b"moov", &inner);

        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"crx ");
        ftyp.extend_from_slice(&[0, 0, 0, 1]);

        let mut cr3 = boxed(b"ftyp", &ftyp);
        cr3.extend_from_slice(&moov);

        let block = exif_block(&mut Cursor::new(&cr3)).unwrap().expect("a block");
        assert_eq!(block, exif);
    }
}
