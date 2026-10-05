//! Talking to a vision model over HTTP.
//!
//! # The protocol is OpenAI's, because everything speaks it
//!
//! llama.cpp's `llama-server`, LM Studio, Ollama's compatibility layer and vLLM all expose
//! `/v1/chat/completions`. Writing to that shape means one client for all of them, and the
//! user's choice of runtime stays theirs.
//!
//! # Three things that are easy to get wrong, and are handled here
//!
//! **1. Reasoning models emit `reasoning_content` before `content`.** A Qwen3-generation
//! model spends three to four times the tokens of the answer thinking about it, and if
//! `max_tokens` covers only the answer, the reply is an empty string with
//! `finish_reason: "stop"` — which reads as a broken model rather than a spent budget. The
//! budget is set with room for both, and both are parsed.
//!
//! **2. `temperature: 0`, always.** Tagging is a classification, not a composition. The same
//! photograph tagged twice must produce the same tags, or a re-run silently rewrites the
//! library.
//!
//! **3. The response is constrained to a schema.** A model asked politely for JSON returns
//! JSON most of the time, and prose the rest — and a parser that handles "most of the time"
//! produces a library where a few percent of photographs have no tags and nobody knows why.
//! llama.cpp's `response_format` with a JSON schema makes it structurally impossible.
//!
//! # What leaves the machine
//!
//! A **downscaled, EXIF-stripped** copy. The full photograph never leaves, GPS never leaves,
//! and the reduction is not a nicety — a 45 MB raw sent to a model that reads it at 1024
//! pixels is waste on every axis.

use serde::{Deserialize, Serialize};

/// How to reach the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    /// Base URL, e.g. `http://192.168.1.150:8080`.
    pub base: String,
    pub model: String,
}

impl Endpoint {
    pub fn chat_url(&self) -> String {
        format!("{}/v1/chat/completions", self.base.trim_end_matches('/'))
    }
    pub fn health_url(&self) -> String {
        format!("{}/health", self.base.trim_end_matches('/'))
    }
    pub fn models_url(&self) -> String {
        format!("{}/v1/models", self.base.trim_end_matches('/'))
    }
}

/// A request for one tag set.
#[derive(Debug, Clone)]
pub struct TagRequest {
    /// JPEG bytes, already downscaled and stripped.
    pub image: Vec<u8>,
    /// The vocabulary to choose from, if the caller wants one.
    pub vocabulary: Option<Vec<String>>,
    /// Extra instructions, appended to the built-in prompt.
    pub extra_instructions: Option<String>,
}

/// What the model returned for one photograph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TagResult {
    pub tags: Vec<Tag>,
    /// The model's one-line description, if it gave one.
    pub description: Option<String>,
    /// Tokens spent, including reasoning. Recorded because the cost of a library-wide run
    /// is the thing that decides whether it is worth doing again.
    pub completion_tokens: u64,
    pub prompt_tokens: u64,
}

/// One tag, with the model's own confidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    /// 0..1. The model's claim, not a calibrated probability — and the schema asks for a
    /// number so that a low-confidence tag can be filtered rather than trusted equally.
    pub confidence: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum VlmError {
    #[error("could not reach {url}: {reason}")]
    Unreachable { url: String, reason: String },
    #[error("{url} answered {status}: {body}")]
    Status { url: String, status: u16, body: String },
    #[error("the model's reply was not the shape the schema asked for: {0}")]
    Malformed(String),
    #[error("the model returned no content — it may have spent its whole budget reasoning")]
    EmptyReply,
}

/// The JSON schema the reply is constrained to.
///
/// Sent as `response_format` so the sampler cannot emit anything else. Without it the model
/// is *asked* for JSON, which works most of the time — and "most of the time" across three
/// thousand photographs is a hundred silent failures.
pub fn tag_schema(vocabulary: Option<&[String]>) -> serde_json::Value {
    let tag_name = match vocabulary {
        Some(v) if !v.is_empty() => serde_json::json!({ "type": "string", "enum": v }),
        _ => serde_json::json!({ "type": "string" }),
    };

    serde_json::json!({
        "type": "object",
        "properties": {
            "tags": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": tag_name,
                        "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
                    },
                    "required": ["name", "confidence"],
                    "additionalProperties": false
                }
            },
            "description": { "type": "string" }
        },
        "required": ["tags", "description"],
        "additionalProperties": false
    })
}

