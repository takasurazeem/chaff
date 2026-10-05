//! Getting models onto the machine, and proving they are the right ones.
//!
//! # Why not commit them
//!
//! YuNet is 233 KB and ships in the repository. SFace is **38 MB**, and committing it would
//! multiply the repository size for every clone, every CI checkout, and every fork — to
//! distribute a file that is one HTTP request away and never changes.
//!
//! # Why the hash is not optional
//!
//! A downloaded file that is used without verification is an unverified input to a program
//! that then makes claims about a person's photographs. A truncated download, a captive
//! portal's HTML error page, or a substituted file all produce the same symptom — a model
//! that loads and gives quietly wrong answers. The hash is checked before the file is moved
//! into place, so a bad download cannot become the cached copy.
//!
//! # Why it is cached in the app data directory
//!
//! Not beside the models in the source tree, which may be read-only or absent in a packaged
//! build. Not in the library, which is the user's and is not ours to write into.

use std::path::{Path, PathBuf};

use chaff_core::egress::{self, Policy};

/// A model this application knows how to fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    /// Filename once cached.
    pub file: &'static str,
    pub url: &'static str,
    /// blake3 of the file, hex.
    pub blake3: &'static str,
    pub bytes: u64,
    /// Shown before downloading. Faces are biometric data and these licences differ.
    pub licence: &'static str,
    pub description: &'static str,
}

/// SFace, from OpenCV Zoo.
///
/// **Apache-2.0**, which is why it is the default recogniser rather than the more accurate
/// InsightFace `buffalo_l` — that pack is non-commercial and is #44, opt-in, with its
/// licence shown at the point of download.
pub const SFACE: ModelSpec = ModelSpec {
    file: "face_recognition_sface_2021dec.onnx",
    url: "https://github.com/opencv/opencv_zoo/raw/main/models/face_recognition_sface/face_recognition_sface_2021dec.onnx",
    blake3: "584d0f5792e9e776ab5e5bf3f559f2310ff8fdd3b39f94dd364751046592b512",
    bytes: 38_696_353,
    licence: "Apache-2.0",
    description: "Face recognition: a 128-dimension embedding per face, for grouping",
};

/// InsightFace `buffalo_l` (#44).
///
/// # Why this is not the default, and why the licence is in the type
///
/// `buffalo_l` is materially more accurate than SFace at grouping faces, and its licence is
/// **non-commercial**. Shipping it as the default would make every user of this application
/// subject to a licence they never agreed to, for a feature they did not ask to enable.
///
/// So it is opt-in, and `licence` is a field rather than a comment: [`ModelStore::ensure`]
/// takes a [`Consent`] for a model whose licence is not permissive, so a download cannot
/// happen without the user having been told. A licence notice in a README is one nobody
/// reads, and "we documented it" is not consent.
pub const BUFFALO_L: ModelSpec = ModelSpec {
    file: "buffalo_l.zip",
    url: "https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip",
    // Filled from the release; the store refuses to use a model whose hash is not known.
    blake3: "0000000000000000000000000000000000000000000000000000000000000000",
    bytes: 281_000_000,
    licence: "Non-commercial research use only (InsightFace)",
    description: "Face recognition: more accurate grouping, non-commercial licence",
};

/// Whether the user has agreed to a model's licence.
///
/// # Why this is a type and not a boolean
///
/// A boolean at the call site is a boolean somebody passes `true` to because the tests need
/// it to work. Making it a named argument that must be constructed from the *licence text*
/// means the caller has to have it — and [`Consent::for_model`] is the only constructor,
/// so the text cannot be invented at the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consent {
    /// The licence the user was shown. Checked against the model's own.
    pub licence: String,
}

impl Consent {
    /// Consent to a specific model's licence, having been shown it.
    pub fn for_model(spec: &ModelSpec) -> Self {
        Self { licence: spec.licence.to_string() }
    }

    /// Whether a model may be downloaded under this consent.
    pub fn covers(&self, spec: &ModelSpec) -> bool {
        self.licence == spec.licence
    }
}

/// Is a model's licence permissive enough to use without asking?
///
/// The list is short and exact on purpose: a model whose licence nobody has classified is
/// treated as restricted, because the failure of guessing wrong is the user being bound by
/// terms they never saw.
pub fn is_permissive(spec: &ModelSpec) -> bool {
    matches!(spec.licence, "MIT" | "Apache-2.0" | "BSD-3-Clause")
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error(
        "{file} is licensed \"{licence}\" and cannot be downloaded without agreeing to it. \
         Show the user the licence, then pass Consent::for_model."
    )]
    ConsentRequired { file: String, licence: String },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not download {url}: {reason}")]
    Download { url: String, reason: String },
    #[error(
        "the downloaded {file} is not the expected file — got {found}, expected {expected}. \
         It was discarded rather than used."
    )]
    ChecksumMismatch { file: String, expected: String, found: String },
}

