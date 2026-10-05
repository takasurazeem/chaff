//! Full-decoding a raw when it has no usable preview (#8).
//!
//! # Why this exists
//!
//! Most raw files carry an embedded JPEG, and the preview path reads that — it is instant and
//! it is what the camera thought the photograph looked like. Some do not: a few formats, some
//! camera settings, and files from tools that strip the preview. Those photographs were
//! **unreadable**, which meant unscored, ungrouped and untagged.
//!
//! # Why the vendored build
//!
//! `libraw_rs_vendor` compiles LibRaw from source. A system `libraw` would be a package the
//! user has to install — and on the Linux box it is not there — which makes a feature that
//! works on the developer's machine and not on the server. The cost is a longer first build.
//!
//! # Safety
//!
//! This is the only `unsafe` block in the engine, and it is confined to this module. LibRaw is
//! a C library with a handle that must be freed and buffers that must be released, so the
//! handle is wrapped in a type whose `Drop` calls `libraw_close` — including on the error
//! paths, which is where a hand-written cleanup is most often missed.

use std::ffi::CString;
use std::path::Path;

use image::{DynamicImage, RgbImage};

use libraw_rs_vendor as lr;

#[derive(Debug, thiserror::Error)]
pub enum RawError {
    #[error("LibRaw could not be initialised")]
    Init,
    #[error("{path} could not be opened by LibRaw: {reason}")]
    Open { path: String, reason: String },
    #[error("LibRaw could not unpack {path}: {reason}")]
    Unpack { path: String, reason: String },
    #[error("LibRaw could not process {path}: {reason}")]
    Process { path: String, reason: String },
    #[error("LibRaw returned no image for {path}")]
    NoImage { path: String },
    #[error("{path} has a path that is not valid UTF-8")]
    BadPath { path: String },
    #[error("LibRaw produced {width}x{height} with {bits} bits and {colors} channels, which this build cannot read")]
    Unexpected { width: u16, height: u16, bits: u16, colors: u16 },
    #[error(
        "this build has no raw decoder: the vendored LibRaw does not compile under MSVC. \
         A raw with no embedded preview cannot be read on Windows."
    )]
    Unsupported,
}

#[cfg(windows)]
pub fn decode(_path: &Path) -> Result<DynamicImage, RawError> {
    // **A real loss, stated rather than hidden.** The vendored LibRaw does not compile under
    // MSVC — its build script passes GCC flags to `cl.exe` — so Windows has no full-decode
    // fallback and a raw with no embedded preview stays unreadable there. Every other
    // platform has it.
    Err(RawError::Unsupported)
}

/// A LibRaw handle that frees itself.
#[cfg(not(windows))]
struct Handle(*mut lr::libraw_data_t);

#[cfg(not(windows))]
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from `libraw_init`, is non-null, and this is the only
            // place it is freed. `Drop` runs on every path out of `decode`, including the
            // `?` returns — which is the reason the handle is a type rather than a local.
            unsafe { lr::libraw_close(self.0) };
        }
    }
}

/// LibRaw's message for a code, as a `String`.
#[cfg(not(windows))]
fn message(code: std::os::raw::c_int) -> String {
    // SAFETY: `libraw_strerror` returns a pointer to a static string for any code, including
    // unknown ones — it does not return null.
    let p = unsafe { lr::libraw_strerror(code) };
    if p.is_null() {
        return format!("error {code}");
    }
    // SAFETY: a NUL-terminated C string owned by LibRaw, valid for the life of the process.
    unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Decode a raw file in full.
///
/// The expensive path: this runs the demosaic, which is why it is a fallback rather than the
/// default. The embedded preview is instant and usually enough for scoring; this is for the
/// files where there is no preview to use.
#[cfg(not(windows))]
pub fn decode(path: &Path) -> Result<DynamicImage, RawError> {
    let path_str = path.display().to_string();
    let c_path = CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| RawError::BadPath { path: path_str.clone() })?;

    // SAFETY: `libraw_init(0)` takes no pointers and returns either null or an owned handle.
    let handle = Handle(unsafe { lr::libraw_init(0) });
    if handle.0.is_null() {
        return Err(RawError::Init);
    }

    // SAFETY: `handle.0` is a live handle, and `c_path` outlives the call.
    let code = unsafe { lr::libraw_open_file(handle.0, c_path.as_ptr()) };
    if code != lr::LibRaw_errors_LIBRAW_SUCCESS {
        return Err(RawError::Open { path: path_str, reason: message(code) });
    }

    // SAFETY: as above.
    let code = unsafe { lr::libraw_unpack(handle.0) };
    if code != lr::LibRaw_errors_LIBRAW_SUCCESS {
        return Err(RawError::Unpack { path: path_str, reason: message(code) });
    }

    // SAFETY: as above.
    let code = unsafe { lr::libraw_dcraw_process(handle.0) };
    if code != lr::LibRaw_errors_LIBRAW_SUCCESS {
        return Err(RawError::Process { path: path_str, reason: message(code) });
    }

    let mut err: std::os::raw::c_int = 0;
    // SAFETY: `handle.0` is live and `err` is a valid out-parameter. The returned pointer is
    // owned by LibRaw until `libraw_dcraw_clear_mem`, which is called below on every path.
    let image = unsafe { lr::libraw_dcraw_make_mem_image(handle.0, &mut err) };
    if image.is_null() {
        return Err(RawError::NoImage { path: path_str });
    }

    // Copied out and released before anything can return early.
    //
    // SAFETY: `image` came from `libraw_dcraw_make_mem_image` and has not been cleared.
    let result = unsafe { copy_out(image, &path_str) };
    // SAFETY: `image` came from `libraw_dcraw_make_mem_image` and has not been freed.
    unsafe { lr::libraw_dcraw_clear_mem(image) };
    result
}