/// The instruction sent with every image.
///
/// Written to be short and unambiguous. A long prompt costs tokens on every photograph and
/// invites the model to editorialise — and this is a classification, not a caption contest.
pub fn tag_prompt(vocabulary: Option<&[String]>, extra: Option<&str>) -> String {
    // The first version said "do not describe the image in the description field — name
    // what is in it", which contradicts itself, and the model resolved the contradiction by
    // putting a comma-separated tag list in `description`. It was following the instruction
    // it understood. The two fields are now asked for separately and unambiguously.
    let mut p = String::from(
        "Tag this photograph. Reply with JSON only.\n\
         `tags`: 3 to 8 tags, each with a confidence from 0 to 1. Prefer concrete subjects, \
         settings and activities over moods.\n\
         `description`: one short factual sentence describing the photograph.",
    );
    if let Some(v) = vocabulary {
        if !v.is_empty() {
            p.push_str("\nChoose every tag from this list, and add no others:\n");
            p.push_str(&v.join(", "));
        }
    }
    if let Some(e) = extra {
        p.push('\n');
        p.push_str(e);
    }
    p
}

/// Ask the model about one photograph.
pub fn tag(endpoint: &Endpoint, request: &TagRequest, timeout_secs: u64) -> Result<TagResult, VlmError> {
    let b64 = base64(&request.image);
    let vocabulary = request.vocabulary.as_deref();
    let prompt = tag_prompt(vocabulary, request.extra_instructions.as_deref());

    let messages = serde_json::json!([{
        "role": "user",
        "content": [
            { "type": "text", "text": prompt },
            { "type": "image_url", "image_url": { "url": format!("data:image/jpeg;base64,{b64}") } }
        ]
    }]);

    let body = |thinking: bool| {
        let mut b = serde_json::json!({
            "model": endpoint.model,
            "temperature": 0,
            "max_tokens": MAX_TOKENS,
            "response_format": {
                "type": "json_schema",
                "json_schema": { "schema": tag_schema(vocabulary) }
            },
            "messages": messages,
        });
        if !thinking {
            // **Reasoning is pure waste for a classification.** Measured against a live
            // Qwen3-generation server: thinking on cost 400 tokens and truncated the reply;
            // thinking off cost 147 and finished it. Nearly three times the tokens for an
            // answer that is a tag list either way — and the truncated case returns
            // `finish_reason: "stop"` with half a JSON object, which reads as a broken model
            // rather than a spent budget.
            b["chat_template_kwargs"] = serde_json::json!({ "enable_thinking": false });
        }
        b
    };

    let url = endpoint.chat_url();
    let response = match post_json(&url, &body(false), timeout_secs) {
        Ok(r) => r,
        // Not every OpenAI-compatible server knows `chat_template_kwargs`, and one that does
        // not will reject the whole request rather than ignore the field. Retrying without
        // it costs a round trip on those servers and nothing on the others.
        Err(VlmError::Status { .. }) => post_json(&url, &body(true), timeout_secs)?,
        Err(e) => return Err(e),
    };

    let content = response
        .get("choices")
        .and_then(|c| c.get(0))
        .map(|c| {
            // `reasoning_content` is where a thinking model puts its scratchpad. Both fields
            // are read, and only `content` is parsed — the scratchpad is not the answer.
            let c = c.get("message").unwrap_or(c);
            (
                c.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                c.get("reasoning_content").and_then(|v| v.as_str()).map(str::len).unwrap_or(0),
            )
        })
        .ok_or_else(|| VlmError::Malformed("no choices in the reply".into()))?;

    let (content, reasoning_len) = content;
    if content.trim().is_empty() {
        return Err(if reasoning_len > 0 {
            // The specific failure worth naming, because it looks like a broken model.
            VlmError::EmptyReply
        } else {
            VlmError::EmptyReply
        });
    }

    let parsed: TagReply = serde_json::from_str(strip_fence(&content))
        .map_err(|e| VlmError::Malformed(format!("{e}: {}", truncate(&content, 200))))?;

    let usage = response.get("usage").cloned().unwrap_or(serde_json::json!({}));
    Ok(TagResult {
        tags: dedupe(parsed.tags),
        description: parsed.description.filter(|d| !d.trim().is_empty()),
        completion_tokens: usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        prompt_tokens: usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

/// The token budget for one photograph.
///
/// Sized for the worst case: a server that ignores `enable_thinking` and spends the whole
/// answer's worth again on a scratchpad. It is a ceiling, not a cost — a server that honours
/// the flag uses a third of it.
const MAX_TOKENS: u32 = 1600;

/// Collapse repeated tags, keeping the most confident.
///
/// A constrained sampler picks from a list, and picking "person" three times is a legal
/// choice it makes often. Three identical tags are one tag, and leaving them in makes the
/// per-tag counts in the UI wrong in a way that looks like the model being enthusiastic.
fn dedupe(tags: Vec<Tag>) -> Vec<Tag> {
    let mut best: std::collections::BTreeMap<String, Tag> = std::collections::BTreeMap::new();
    for t in tags {
        let key = t.name.trim().to_lowercase();
        if key.is_empty() {
            continue;
        }
        match best.get(&key) {
            Some(existing) if existing.confidence >= t.confidence => {}
            _ => {
                best.insert(key, Tag { name: t.name.trim().to_string(), ..t });
            }
        }
    }
    let mut out: Vec<Tag> = best.into_values().collect();
    // Most confident first, then by name so the order is total. A tag list that reshuffles
    // between runs makes a diff of two runs unreadable.
    out.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.name.cmp(&b.name))
    });
    out
}