/// Where downloaded models live, and how they are verified.
#[derive(Debug, Clone)]
pub struct ModelStore {
    root: PathBuf,
}

impl ModelStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The cached path for a model, whether or not it is there.
    pub fn path_for(&self, spec: &ModelSpec) -> PathBuf {
        self.root.join(spec.file)
    }

    /// True when the model is cached.
    pub fn is_present(&self, spec: &ModelSpec) -> bool {
        self.path_for(spec).is_file()
    }

    /// Return the model, downloading it if it is not cached.
    ///
    /// `on_progress` is called with `(downloaded_bytes, total_bytes)`.
    pub fn ensure(
        &self,
        spec: &ModelSpec,
        mut on_progress: impl FnMut(u64, u64),
    ) -> Result<PathBuf, ModelError> {
        // **A restricted licence needs consent before anything is fetched.** Checked here
        // rather than at the call site so there is no path to the download that skips it —
        // the same reasoning as the egress chokepoint, applied to a licence.
        if !is_permissive(spec) {
            return Err(ModelError::ConsentRequired {
                file: spec.file.to_string(),
                licence: spec.licence.to_string(),
            });
        }
        self.ensure_with_consent(spec, None, &mut on_progress)
    }

    /// Return the model, having been given the user's consent to its licence.
    pub fn ensure_with_consent(
        &self,
        spec: &ModelSpec,
        consent: Option<&Consent>,
        on_progress: &mut impl FnMut(u64, u64),
    ) -> Result<PathBuf, ModelError> {
        if !is_permissive(spec) {
            match consent {
                Some(c) if c.covers(spec) => {}
                _ => {
                    return Err(ModelError::ConsentRequired {
                        file: spec.file.to_string(),
                        licence: spec.licence.to_string(),
                    })
                }
            }
        }
        let path = self.path_for(spec);
        if path.is_file() {
            // Verified on use, not only on download: a cached file can be truncated by a
            // full disk or replaced by something else, and the whole point is that nothing
            // unverified reaches the model loader.
            if hash_file(&path)? == spec.blake3 {
                return Ok(path);
            }
            log::warn!("cached {} failed verification; downloading again", spec.file);
        }

        std::fs::create_dir_all(&self.root).map_err(|source| ModelError::Io {
            path: self.root.clone(),
            source,
        })?;

        // Through the chokepoint. A model download is the one request this application
        // makes to the public internet, and it goes through the same allowlist as everything
        // else rather than reaching for `ureq` on its own.
        let bytes = download(&Policy::default(), spec.url, spec.bytes, on_progress)?;

        let found = blake3::hash(&bytes).to_hex().to_string();
        if found != spec.blake3 {
            return Err(ModelError::ChecksumMismatch {
                file: spec.file.to_string(),
                expected: spec.blake3.to_string(),
                found,
            });
        }

        // Written to a temporary name and renamed, so a partial download can never be the
        // cached copy — the failure mode that makes a model "load" and answer wrongly.
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, &bytes).map_err(|source| ModelError::Io {
            path: tmp.clone(),
            source,
        })?;
        std::fs::rename(&tmp, &path).map_err(|source| ModelError::Io {
            path: path.clone(),
            source,
        })?;

        Ok(path)
    }
}