/// Copy a `libraw_processed_image_t` into an `image` buffer.
///
/// # Safety
///
/// `image` must be a non-null pointer from `libraw_dcraw_make_mem_image` that has not been
/// cleared.
#[cfg(not(windows))]
unsafe fn copy_out(
    image: *const lr::libraw_processed_image_t,
    path: &str,
) -> Result<DynamicImage, RawError> {
    // SAFETY: the caller guarantees the pointer is live. `data` is declared as a one-element
    // array — the C idiom for a flexible trailing buffer — so the pixel data starts there and
    // runs for `data_size` bytes, which is what is read.
    let header = &*image;
    let (width, height, bits, colors, size) =
        (header.width, header.height, header.bits, header.colors, header.data_size as usize);

    if header.type_ != lr::LibRaw_image_formats_LIBRAW_IMAGE_BITMAP {
        // A JPEG preview would need a second decoder; asking LibRaw for a bitmap is what the
        // caller does, so this is a "should not happen" rather than a case to handle.
        return Err(RawError::Unexpected { width, height, bits, colors });
    }
    // 8-bit RGB is what `dcraw_process` produces by default. A 16-bit output would need a
    // different conversion, and silently truncating it would lose the highlights.
    if bits != 8 || colors != 3 {
        return Err(RawError::Unexpected { width, height, bits, colors });
    }

    // SAFETY: `data` is the start of a buffer of `data_size` bytes, and the check above
    // established 3 bytes per pixel at 8 bits.
    let data = std::slice::from_raw_parts(header.data.as_ptr(), size);
    let expected = width as usize * height as usize * 3;
    if data.len() < expected {
        return Err(RawError::NoImage { path: path.to_string() });
    }

    let buffer = RgbImage::from_raw(width as u32, height as u32, data[..expected].to_vec())
        .ok_or_else(|| RawError::NoImage { path: path.to_string() })?;
    Ok(DynamicImage::ImageRgb8(buffer))
}

/// Is LibRaw available in this build?
///
/// False on Windows, where the vendored crate does not compile. Present so a caller can ask
/// rather than discover it from an error.
pub fn available() -> bool {
    !cfg!(windows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_reports_that_it_has_no_decoder() {
        // A capability query rather than an error to discover. On Windows this is false and
        // `decode` returns `Unsupported`; elsewhere it is true.
        assert_eq!(available(), !cfg!(windows));
    }

    #[cfg(not(windows))]
    #[test]
    fn a_file_that_is_not_a_raw_is_an_error_not_a_crash() {
        // **The property that matters most here.** This is the only `unsafe` in the engine,
        // and it is handed arbitrary files by a directory walk. A malformed file must produce
        // an error, not a segfault and not a freed handle.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-raw.cr3");
        std::fs::write(&path, b"this is not a raw file").unwrap();

        let e = decode(&path);
        assert!(e.is_err(), "got {e:?}");
    }

    #[cfg(not(windows))]
    #[test]
    fn a_missing_file_is_an_error_not_a_crash() {
        let e = decode(Path::new("/nonexistent-xyz/IMG_0001.CR3"));
        assert!(matches!(e, Err(RawError::Open { .. })), "got {e:?}");
    }

    #[cfg(not(windows))]
    #[test]
    fn an_empty_file_is_an_error_not_a_crash() {
        // The zero-byte case, which is what an interrupted copy leaves behind.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.cr3");
        std::fs::write(&path, b"").unwrap();
        assert!(decode(&path).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn a_truncated_header_is_an_error_not_a_crash() {
        // A file that looks like a raw for the first few bytes and then stops. This is the
        // case a length check in the C library might not catch, and the one that would read
        // past a buffer.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("truncated.cr3");
        let mut bytes = b"II*\0".to_vec(); // a plausible TIFF header
        bytes.extend_from_slice(&[0x10, 0x00, 0x00, 0x00]);
        bytes.extend_from_slice(&[0u8; 8]);
        std::fs::write(&path, &bytes).unwrap();
        assert!(decode(&path).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn many_failed_decodes_do_not_leak_or_double_free() {
        // **The RAII guard's test.** LibRaw handles must be freed exactly once, on every path.
        // A hand-written cleanup usually gets the success path right and the error paths
        // wrong, and the symptom is a leak that only shows after a long run — or a double free
        // that shows as a crash in an unrelated place.
        //
        // This does not prove the absence of a leak, which needs a sanitizer. It does prove
        // that a hundred failures in a row neither crash nor corrupt the allocator.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..100 {
            let path = dir.path().join(format!("bad{i}.cr3"));
            std::fs::write(&path, format!("not a raw {i}")).unwrap();
            assert!(decode(&path).is_err());
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn a_null_handle_would_be_caught_rather_than_dereferenced() {
        // `libraw_init` returns null on failure, and the check is before any use. Asserted
        // here so the guard cannot be "simplified" away.
        let guard = Handle(std::ptr::null_mut());
        drop(guard); // must not call libraw_close on a null pointer
    }

    #[cfg(not(windows))]
    #[test]
    fn libraw_is_compiled_in() {
        // The vendored build means there is no system package to install — which is what
        // makes this work on the Linux box, where `libraw` is not present.
        assert!(available());
    }
}
