//! CLIP zero-shot tagging, for the tier with no model server (#53).
//!
//! # What this is for
//!
//! The VLM path (#49) is better — a 35B vision model writes real descriptions and tags. It
//! also needs a GPU and a server running. This is the fallback: a small image encoder that
//! runs on a CPU, compares a photograph against a fixed vocabulary, and returns the closest
//! ones.
//!
//! # Why the vocabulary is fixed and its embeddings are committed
//!
//! CLIP's text side needs a BPE tokenizer and a 61 MB text encoder. The vocabulary here does
//! not change, so its embeddings are computed **once** by `examples/clip_vocab.rs` and
//! committed as a ~200 KB data file. At runtime only the vision encoder runs.
//!
//! Shipping the tokenizer and the text model would mean 145 MB and two dependencies to
//! recompute numbers that never change.
//!
//! # What it cannot do
//!
//! CLIP compares against phrases it was given. It cannot name something outside the
//! vocabulary, and its confidences are **not calibrated** — a 0.31 similarity is not "31%
//! likely". They are only comparable to each other, within one photograph, which is exactly
//! how they are used.

use std::path::{Path, PathBuf};

use crate::detect::DetectError;

/// What CLIP's ViT-B/32 image encoder was trained at.
const INPUT: usize = 224;
/// Dimensions in an embedding.
pub const DIMENSIONS: usize = 512;

/// The phrases the fallback can choose between.
///
/// **Closed on purpose.** CLIP returns whichever phrase is closest, so an open vocabulary
/// would return a confident answer for a photograph of anything at all. A fixed list means
/// the worst case is "the closest of these", which is a bounded and explainable claim.
///
/// Chosen for what people actually filter a photograph library by: subjects, settings,
/// activities, and the light.
/// The phrases CLIP chooses between.
///
/// # Why this list is the whole feature, and the encoder is not
///
/// CLIP is a **ranking** model: it scores an image against a set of phrases and the pass keeps the
/// best few. The encoder is 88 MB and does not change; this list is what a user actually sees on
/// their photographs.
///
/// The first version had **38 phrases** — enough to prove the path worked and not enough to be
/// useful. "Travel" was the tag for a landscape, a street and a suitcase alike, because those were
/// the only words available. The list below is the same weights with a vocabulary worth having.
///
/// # How it is organised
///
/// By **what a photographer looks for**, not by taxonomy. A culling session asks "are there people
/// in this?", "is this the reception or the ceremony?", "did I get the bird?" — so the phrases are
/// grouped the way those questions are, and a few near-synonyms are kept where CLIP scores them
/// differently. Duplicates cost nothing but a row of floats; a missing phrase is a photograph
/// nobody can find.
///
/// # Changing it
///
/// The embeddings are **precomputed and committed** — CLIP's text side needs a BPE tokenizer and a
/// second 61 MB model, and both are build-time tools. After editing this list:
///
/// ```text
/// cargo run -p chaff-faces --example clip_vocab -- \
///     tokenizer.json text_model_int8.onnx models/clip/vocabulary.bin
/// ```
///
/// The vision encoder is unchanged, so nothing else needs rebuilding. A stale `vocabulary.bin`
/// fails loudly: `Vocabulary::load` checks that the file's phrase count matches this list.
pub const VOCABULARY: &[&str] = &[
    // ---- People ----
    "a person", "a group of people", "a crowd of people", "a child", "a baby",
    "a family", "a couple", "a man", "a woman", "an elderly person",
    "a portrait", "a selfie", "people at a party", "people at a wedding",
    "a bride and groom", "people at a concert", "people playing sport",
    "people sitting at a table", "people standing", "people walking",
    "a person smiling", "a person looking at the camera", "a person from behind",
    // ---- Animals ----
    "a dog", "a puppy", "a cat", "a kitten", "a bird", "a bird in flight",
    "a horse", "a cow", "a sheep", "a wild animal", "an insect", "a butterfly",
    "a fish", "a pet", "wildlife",
    // ---- Places: outside ----
    "a beach", "a rocky coast", "a mountain", "a snow-capped mountain", "a hill",
    "a forest", "a single tree", "a field", "a meadow", "a desert", "a canyon",
    "a lake", "a river", "a waterfall", "the sea", "a harbour", "a garden",
    "a park", "a city street", "a village", "a farm", "a bridge",
    "a road", "a path or trail", "a campsite",
    // ---- Places: inside ----
    "a room indoors", "a kitchen", "a living room", "a bedroom", "a bathroom",
    "a restaurant", "a cafe", "a bar", "a shop", "an office", "a museum",
    "a church", "a stadium", "an airport", "a train station", "a hotel room",
    "a library", "a classroom", "a hospital", "a workshop",
    // ---- Buildings and structures ----
    "a building", "a house", "a skyscraper", "a ruin", "a castle", "a temple",
    "a bridge at night", "a monument", "a fence", "a wall", "a staircase",
    "a doorway", "a window", "a roof",
    // ---- Things ----
    "food", "a meal on a plate", "a drink", "a cup of coffee", "a cake",
    "fruit", "vegetables", "a car", "a motorcycle", "a bicycle", "a bus",
    "a train", "an aeroplane", "a boat", "a truck", "a tractor",
    "flowers", "a bouquet of flowers", "a houseplant", "a book", "a document",
    "a screen", "a computer", "a phone", "a camera", "a musical instrument",
    "a guitar", "a painting", "a sculpture", "artwork", "a sign", "a map",
    "clothing", "jewellery", "a tool", "furniture", "a lamp", "a candle",
    "a toy", "a balloon", "a flag", "money", "a bottle", "a bag",
    // ---- Activities and events ----
    "a wedding", "a birthday party", "a dinner party", "a concert",
    "a music performance", "sport", "a football match", "running",
    "cycling", "swimming", "skiing", "climbing", "fishing", "cooking",
    "reading", "working", "shopping", "a protest", "a parade", "fireworks",
    "travel", "a holiday", "a picnic", "a market", "a festival",
    // ---- Light, weather and time ----
    "a sunset", "a sunrise", "golden hour", "blue hour", "night",
    "a night sky", "stars", "the moon", "clouds", "a storm", "rain",
    "snow", "fog or mist", "a rainbow", "backlit", "silhouette",
    "harsh sunlight", "soft light", "shadow", "reflection",
    // ---- Scene, in the words a person would use ----
    //
    // **The categories a culling session actually asks for**, and the ones the first pass missed.
    // "Outdoors" and "indoors" are not implied by the phrases around them: CLIP scores each phrase
    // independently, so a beach photograph does not score "outdoors" unless "outdoors" is one of
    // the things it is choosing between.
    //
    // These are deliberately **coarse**. A user narrowing 3,000 photographs wants "show me the
    // outdoor ones" before "show me the ones with a mountain", and a tagger with only the second
    // makes the first a matter of selecting nine tags at once.
    "outdoors", "indoors", "a landscape", "a street scene", "a group photo",
    "a group portrait", "a night scene", "a cityscape", "a seascape",
    "a rural scene", "an urban scene", "a wide open space", "a confined space",

    // ---- Photographic character ----
    "a close-up", "a macro photograph", "a wide landscape", "a still life",
    "an abstract photograph", "a black and white photograph", "a blurry photograph",
    "a dark photograph", "a bright photograph", "a photograph of text",
    "a screenshot", "a diagram or chart",
    // ---- Water and sky ----
    "a waterfall in a forest", "waves", "a swimming pool", "a fountain",
    "a sky with clouds", "a clear blue sky",
];

