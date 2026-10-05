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
pub fn exif_blocks<R: Read + Seek>(reader: &mut R) -> std::io::Result<Option<Vec<Vec<u8>>>> {
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
                        let blocks = find_cmt_blocks(reader, inner_start, inner_end)?;
                        if !blocks.is_empty() {
                            return Ok(Some(blocks));
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

/// Every `CMT` block inside a Canon `uuid` box, in order.
///
/// # Why all of them and not just the first
///
/// A CR3 splits its metadata across boxes, and the split is not arbitrary:
///
/// * **CMT1** — IFD0. Make, Model, Orientation, `DateTime`.
/// * **CMT2** — the Exif sub-IFD. ISO, shutter, aperture, focal length, `LensModel`,
///   `DateTimeOriginal`.
/// * **CMT3** — Canon's maker notes.
/// * **CMT4** — GPS.
///
/// Reading only CMT1 produces a panel that says *"Canon EOS RP"* and nothing else — which is
/// exactly what the first version of this did, and what a user saw: camera and a date, no lens,
/// no ISO, no shutter, no aperture. The `DateTime` it showed came from IFD0's fallback, which is
/// how it looked plausible enough not to be questioned.
fn find_cmt_blocks<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    end: u64,
) -> std::io::Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
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

        if kind.starts_with(b"CMT") {
            let mut block = vec![0u8; body_len as usize];
            reader.seek(SeekFrom::Start(body_start))?;
            reader.read_exact(&mut block)?;
            out.push(block);
        }
        offset = body_start + body_len;
    }
    Ok(out)
}

/// Wrap a bare IFD as the Exif sub-IFD of a synthetic TIFF.
///
/// # Why this is necessary
///
/// `CMT2` is a **bare IFD** — an entry count followed by entries — with no TIFF header, because
/// in the file it is reached through IFD0's `ExifIFD` pointer. Pulled out of the container it is
/// not a document, and `exif::Reader::read_raw` needs one.
///
/// So this builds the smallest TIFF that says "the Exif sub-IFD is over there": a header, an
/// IFD0 with a single `0x8769` entry pointing past itself, and the real IFD after it. That is
/// enough for the reader to parse the block with the same code it uses for a JPEG.
pub fn wrap_as_exif_ifd(ifd: &[u8]) -> Vec<u8> {
    const HEADER: u32 = 8;
    const IFD0_ENTRIES: u16 = 1;
    const IFD0_LEN: u32 = 2 + 12 * IFD0_ENTRIES as u32 + 4;
    let target = HEADER + IFD0_LEN;

    let mut body = ifd.to_vec();
    shift_value_offsets(&mut body, target);

    let mut out = Vec::with_capacity(body.len() + target as usize);
    out.extend_from_slice(b"II*\0");
    out.extend_from_slice(&HEADER.to_le_bytes());

    // IFD0: one entry, the ExifIFD pointer.
    out.extend_from_slice(&IFD0_ENTRIES.to_le_bytes());
    out.extend_from_slice(&0x8769u16.to_le_bytes()); // ExifIFD
    out.extend_from_slice(&4u16.to_le_bytes()); // LONG
    out.extend_from_slice(&1u32.to_le_bytes()); // one value
    out.extend_from_slice(&target.to_le_bytes()); // at the IFD we were handed
    out.extend_from_slice(&0u32.to_le_bytes()); // no next IFD

    out.extend_from_slice(&body);
    out
}

