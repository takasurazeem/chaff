//! Tagging a library: downscale, ask, store.
//!
//! # Why it is a pass, and why it is resumable
//!
//! A vision model takes one to two seconds per photograph. Three thousand photographs is
//! over an hour of GPU time, and a run that loses everything when the window closes is a run
//! nobody starts. Each photograph is committed as it is tagged, so stopping halfway keeps
//! what was done and starting again picks up where it left off.
//!
//! # What is sent, and what is not
//!
//! A **downscaled, re-encoded** copy. The model reads at most about a thousand pixels, so a
//! 45 MB raw is waste on every axis — bandwidth, GPU time, and the tokens that describe it.
//! Re-encoding rather than resizing the original also **drops EXIF**, so GPS never leaves
//! the machine even when the endpoint is on the LAN.
//!
//! # Queue on outage (#51)
//!
//! If the endpoint stops answering mid-run, the pass **stops** rather than failing each
//! remaining photograph one at a time. The work list is derived from the catalog, so
//! everything not yet tagged is still outstanding when the endpoint comes back — the queue
//! is the database, not a list in memory that dies with the process.

use std::path::Path;

use crate::catalog::store;
use crate::rusqlite::Connection;
use crate::egress::Policy;
use crate::{thumb, vlm};
use serde::Serialize;

/// How the image is prepared for the model.
///
/// 768 rather than the model's full input: a photograph at 768 pixels keeps everything a
/// tagger needs and costs a third of the tokens of one at 1600. Measured against a live
/// server, the difference in tag quality was not visible and the difference in time was.
const SEND_EDGE: u32 = 768;
const JPEG_QUALITY: u8 = 85;

/// What a tagging pass did.
#[derive(Debug, Clone, Serialize)]
pub struct TagPassReport {
    pub tagged: usize,
    pub remaining: usize,
    /// Photographs whose image could not be read.
    pub unreadable: usize,
    /// Photographs the model could not answer for.
    pub failed: usize,
    pub tags: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub elapsed_ms: u128,
    /// Set when the endpoint stopped answering, so the UI can say "stopped" rather than
    /// "finished" — the difference between a complete library and a third of one.
    pub stopped_because: Option<String>,
}

/// Run a tagging pass.
///
/// `limit` bounds how many photographs to do in one call, so a pass over a large library can
/// be done in pieces with feedback between them rather than as one silent hour.
pub fn run(
    conn: &mut Connection,
    library_id: i64,
    endpoint: &vlm::Endpoint,
    limit: usize,
    now: i64,
    on_progress: &mut dyn FnMut(usize, usize),
) -> Result<TagPassReport, String> {
    let started = std::time::Instant::now();
    // Every request goes through the chokepoint, so there is one list of what this
    // application connects to and one place to read it (#34).
    let policy = Policy::default();

    let pending = store::photos_needing_tags(conn, library_id, &endpoint.model)
        .map_err(|e| e.to_string())?;
    let total = pending.len();
    let batch: Vec<_> = pending.into_iter().take(limit.max(1)).collect();

    let mut report = TagPassReport {
        tagged: 0,
        remaining: total.saturating_sub(batch.len()),
        unreadable: 0,
        failed: 0,
        tags: 0,
        prompt_tokens: 0,
        completion_tokens: 0,
        elapsed_ms: 0,
        stopped_because: None,
    };

    for (i, (photo_id, path)) in batch.iter().enumerate() {
        on_progress(i, batch.len());

        let Some(jpeg) = prepare(Path::new(path)) else {
            report.unreadable += 1;
            continue;
        };

        match vlm::tag(&policy, endpoint, &vlm::TagRequest {
            image: jpeg,
            vocabulary: None,
            extra_instructions: None,
        }, 300) {
            Ok(result) => {
                let pairs: Vec<(String, f64)> = result
                    .tags
                    .iter()
                    .map(|t| (t.name.clone(), t.confidence as f64))
                    .collect();

                store::replace_tags(
                    conn,
                    *photo_id,
                    &pairs,
                    &endpoint.model,
                    result.description.as_deref(),
                    now,
                )
                .map_err(|e| e.to_string())?;

                report.tagged += 1;
                report.tags += pairs.len();
                report.prompt_tokens += result.prompt_tokens;
                report.completion_tokens += result.completion_tokens;
            }
            Err(vlm::VlmError::Unreachable { reason, .. }) => {
                // **Stop, do not grind.** If the endpoint is gone, every remaining
                // photograph would fail the same way after a timeout each. The work list is
                // the catalog, so nothing is lost by stopping — this is the queue-on-outage
                // behaviour, and it is a property of where the queue lives rather than a
                // feature bolted on.
                report.stopped_because = Some(reason);
                break;
            }
            Err(e) => {
                log::warn!("tagging {path} failed: {e}");
                report.failed += 1;
            }
        }
    }
    on_progress(batch.len(), batch.len());

    report.elapsed_ms = started.elapsed().as_millis();
    log::info!(
        "tag pass: {} tagged, {} failed, {} unreadable, {} tags, {} tokens, {:.1}s{}",
        report.tagged,
        report.failed,
        report.unreadable,
        report.tags,
        report.completion_tokens,
        report.elapsed_ms as f64 / 1000.0,
        report
            .stopped_because
            .as_deref()
            .map(|r| format!(" — stopped: {r}"))
            .unwrap_or_default(),
    );

    Ok(report)
}