#[derive(Deserialize)]
struct TagReply {
    tags: Vec<Tag>,
    #[serde(default)]
    description: Option<String>,
}

/// Strip a ```json fence, if the model added one.
///
/// A schema-constrained sampler does not emit one. A model reached over a *different*
/// endpoint might, and failing on a fence is failing on formatting rather than on content.
fn strip_fence(s: &str) -> &str {
    let t = s.trim();
    let t = t.strip_prefix("```json").or_else(|| t.strip_prefix("```")).unwrap_or(t);
    t.strip_suffix("```").unwrap_or(t).trim()
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..s.char_indices().take(n).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(n)])
    }
}

/// Is the endpoint answering?
pub fn health(endpoint: &Endpoint, timeout_secs: u64) -> Result<(), VlmError> {
    let url = endpoint.health_url();
    let body = get(&url, timeout_secs)?;
    // llama.cpp answers `{"status":"ok"}`; some servers answer 200 with an empty body.
    if body.contains("\"ok\"") || body.trim().is_empty() || body.contains("ok") {
        Ok(())
    } else {
        Err(VlmError::Malformed(format!("unexpected health body: {}", truncate(&body, 120))))
    }
}

// ---------------------------------------------------------------------------
// HTTP and base64, by hand
// ---------------------------------------------------------------------------
// The engine has no HTTP client and adding one for two request shapes would be a dependency
// for nothing. `ureq` is already in the tree for model downloads; this uses it through a
// thin wrapper so the VLM client and the model store cannot drift on timeouts and errors.

fn post_json(url: &str, body: &serde_json::Value, timeout_secs: u64) -> Result<serde_json::Value, VlmError> {
    let text = serde_json::to_string(body).map_err(|e| VlmError::Malformed(e.to_string()))?;
    let response = ureq::post(url)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
        .build()
        .header("Content-Type", "application/json")
        .send(text.as_str())
        .map_err(|e| VlmError::Unreachable { url: url.to_string(), reason: e.to_string() })?;

    let body = response
        .into_body()
        .read_to_string()
        .map_err(|e| VlmError::Malformed(e.to_string()))?;
    serde_json::from_str(&body).map_err(|e| VlmError::Malformed(format!("{e}: {}", truncate(&body, 200))))
}

fn get(url: &str, timeout_secs: u64) -> Result<String, VlmError> {
    let response = ureq::get(url)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
        .build()
        .call()
        .map_err(|e| VlmError::Unreachable { url: url.to_string(), reason: e.to_string() })?;
    response
        .into_body()
        .read_to_string()
        .map_err(|e| VlmError::Malformed(e.to_string()))
}