/// The committed vocabulary embeddings.
pub struct Vocabulary {
    phrases: Vec<String>,
    /// Row-major, `phrases.len() * DIMENSIONS`, each row unit-length.
    vectors: Vec<f32>,
    dim: usize,
}

impl Vocabulary {
    /// Load from the committed file.
    pub fn load(path: &Path) -> Result<Self, ClipError> {
        let bytes = std::fs::read(path).map_err(|e| ClipError::Io {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;

        // A magic string rather than a bare length: a truncated or wrong file must fail
        // loudly, because the failure mode otherwise is plausible nonsense — embeddings of
        // the right shape that mean nothing.
        if bytes.len() < 16 || &bytes[..10] != b"CHAFFCLIP1" {
            return Err(ClipError::Format(path.to_path_buf()));
        }
        let count = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as usize;
        let dim = u32::from_le_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]) as usize;

        let expected = 18 + count * dim * 4;
        if bytes.len() != expected {
            return Err(ClipError::Format(path.to_path_buf()));
        }

        // **The file and the list must agree.** The embeddings are computed from `VOCABULARY` by
        // a build-time tool, so editing the list without regenerating the file would pair new
        // phrases with old vectors — plausible nonsense, every tag wrong, nothing failing.
        if count != VOCABULARY.len() {
            return Err(ClipError::Mismatch {
                file: path.to_path_buf(),
                in_file: count,
                in_code: VOCABULARY.len(),
            });
        }

        let mut vectors = Vec::with_capacity(count * dim);
        for chunk in bytes[18..].chunks_exact(4) {
            vectors.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }

        Ok(Self { phrases: VOCABULARY.iter().map(|s| s.to_string()).collect(), vectors, dim })
    }