/// Downscale and re-encode, which is also what strips the metadata.
fn prepare(path: &Path) -> Option<Vec<u8>> {
    let img = thumb::decode_source(path).ok()?;
    let small = img.resize(SEND_EDGE, SEND_EDGE, image::imageops::FilterType::Lanczos3).to_rgb8();

    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
        .encode(small.as_raw(), small.width(), small.height(), image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}

/// What an endpoint says about itself (#52).
///
/// # Why this is a real diagnostic and not a ping
///
/// "Is the server up?" is the least useful question. The failures that actually happen are:
/// the server is up but has no vision model loaded, the model is loaded but ignores
/// `response_format`, or it answers but spends its budget thinking. Each produces a
/// different symptom and a different fix, and a health check that only proves the port is
/// open sends the user looking in the wrong place.
#[derive(Debug, Clone, Serialize)]
pub struct EndpointReport {
    pub reachable: bool,
    /// Whether `/health` answered.
    pub healthy: bool,
    /// Model ids the endpoint advertises.
    pub models: Vec<String>,
    /// Whether a real photograph produced parseable tags.
    pub vision_works: bool,
    /// Whether the schema was honoured — a tag outside a supplied vocabulary means it was not.
    pub schema_enforced: bool,
    /// Tokens the model spent thinking, when it did. Zero is the good case.
    pub reasoning_tokens_wasted: u64,
    pub seconds_per_photo: f64,
    /// One line the user can act on.
    pub verdict: String,
}

/// Exercise an endpoint with a real request.
pub fn diagnose(endpoint: &vlm::Endpoint, fixture: Option<&Path>) -> EndpointReport {
    let mut report = EndpointReport {
        reachable: false,
        healthy: false,
        models: Vec::new(),
        vision_works: false,
        schema_enforced: false,
        reasoning_tokens_wasted: 0,
        seconds_per_photo: 0.0,
        verdict: String::new(),
    };

    let policy = Policy::default();
    report.healthy = vlm::health(&policy, endpoint, 10).is_ok();
    report.reachable = report.healthy || !report.models.is_empty();
    if !report.healthy {
        // A server with no `/health` may still serve; try the model list before giving up.
        report.reachable = vlm::list_models(&policy, endpoint, 10).is_ok();
        if !report.reachable {
            report.verdict = format!(
                "Nothing is answering at {}. Start the model server, or check the address.",
                endpoint.base
            );
            return report;
        }
    }

    report.models = vlm::list_models(&policy, endpoint, 10).unwrap_or_default();

    let Some(fixture) = fixture else {
        report.verdict = "The endpoint answers. No photograph was available to test vision with."
            .to_string();
        return report;
    };
    let Some(jpeg) = prepare(fixture) else {
        report.verdict = "The endpoint answers, but the test photograph could not be read."
            .to_string();
        return report;
    };

    // A vocabulary with one obviously-wrong option: a model that ignores the schema will
    // return something outside it, which is the check.
    let vocabulary = vec!["person".to_string(), "landscape".to_string()];

    let started = std::time::Instant::now();
    match vlm::tag(
        &policy,
        endpoint,
        &vlm::TagRequest {
            image: jpeg,
            vocabulary: Some(vocabulary.clone()),
            extra_instructions: None,
        },
        300,
    ) {
        Ok(result) => {
            report.seconds_per_photo = started.elapsed().as_secs_f64();
            report.vision_works = !result.tags.is_empty();
            report.schema_enforced =
                !result.tags.is_empty() && result.tags.iter().all(|t| vocabulary.contains(&t.name));
            report.reasoning_tokens_wasted =
                result.completion_tokens.saturating_sub(result.tags.len() as u64 * 12);

            report.verdict = if !report.vision_works {
                format!(
                    "{} answered but produced no tags. It is probably a text-only model — load a \
                     vision model and its projector.",
                    endpoint.model
                )
            } else if !report.schema_enforced {
                format!(
                    "{} tagged the photograph but ignored the vocabulary. Its server does not \
                     enforce `response_format`, so tags will be free-form and may vary between runs.",
                    endpoint.model
                )
            } else {
                format!(
                    "{} works: {:.1}s per photograph, {:.0} photographs per hour.",
                    endpoint.model,
                    report.seconds_per_photo,
                    if report.seconds_per_photo > 0.0 {
                        3600.0 / report.seconds_per_photo
                    } else {
                        0.0
                    }
                )
            };
        }
        Err(e) => {
            report.verdict = format!(
                "{} could not tag a photograph: {e}. Check that a vision model and its projector \
                 are both loaded.",
                endpoint.model
            );
        }
    }

    report
}