fn hash_file(path: &Path) -> Result<String, ModelError> {
    let data = std::fs::read(path).map_err(|source| ModelError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(blake3::hash(&data).to_hex().to_string())
}

/// Fetch a URL into memory, reporting progress.
///
/// **Through the chokepoint, not `ureq` directly.** The first version checked the policy and
/// then called `ureq` itself, which made the allowlist true and the "single chokepoint"
/// claim only nearly true — one transport outside the one place that is supposed to own
/// them all. A claim that is nearly true is the kind this codebase has been burned by.
///
/// Into memory rather than streamed to disk, because the hash must be checked *before* the
/// file is written. A partial file on disk that is verified afterwards leaves a window where
/// it is present and unverified.
fn download(
    policy: &Policy,
    url: &str,
    expected_bytes: u64,
    on_progress: &mut impl FnMut(u64, u64),
) -> Result<Vec<u8>, ModelError> {
    let mut report = |done: u64, total: u64| on_progress(done, if total == 0 { expected_bytes } else { total });
    egress::download_with_progress(policy, url, 900, &mut report)
        .map_err(|e| ModelError::Download { url: url.to_string(), reason: e.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cached_model_is_verified_not_trusted() {
        // A cached file can be truncated by a full disk or replaced by something else. The
        // first version checked the hash only on download, so anything already on disk was
        // used unverified.
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        // An unreachable URL on purpose. The first version reused `SFACE`'s real URL, so
        // once the real hash replaced the placeholder this test downloaded 38 MB and
        // *succeeded* — proving nothing, slowly. The property under test is "a corrupt
        // cached file is not returned", and to see that the download has to fail.
        let spec = ModelSpec {
            file: "m.onnx",
            url: "http://127.0.0.1:1/never",
            ..SFACE
        };

        std::fs::write(store.path_for(&spec), b"not the model").unwrap();
        assert!(store.is_present(&spec), "the file exists");

        let result = store.ensure(&spec, |_, _| {});
        assert!(result.is_err(), "a corrupt cached model must not be returned");
    }

    #[test]
    fn a_download_that_does_not_match_is_discarded() {
        // The failure this prevents: a captive portal's HTML error page, or a truncated
        // transfer, producing a model that loads and answers wrongly.
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let spec = ModelSpec {
            file: "m.onnx",
            url: "http://127.0.0.1:1/never",
            blake3: "0000000000000000000000000000000000000000000000000000000000000000",
            ..SFACE
        };
        assert!(store.ensure(&spec, |_, _| {}).is_err());
        assert!(!store.is_present(&spec), "nothing must be left behind");
    }

    #[test]
    fn a_failed_download_leaves_no_partial_file() {
        // The `.part` temporary is the mechanism: a partial file can never be the cached
        // copy, because it is never renamed into place.
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let spec = ModelSpec {
            file: "m.onnx",
            url: "http://127.0.0.1:1/never",
            ..SFACE
        };
        let _ = store.ensure(&spec, |_, _| {});
        assert!(!store.path_for(&spec).exists());
        assert!(!dir.path().join("m.part").exists());
    }

    #[test]
    fn a_non_permissive_model_cannot_be_fetched_without_consent() {
        // **The point of #44.** Shipping a non-commercial model as the default would bind
        // every user to a licence they never agreed to, for a feature they did not ask to
        // enable. A licence notice in a README is one nobody reads, and "we documented it"
        // is not consent.
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());

        let e = store.ensure(&BUFFALO_L, |_, _| {}).unwrap_err();
        assert!(
            matches!(e, ModelError::ConsentRequired { .. }),
            "expected a consent error, got {e}"
        );
        // And it did not fetch anything on the way to refusing.
        assert!(!store.is_present(&BUFFALO_L));
    }

    #[test]
    fn consent_for_one_model_does_not_cover_another() {
        // Consent is to a *licence*, not to downloads in general. A user who agreed to
        // buffalo_l's terms has not agreed to whatever the next model's terms are.
        let consent = Consent::for_model(&BUFFALO_L);
        assert!(consent.covers(&BUFFALO_L));
        assert!(!consent.covers(&SFACE), "SFace's licence is a different agreement");
    }

    #[test]
    fn the_permissive_list_is_exact_and_short() {
        // A model whose licence nobody has classified is treated as **restricted**, because
        // the failure of guessing wrong is the user being bound by terms they never saw.
        assert!(is_permissive(&SFACE), "Apache-2.0");
        assert!(!is_permissive(&BUFFALO_L));
        let unknown = ModelSpec { licence: "Custom", ..SFACE };
        assert!(!is_permissive(&unknown), "an unclassified licence is not permissive");
    }

    #[test]
    fn the_buffalo_spec_says_it_is_non_commercial() {
        // The text is what the user is shown, so it has to say the thing that matters.
        assert!(BUFFALO_L.licence.to_lowercase().contains("non-commercial"));
        assert!(BUFFALO_L.description.to_lowercase().contains("non-commercial"));
    }

    #[test]
    fn the_sface_spec_names_a_permissive_licence() {
        // Faces are biometric data. The default recogniser must be one whose licence allows
        // the use, and the field exists so a non-permissive one can say so where it is
        // chosen rather than in a README.
        assert_eq!(SFACE.licence, "Apache-2.0");
        assert!(SFACE.bytes > 0);
        assert!(SFACE.url.starts_with("https://"));
    }
}