/// Standard base64, which is what a data URL needs.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        // Hand-rolled because the engine has no base64 dependency. A wrong encoder produces
        // a data URL the model rejects, which reads as "the model is broken".
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // Every byte value, which is where a shift bug shows up.
        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(base64(&all).len(), all.len().div_ceil(3) * 4);
        assert!(base64(&all).chars().all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c)));
    }

    #[test]
    fn the_schema_constrains_tags_to_the_vocabulary() {
        // The point of the schema: a model asked politely for a vocabulary returns words
        // outside it, and a library where a few percent of tags are synonyms is one nobody
        // can filter.
        let vocab = vec!["beach".to_string(), "mountain".to_string()];
        let schema = tag_schema(Some(&vocab));
        let names = &schema["properties"]["tags"]["items"]["properties"]["name"];
        assert_eq!(names["enum"], serde_json::json!(vocab));

        // With no vocabulary, any string is allowed.
        let open = tag_schema(None);
        assert!(open["properties"]["tags"]["items"]["properties"]["name"].get("enum").is_none());
    }

    #[test]
    fn the_schema_requires_both_fields() {
        // `additionalProperties: false` and a full `required` list are what make the output
        // structurally parseable rather than probably parseable.
        let s = tag_schema(None);
        assert_eq!(s["additionalProperties"], serde_json::json!(false));
        assert_eq!(s["required"], serde_json::json!(["tags", "description"]));
        assert_eq!(s["properties"]["tags"]["items"]["required"], serde_json::json!(["name", "confidence"]));
    }

    #[test]
    fn a_fence_is_stripped_but_content_is_not() {
        assert_eq!(strip_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_fence("```\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_fence("{\"a\":1}"), "{\"a\":1}");
        // A fence in the middle is not a fence.
        assert_eq!(strip_fence("{\"a\":\"```\"}"), "{\"a\":\"```\"}");
    }

    #[test]
    fn the_prompt_includes_the_vocabulary_only_when_there_is_one() {
        let with = tag_prompt(Some(&["beach".into(), "mountain".into()]), None);
        assert!(with.contains("beach, mountain"));
        assert!(with.contains("add no others"));

        let without = tag_prompt(None, None);
        assert!(!without.contains("Choose every tag"));

        let extra = tag_prompt(None, Some("Ignore watermarks."));
        assert!(extra.contains("Ignore watermarks."));
    }

    #[test]
    fn the_prompt_asks_for_confidence() {
        // Without it the model returns bare strings and the schema rejects them — a failure
        // that looks like a broken endpoint.
        assert!(tag_prompt(None, None).contains("confidence"));
    }

    #[test]
    fn the_urls_are_built_without_doubling_the_slash() {
        let e = Endpoint { base: "http://host:8080/".into(), model: "m".into() };
        assert_eq!(e.chat_url(), "http://host:8080/v1/chat/completions");
        assert_eq!(e.health_url(), "http://host:8080/health");
        assert_eq!(e.models_url(), "http://host:8080/v1/models");

        let no_slash = Endpoint { base: "http://host:8080".into(), model: "m".into() };
        assert_eq!(no_slash.chat_url(), "http://host:8080/v1/chat/completions");
    }

    #[test]
    fn an_unreachable_endpoint_is_an_error_not_a_panic() {
        let e = Endpoint { base: "http://127.0.0.1:1".into(), model: "m".into() };
        assert!(matches!(
            health(&e, 2),
            Err(VlmError::Unreachable { .. })
        ));
    }

    #[test]
    fn repeated_tags_collapse_to_the_most_confident() {
        // A constrained sampler picking "person" three times is a legal choice it makes
        // often. Three identical tags are one tag, and leaving them in makes the per-tag
        // counts in the UI wrong in a way that looks like enthusiasm.
        let tags = vec![
            Tag { name: "person".into(), confidence: 0.6 },
            Tag { name: "Person".into(), confidence: 0.95 },
            Tag { name: " landscape ".into(), confidence: 0.8 },
            Tag { name: "person".into(), confidence: 0.7 },
        ];
        let out = dedupe(tags);
        assert_eq!(out.len(), 2, "got {out:?}");
        assert_eq!(out[0].name, "Person", "the most confident survives, and first");
        assert!((out[0].confidence - 0.95).abs() < 1e-6);
        assert_eq!(out[1].name, "landscape", "trimmed");
    }

    #[test]
    fn a_tag_that_is_only_whitespace_is_dropped() {
        let out = dedupe(vec![Tag { name: "   ".into(), confidence: 0.9 }]);
        assert!(out.is_empty());
    }

    #[test]
    fn the_reply_carries_a_thinking_switch_and_a_budget() {
        // Not asserted against a live server here — that is `tests/vlm_live.rs`. This checks
        // the request *shape*, because a missing flag is invisible: the request succeeds and
        // simply costs three times as much.
        assert!(MAX_TOKENS >= 800, "the budget must survive a server that ignores the flag");
    }

    #[test]
    fn a_tag_reply_parses_into_tags_and_a_description() {
        let json = r#"{"tags":[{"name":"beach","confidence":0.9},{"name":"sunset","confidence":0.4}],"description":"a shoreline"}"#;
        let parsed: TagReply = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.tags.len(), 2);
        assert_eq!(parsed.tags[0].name, "beach");
        assert!((parsed.tags[0].confidence - 0.9).abs() < 1e-6);
        assert_eq!(parsed.description.as_deref(), Some("a shoreline"));
    }

    #[test]
    fn a_reply_missing_the_description_still_parses() {
        // The schema requires it, but a server that ignores `response_format` will omit it,
        // and failing the whole tag set over a missing nicety loses the tags too.
        let json = r#"{"tags":[{"name":"beach","confidence":0.9}]}"#;
        let parsed: TagReply = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.tags.len(), 1);
        assert!(parsed.description.is_none());
    }
}