    /// The phrase nearest an image embedding, and the rest in order.
    ///
    /// Returns every phrase with its similarity, best first. The caller decides where to cut:
    /// a threshold here would be a constant chosen against a corpus this does not have, and
    /// the numbers are not calibrated anyway.
    pub fn rank(&self, image: &[f32]) -> Vec<(String, f32)> {
        if image.len() != self.dim {
            return Vec::new();
        }
        let mut out: Vec<(String, f32)> = self
            .phrases
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let row = &self.vectors[i * self.dim..(i + 1) * self.dim];
                // Both sides are unit-length, so the dot product is the cosine.
                let dot: f32 = row.iter().zip(image).map(|(a, b)| a * b).sum();
                (p.clone(), dot)
            })
            .collect();

        // Descending, ties broken by phrase so the order is total and reproducible.
        out.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        out
    }

    pub fn len(&self) -> usize {
        self.phrases.len()
    }
    pub fn is_empty(&self) -> bool {
        self.phrases.is_empty()
    }
}

/// The committed embeddings, if they are present.
pub fn bundled() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("models/clip/vocabulary.bin");
    p.is_file().then_some(p)
}

/// The CLIP image encoder.
pub struct Clip {
    session: std::sync::Mutex<ort::session::Session>,
    input_name: String,
    output_name: String,
}

impl Clip {
    pub fn from_file(path: &Path) -> Result<Self, DetectError> {
        ort::init().with_name("chaff").commit();
        let session = ort::session::Session::builder()
            .map_err(|e| DetectError::Inference(e.to_string()))?
            .with_intra_threads(2)
            .map_err(|e| DetectError::Inference(e.to_string()))?
            .commit_from_file(path)
            .map_err(|e| DetectError::Inference(format!("{}: {e}", path.display())))?;

        let input_name = session.inputs()[0].name().to_string();
        let output_name = session.outputs()[0].name().to_string();
        Ok(Self { session: std::sync::Mutex::new(session), input_name, output_name })
    }

    /// The image embedding, unit-length.
    pub fn embed(&self, pixels: &[u8], width: u32, height: u32) -> Result<Vec<f32>, DetectError> {
        let expected = width as usize * height as usize * 3;
        if pixels.len() != expected {
            return Err(DetectError::BufferSize { width, height, expected });
        }

        let input = preprocess(pixels, width, height);
        let tensor = ort::value::Tensor::from_array(([1usize, 3, INPUT, INPUT], input))
            .map_err(|e| DetectError::Input(e.to_string()))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| DetectError::Inference("the session lock is poisoned".into()))?;
        let outputs = session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| DetectError::Inference(e.to_string()))?;

        let value = outputs
            .get(self.output_name.as_str())
            .ok_or_else(|| DetectError::Inference(format!("no output {}", self.output_name)))?;
        let array = value
            .try_extract_array::<f32>()
            .map_err(|e| DetectError::Inference(e.to_string()))?;

        let mut v: Vec<f32> = array.iter().copied().collect();
        // Normalised here, so a comparison is a dot product and two callers cannot disagree
        // about whether it was done.
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 1e-9 {
            for x in v.iter_mut() {
                *x /= n;
            }
        }
        Ok(v)
    }
}

/// The image encoder in a model store, if it has been fetched.
///
/// **Not `bundled`, because it is not bundled.** The model is 84 MB and is downloaded and
/// hash-verified like SFace — see [`crate::models::CLIP_VISION`]. What *is* committed is the
/// vocabulary, which is this project's own output.
pub fn model_in(store: &crate::models::ModelStore) -> Option<PathBuf> {
    let p = store.path_for(&crate::models::CLIP_VISION);
    p.is_file().then_some(p)
}

/// The image encoder in the default model store.
pub fn bundled_model() -> Option<PathBuf> {
    model_in(&crate::models::ModelStore::new(default_store_root()))
}

/// Where models live when no store is given.
///
/// Public because an integration test has to look in the same place the library does, and a test
/// that guesses the path is a test that silently skips when the guess is wrong — which is worse
/// than one that fails.
pub fn default_store() -> crate::models::ModelStore {
    crate::models::ModelStore::new(default_store_root())
}

/// Where downloaded models live when nothing else is specified.
fn default_store_root() -> PathBuf {
    std::env::var("CHAFF_DATA")
        .map(|d| PathBuf::from(d).join("models"))
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("models")
        })
}