/// Add `shift` to every out-of-line value offset in a bare IFD.
///
/// # Why this is necessary
///
/// A bare IFD's offsets are relative to **its own start**. Moving it behind a TIFF header moves
/// everything it points at, so every offset that names a location has to move with it. Without
/// this the reader follows an offset into the middle of the header, finds nothing, and reports
/// no metadata — which is the failure this whole module exists to stop looking like a normal
/// file.
///
/// Inline values (four bytes or fewer) are **not** offsets and must not be touched; adding a
/// shift to a two-byte ISO value would turn 100 into 126.
fn shift_value_offsets(ifd: &mut [u8], shift: u32) {
    if ifd.len() < 2 {
        return;
    }
    let count = u16::from_le_bytes([ifd[0], ifd[1]]) as usize;
    for i in 0..count {
        let at = 2 + i * 12;
        if at + 12 > ifd.len() {
            // A truncated IFD is a corrupt file. Stopping is right; reading past the end is
            // how a parser segfaults on a photograph.
            return;
        }
        let kind = u16::from_le_bytes([ifd[at + 2], ifd[at + 3]]);
        let n = u32::from_le_bytes([ifd[at + 4], ifd[at + 5], ifd[at + 6], ifd[at + 7]]);

        let unit = match kind {
            1 | 2 | 6 | 7 => 1u32, // BYTE, ASCII, SBYTE, UNDEFINED
            3 | 8 => 2,            // SHORT, SSHORT
            4 | 9 | 11 => 4,       // LONG, SLONG, FLOAT
            5 | 10 | 12 => 8,      // RATIONAL, SRATIONAL, DOUBLE
            // An unknown type is left alone rather than guessed at. The reader will reject it,
            // which is better than this moving a value that was not an offset.
            _ => continue,
        };

        if unit.saturating_mul(n) > 4 {
            let off = u32::from_le_bytes([ifd[at + 8], ifd[at + 9], ifd[at + 10], ifd[at + 11]]);
            let moved = off.saturating_add(shift);
            ifd[at + 8..at + 12].copy_from_slice(&moved.to_le_bytes());
        }
    }
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

    let Some(blocks) = exif_blocks(&mut file)? else {
        return Ok(None);
    };

    // **Every block, merged, first value wins.**
    //
    // CMT1 is IFD0 and CMT2 is the Exif sub-IFD, so a panel that read only the first showed
    // "Canon EOS RP" and a date and nothing else — no lens, no ISO, no shutter, no aperture.
    // Which box holds what is Canon's business; taking everything and filling gaps is the
    // answer that does not depend on knowing.
    let mut merged: Option<super::exif::ExifData> = None;
    for (i, block) in blocks.iter().enumerate() {
        // The first block is a complete TIFF; the rest are bare IFDs and need wrapping.
        let parsed = if i == 0 {
            super::exif::parse_tiff_block(block)
        } else {
            super::exif::parse_tiff_block(&wrap_as_exif_ifd(block))
        };
        let Some(data) = parsed else { continue };
        merged = Some(match merged {
            None => data,
            Some(acc) => acc.filled_from(data),
        });
    }
    Ok(merged)
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
        cr3_with_blocks(&[(b"CMT1", exif)])
    }

    /// A CR3 with the given `CMT` blocks, in order.
    fn cr3_with_blocks(blocks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"crx ");
        ftyp.extend_from_slice(&[0, 0, 0, 1]);
        ftyp.extend_from_slice(b"crx isom");

        let mut uuid_body = CANON_UUID.to_vec();
        for (kind, body) in blocks {
            uuid_body.extend_from_slice(&boxed(kind, body));
        }

        let mut out = boxed(b"ftyp", &ftyp);
        out.extend_from_slice(&boxed(b"uuid", &uuid_body));
        out
    }

    /// A **bare IFD** with one ASCII entry, offsets relative to its own start.
    ///
    /// This is what `CMT2` is: an entry count and entries, with no TIFF header, because in the
    /// file it is reached through IFD0's `ExifIFD` pointer.
    fn bare_ifd_ascii(tag: u16, value: &[u8]) -> Vec<u8> {
        let mut v = value.to_vec();
        v.push(0);
        // count(2) + entry(12) + next(4) = 18, so the value goes at 18 **relative to here**.
        let mut t = Vec::new();
        t.extend_from_slice(&1u16.to_le_bytes());
        t.extend_from_slice(&tag.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes()); // ASCII
        t.extend_from_slice(&(v.len() as u32).to_le_bytes());
        t.extend_from_slice(&18u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&v);
        t
    }

    /// A TIFF with one ASCII entry: `(tag, value)`.
    fn tiff_ascii(tag: u16, value: &[u8]) -> Vec<u8> {
        let mut v = value.to_vec();
        v.push(0);
        // Header + IFD (2 + 12 + 4) = 8 + 18 = 26, so the value goes at 26.
        let mut t = Vec::new();
        t.extend_from_slice(b"II*\0");
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&1u16.to_le_bytes());
        t.extend_from_slice(&tag.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes()); // ASCII
        t.extend_from_slice(&(v.len() as u32).to_le_bytes());
        t.extend_from_slice(&26u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&v);
        t
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

        let blocks = exif_blocks(&mut Cursor::new(&cr3)).unwrap().expect("a block");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], exif);
    }

    #[test]
    fn every_cmt_block_is_read_not_only_the_first() {
        // **The bug the user found by looking.** A CR3 splits its metadata: CMT1 is IFD0 (Make,
        // Model, DateTime) and CMT2 is the Exif sub-IFD (ISO, shutter, aperture, focal length,
        // LensModel). Reading only CMT1 produced a panel saying "Canon EOS RP" and a date and
        // nothing else — which looked plausible enough that it was not questioned.
        let cmt1 = tiff_ascii(0x010F, b"Canon"); // Make
        let cmt2 = bare_ifd_ascii(0x0110, b"Canon EOS RP"); // Model — standing in for the sub-IFD

        let cr3 = cr3_with_blocks(&[(b"CMT1", &cmt1), (b"CMT2", &cmt2)]);
        let blocks = exif_blocks(&mut Cursor::new(&cr3)).unwrap().expect("blocks");
        assert_eq!(blocks.len(), 2, "both blocks must be found: {blocks:?}");
        assert_eq!(blocks[0], cmt1);
        assert_eq!(blocks[1], cmt2);
    }

    #[test]
    fn a_bare_ifd_is_wrapped_so_the_reader_can_parse_it() {
        // CMT2 is a **bare IFD** — an entry count and entries, with no TIFF header, because in
        // the file it is reached through IFD0's ExifIFD pointer. Pulled out of the container it
        // is not a document, and the reader needs one.
        let bare = bare_ifd_ascii(0x0110, b"Canon EOS RP");

        let wrapped = wrap_as_exif_ifd(&bare);
        assert!(wrapped.starts_with(b"II*\0"), "it must be a TIFF now");

        // And the reader gets the tag out of it.
        let parsed = crate::exif::parse_tiff_block(&wrapped);
        assert_eq!(
            parsed.and_then(|d| d.model),
            Some("Canon EOS RP".to_string()),
            "a wrapped bare IFD must parse"
        );
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

        assert_eq!(exif_blocks(&mut Cursor::new(&cr3)).unwrap(), None);
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
        assert_eq!(exif_blocks(&mut Cursor::new(&cr3)).unwrap(), None);
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
        assert_eq!(exif_blocks(&mut Cursor::new(&cr3)).unwrap(), None);
    }

    #[test]
    fn an_empty_file_is_an_error_or_nothing_never_a_panic() {
        assert!(exif_blocks(&mut Cursor::new(Vec::new())).is_ok());
        assert!(exif_blocks(&mut Cursor::new(b"crx ".to_vec())).is_ok());
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

        let blocks = exif_blocks(&mut Cursor::new(&cr3)).unwrap().expect("a block");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], exif);
    }
}