/// Resize, centre-crop and normalise, as CLIP's preprocessing specifies.
///
/// CLIP's own transform is resize-shortest-side-then-centre-crop, **not** a plain squash.
/// Squashing changes the aspect of everything in the frame, and CLIP was trained on crops.
///
/// The normalisation constants are CLIP's, and using the wrong ones produces embeddings that
/// are stable, comparable, and systematically wrong.
fn preprocess(pixels: &[u8], width: u32, height: u32) -> Vec<f32> {
    const MEAN: [f32; 3] = [0.481_454_6, 0.457_827_5, 0.408_210_7];
    const STD: [f32; 3] = [0.268_629_5, 0.261_302_6, 0.275_777_1];

    // The crop: the largest square that fits, centred.
    let side = width.min(height);
    let x0 = (width - side) / 2;
    let y0 = (height - side) / 2;

    let plane = INPUT * INPUT;
    let mut out = vec![0f32; 3 * plane];
    for ty in 0..INPUT {
        for tx in 0..INPUT {
            // Nearest neighbour: this feeds a classifier, and a smoother filter changes
            // nothing it cares about while costing time on the CPU tier this exists for.
            let sx = x0 + (tx as u32 * side) / INPUT as u32;
            let sy = y0 + (ty as u32 * side) / INPUT as u32;
            let src = ((sy.min(height - 1) * width + sx.min(width - 1)) * 3) as usize;
            let dst = ty * INPUT + tx;
            for c in 0..3 {
                // RGB, because CLIP's processor is RGB — unlike YuNet, which wants BGR.
                out[c * plane + dst] = (pixels[src + c] as f32 / 255.0 - MEAN[c]) / STD[c];
            }
        }
    }
    out
}

#[derive(Debug, thiserror::Error)]
pub enum ClipError {
    #[error("{path}: {reason}")]
    Io { path: PathBuf, reason: String },
    #[error("{0} is not a vocabulary file this build can read")]
    Format(PathBuf),
    #[error(
        "{file} holds {in_file} phrases and this build expects {in_code} — the vocabulary was \
         edited without regenerating the file. Regenerate it with `cargo run -p chaff-faces \
         --example clip_vocab -- tokenizer.json text_model_int8.onnx models/clip/vocabulary.bin`."
    )]
    Mismatch {
        file: PathBuf,
        in_file: usize,
        in_code: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vocabulary with known vectors, so `rank` can be checked exactly.
    fn vocab() -> Vocabulary {
        let dim: usize = 4;
        // Flat and row-major, which is the shape the real file loads into — a fixture with
        // a different shape would not exercise the indexing `rank` does.
        let mut vectors = vec![0f32; VOCABULARY.len() * dim];
        for i in 0..VOCABULARY.len() {
            vectors[i * dim + (i % dim)] = 1.0;
        }
        Vocabulary { phrases: VOCABULARY.iter().map(|s| s.to_string()).collect(), vectors, dim }
    }

    #[test]
    fn ranking_returns_the_nearest_phrase_first() {
        let v = vocab();
        let ranked = v.rank(&[0.0, 1.0, 0.0, 0.0]);
        assert_eq!(ranked.len(), VOCABULARY.len());
        // Every phrase in this fixture whose index is 1 mod 4 points at axis 1.
        assert!(ranked[0].1 > 0.99, "got {:?}", ranked[0]);
        // And it is sorted descending.
        assert!(ranked.windows(2).all(|w| w[0].1 >= w[1].1));
    }

    #[test]
    fn ranking_is_deterministic() {
        // A tag list that reshuffles between runs makes a diff of two runs unreadable.
        let v = vocab();
        let a = v.rank(&[0.5, 0.5, 0.5, 0.5]);
        let b = v.rank(&[0.5, 0.5, 0.5, 0.5]);
        assert_eq!(a, b);
    }

    #[test]
    fn a_wrongly_sized_embedding_ranks_nothing() {
        // Returning an empty list is the honest answer. The alternative — comparing against
        // whatever happens to be in memory — produces confident nonsense.
        let v = vocab();
        assert!(v.rank(&[1.0, 2.0]).is_empty());
        assert!(v.rank(&[]).is_empty());
    }

    #[test]
    fn a_missing_or_corrupt_vocabulary_file_is_an_error() {
        // **The failure mode this prevents.** A truncated file of the right shape would load
        // and produce embeddings that mean nothing, and every tag would be plausible.
        assert!(matches!(
            Vocabulary::load(Path::new("/nonexistent-xyz.bin")),
            Err(ClipError::Io { .. })
        ));

        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.bin");
        std::fs::write(&bad, b"not a vocabulary file at all").unwrap();
        assert!(matches!(Vocabulary::load(&bad), Err(ClipError::Format(_))));

        // The right magic but the wrong length.
        std::fs::write(&bad, b"CHAFFCLIP1\x02\x00\x00\x00\x04\x00\x00\x00short").unwrap();
        assert!(matches!(Vocabulary::load(&bad), Err(ClipError::Format(_))));
    }

    #[test]
    fn a_vocabulary_round_trips_through_the_file_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.bin");

        let count = VOCABULARY.len();
        let dim = 4usize;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"CHAFFCLIP1");
        bytes.extend_from_slice(&(count as u32).to_le_bytes());
        bytes.extend_from_slice(&(dim as u32).to_le_bytes());
        for i in 0..count {
            for d in 0..dim {
                let v = if d == i % dim { 1.0f32 } else { 0.0 };
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        std::fs::write(&path, &bytes).unwrap();

        let v = Vocabulary::load(&path).unwrap();
        assert_eq!(v.len(), count);
        let ranked = v.rank(&[0.0, 1.0, 0.0, 0.0]);
        assert!(ranked[0].1 > 0.99);
    }

    #[test]
    fn the_vocabulary_is_closed_and_has_no_duplicates() {
        // **Closed on purpose.** CLIP returns whichever phrase is closest, so an open
        // vocabulary returns a confident answer for a photograph of anything at all. A
        // duplicate would also mean one phrase could never win.
        let mut seen = std::collections::BTreeSet::new();
        for phrase in VOCABULARY {
            assert!(seen.insert(*phrase), "duplicate phrase: {phrase}");
            assert!(!phrase.trim().is_empty());
        }
        assert!(VOCABULARY.len() >= 30, "a vocabulary this small cannot describe a library");
    }

    #[test]
    fn the_vocabulary_covers_the_things_people_filter_by() {
        // A vocabulary missing "a person" or "a beach" would make the fallback useless for
        // the two filters anyone actually wants.
        let joined = VOCABULARY.join(" | ");
        for expected in ["a person", "a beach", "a dog", "a sunset", "food"] {
            assert!(joined.contains(expected), "the vocabulary has no {expected}");
        }
    }

    #[test]
    fn preprocessing_produces_the_shape_clip_wants() {
        let pixels = vec![128u8; 100 * 60 * 3];
        let out = preprocess(&pixels, 100, 60);
        assert_eq!(out.len(), 3 * INPUT * INPUT);
        // A mid-grey pixel normalised with CLIP's constants lands near zero, not near 128.
        // Getting this wrong is an embedding that is stable, comparable and wrong.
        assert!(out.iter().all(|v| v.abs() < 1.0), "got {:?}", &out[..4]);
    }

    #[test]
    fn preprocessing_crops_rather_than_squashing() {
        // CLIP's transform is resize-shortest-side-then-centre-crop, not a plain squash.
        // Squashing changes the aspect of everything in the frame.
        let mut pixels = vec![0u8; 100 * 60 * 3];
        // A white band down the middle, which a centre crop keeps and a squash would not.
        for y in 0..60 {
            for x in 45..55 {
                let i = (y * 100 + x) * 3;
                pixels[i] = 255;
                pixels[i + 1] = 255;
                pixels[i + 2] = 255;
            }
        }
        let out = preprocess(&pixels, 100, 60);
        // The crop is the middle 60x60, so the band is around the centre of the output.
        let centre = (INPUT / 2) * INPUT + INPUT / 2;
        assert!(out[centre] > out[0], "the centre band must survive the crop");
    }

    #[test]
    fn a_wrongly_sized_buffer_is_an_error_not_a_panic() {
        let Some(model) = bundled_model() else { return };
        let Ok(clip) = Clip::from_file(&model) else { return };
        assert!(matches!(
            clip.embed(&[0u8; 10], 100, 100),
            Err(DetectError::BufferSize { .. })
        ));
    }

    #[test]
    fn the_committed_vocabulary_loads_if_it_is_present() {
        let Some(path) = bundled() else {
            eprintln!("SKIP: the vocabulary file has not been generated");
            return;
        };
        let v = Vocabulary::load(&path).expect("the committed vocabulary must load");
        assert_eq!(v.len(), VOCABULARY.len());
        assert_eq!(v.rank(&vec![0.0; DIMENSIONS]).len(), VOCABULARY.len());
    }
}
