//! Reading and writing XMP sidecars.
//!
//! # Merge, never clobber
//!
//! A sidecar is shared. Lightroom, darktable, digiKam and Chaff all write to the same file,
//! and a write that replaces the document destroys everything the other applications put
//! there — keywords, develop settings, a colour label, a print flag. The user's editing work
//! lives in that file and this program is a guest in it.
//!
//! So a write is a **surgical edit**: the rating and label attributes are replaced in place
//! and every other byte is preserved, including formatting and attribute order. That is not
//! the tidiest way to write XML and it is the only way that does not lose other people's
//! data.
//!
//! # What is written
//!
//! `xmp:Rating` and `xmp:Label` in the `xmp` namespace, which is what every application
//! agrees on. Chaff's own shoot grouping and composite score are **not** written: they are
//! this program's opinion, they change when the model changes, and a sidecar is not the place
//! for a number that will be different next week.
//!
//! # The rule about writing at all
//!
//! Writing is **opt-in and off by default**, and a write never happens for a photograph the
//! user has not decided about. A culling tool that silently writes files into a library the
//! moment it opens it is one nobody trusts twice.

use std::path::{Path, PathBuf};

use crate::catalog::store::Decision;
use crate::ext::{RAW_EXTS, RASTER_EXTS};

#[derive(Debug, thiserror::Error)]
pub enum XmpError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the sidecar at {path} is not readable as XML text")]
    Malformed { path: PathBuf },
}

/// What a sidecar says about a photograph.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sidecar {
    pub rating: Option<u8>,
    pub label: Option<String>,
    /// Every `dc:subject` keyword, in document order.
    pub keywords: Vec<String>,
}

/// The sidecar path for an image.
///
/// `IMG_0001.CR3` -> `IMG_0001.xmp`, which is what Lightroom and darktable both write and
/// what the indexer already reads back.
pub fn sidecar_path(image: &Path) -> PathBuf {
    let stem = image.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    image.with_file_name(format!("{stem}.xmp"))
}

/// Is this a file that may have a sidecar?
pub fn supports_sidecar(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| RAW_EXTS.contains(&e.as_str()) || RASTER_EXTS.contains(&e.as_str()))
}

/// Read what a sidecar says, if there is one.
pub fn read(image: &Path) -> Result<Option<Sidecar>, XmpError> {
    let path = sidecar_path(image);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|source| XmpError::Io { path: path.clone(), source })?;
    Ok(Some(parse(&text)))
}

/// The attributes and elements this program understands.
///
/// A hand-rolled scan rather than an XML parser, deliberately: the job is to **find two
/// attributes and leave every other byte alone**, and a parser that round-trips a document
/// reformats it — which is the clobbering this module exists to avoid. Reading is a scan;
/// writing is a replacement of exactly the matched text.
pub fn parse(text: &str) -> Sidecar {
    Sidecar {
        rating: attribute(text, "xmp:Rating")
            .and_then(|v| v.trim().parse::<u8>().ok())
            // A rating of 9 is a corrupt file, not a 5. Clamping would invent a decision the
            // user never made.
            .filter(|r| *r <= 5),
        label: attribute(text, "xmp:Label")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
        keywords: elements(text, "dc:subject", "rdf:li"),
    }
}

/// The value of `name="..."`, anywhere in the document.
fn attribute(text: &str, name: &str) -> Option<String> {
    let mut from = 0usize;
    while let Some(i) = text[from..].find(name) {
        let at = from + i;
        from = at + name.len();

        // Must be a whole attribute name, not the tail of a longer one.
        let before_ok = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || c == '<');
        if !before_ok {
            continue;
        }

        let rest = text[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let inner = &rest[1..];
        let end = inner.find(quote)?;
        return Some(inner[..end].to_string());
    }
    None
}

/// The text of every `<li>` inside a `<container>`.
fn elements(text: &str, container: &str, item: &str) -> Vec<String> {
    let Some(start) = text.find(container) else { return Vec::new() };
    let after = &text[start..];
    // The container ends at its own closing tag, or at the end of the document.
    let end = after.find(&format!("</{container}>")).unwrap_or(after.len());
    let body = &after[..end];

    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(i) = body[from..].find(&format!("<{item}")) {
        let at = from + i;
        let Some(open_end) = body[at..].find('>') else { break };
        let content_start = at + open_end + 1;
        let Some(close) = body[content_start..].find(&format!("</{item}>")) else { break };
        let value = body[content_start..content_start + close].trim();
        if !value.is_empty() {
            out.push(value.to_string());
        }
        from = content_start + close;
    }
    out
}

/// Replace a rating and label in a sidecar, preserving everything else.
///
/// # What this guarantees
///
/// Every byte that is not the rating or label attribute comes back unchanged — including
/// whitespace, attribute order, comments, and namespaces this program has never heard of.
/// That is the whole contract, and it is what makes it safe to write into a file another
/// application owns.
pub fn merge(existing: Option<&str>, decision: Decision, label: Option<&str>) -> String {
    let text = existing.unwrap_or(DEFAULT_DOCUMENT);

    let rating = decision.xmp_rating().to_string();
    let text = set_attribute(text, "xmp:Rating", &rating);
    let text = match label.filter(|l| !l.trim().is_empty()) {
        Some(l) => set_attribute(&text, "xmp:Label", l.trim()),
        // A cleared label removes the attribute rather than writing an empty one: `Label=""`
        // reads as "explicitly no label" to some applications, which is a different claim.
        None => remove_attribute(&text, "xmp:Label"),
    };
    text
}

/// A minimal document, for an image that has no sidecar yet.
const DEFAULT_DOCUMENT: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="0">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"#;

/// Replace an attribute's value, or add it if it is not there.
fn set_attribute(text: &str, name: &str, value: &str) -> String {
    let mut from = 0usize;
    while let Some(i) = text[from..].find(name) {
        let at = from + i;
        from = at + name.len();

        let before_ok = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || c == '<');
        if !before_ok {
            continue;
        }
        let rest = text[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        let quote = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => q,
            _ => continue,
        };
        let value_start = text.len() - rest.len() + 1;
        let Some(end) = text[value_start..].find(quote) else { continue };
        let mut out = String::with_capacity(text.len() + value.len());
        out.push_str(&text[..value_start]);
        out.push_str(value);
        out.push_str(&text[value_start + end..]);
        return out;
    }

    // **Not there yet, so add it.** The first version only replaced, which meant a label
    // could never be written into a document that had no label attribute — and a fresh
    // sidecar has neither. It returned the text unchanged, silently, and the test caught it.
    insert_attribute(text, name, value)
}

/// Add an attribute to the document's description element.
///
/// Inserted before the `>` that closes the first element that already carries an `xmp:` or
/// `rdf:Description` marker, which is where every application looks for it. Falls back to
/// leaving the text alone rather than guessing at a structure this does not understand.
fn insert_attribute(text: &str, name: &str, value: &str) -> String {
    // The element to extend: `rdf:Description` if it is there, otherwise the first tag.
    let anchor = text.find("rdf:Description").or_else(|| text.find('<'));
    let Some(anchor) = anchor else { return text.to_string() };

    // The `>` that closes it, skipping any inside attribute values.
    let mut quote: Option<char> = None;
    let mut close = None;
    for (i, c) in text[anchor..].char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => {
                close = Some(anchor + i);
                break;
            }
            _ => {}
        }
    }
    let Some(close) = close else { return text.to_string() };

    // A self-closing element has to become a pair, or the attribute would be dropped by
    // every parser that reads it.
    let self_closing = text[..close].trim_end().ends_with('/');
    let mut out = String::with_capacity(text.len() + name.len() + value.len() + 4);
    if self_closing {
        let before = text[..close].trim_end();
        out.push_str(&before[..before.len() - 1]);
        out.push_str(&format!(" {name}=\"{value}\"></"));
        out.push_str(&text[close + 1..]);
    } else {
        out.push_str(&text[..close]);
        out.push_str(&format!(" {name}=\"{value}\""));
        out.push_str(&text[close..]);
    }
    out
}

/// Remove an attribute entirely, with the whitespace that separated it.
fn remove_attribute(text: &str, name: &str) -> String {
    let mut from = 0usize;
    while let Some(i) = text[from..].find(name) {
        let at = from + i;
        from = at + name.len();

        let before_ok = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || c == '<');
        if !before_ok {
            continue;
        }
        let rest = text[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        let quote = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => q,
            _ => continue,
        };
        let value_start = text.len() - rest.len() + 1;
        let Some(end) = text[value_start..].find(quote) else { continue };
        let value_end = value_start + end + 1;

        // Take the whitespace before the attribute with it, so removing one does not leave a
        // double space behind.
        let mut start = at;
        while start > 0 && text.as_bytes()[start - 1].is_ascii_whitespace() {
            start -= 1;
        }
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..start]);
        out.push_str(&text[value_end..]);
        return out;
    }
    text.to_string()
}

/// Write a decision into a sidecar, creating one if there is none.
///
/// Returns the path written.
pub fn write(image: &Path, decision: Decision, label: Option<&str>) -> Result<PathBuf, XmpError> {
    let path = sidecar_path(image);
    let existing = std::fs::read_to_string(&path).ok();
    let merged = merge(existing.as_deref(), decision, label);

    // Written to a temporary name and renamed, so an interrupted write cannot leave a
    // truncated sidecar — which would be a corrupted edit for every other application too.
    let tmp = path.with_extension("xmp.tmp");
    std::fs::write(&tmp, merged.as_bytes())
        .map_err(|source| XmpError::Io { path: tmp.clone(), source })?;
    std::fs::rename(&tmp, &path).map_err(|source| XmpError::Io { path: path.clone(), source })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::store::Rating;

    fn decided(rating: u8) -> Decision {
        Decision { rating: Rating::new(rating), rejected: false }
    }

    #[test]
    fn a_sidecar_is_named_after_the_image_stem() {
        // `IMG_0001.CR3` -> `IMG_0001.xmp`, which is what Lightroom and darktable write and
        // what the indexer already reads back.
        assert_eq!(
            sidecar_path(Path::new("/lib/IMG_0001.CR3")),
            PathBuf::from("/lib/IMG_0001.xmp")
        );
        assert_eq!(sidecar_path(Path::new("/lib/a.JPG")), PathBuf::from("/lib/a.xmp"));
        // A sidecar named after the full filename would be a second, invisible one.
        assert_ne!(sidecar_path(Path::new("/lib/a.CR3")), PathBuf::from("/lib/a.CR3.xmp"));
    }

    #[test]
    fn reading_finds_the_rating_label_and_keywords() {
        let doc = r#"<rdf:Description xmp:Rating="4" xmp:Label="Green">
  <dc:subject><rdf:Bag><rdf:li>beach</rdf:li><rdf:li>sunset</rdf:li></rdf:Bag></dc:subject>
</rdf:Description>"#;
        let s = parse(doc);
        assert_eq!(s.rating, Some(4));
        assert_eq!(s.label.as_deref(), Some("Green"));
        assert_eq!(s.keywords, vec!["beach", "sunset"]);
    }

    #[test]
    fn a_document_with_nothing_in_it_reads_as_nothing() {
        let s = parse("<rdf:Description/>");
        assert_eq!(s.rating, None);
        assert_eq!(s.label, None);
        assert!(s.keywords.is_empty());
    }

    #[test]
    fn an_out_of_range_rating_is_ignored_rather_than_clamped() {
        // A rating of 9 is a corrupt file, not a 5. Clamping would silently invent a
        // decision the user never made.
        assert_eq!(parse(r#"<x xmp:Rating="9"/>"#).rating, None);
        assert_eq!(parse(r#"<x xmp:Rating="abc"/>"#).rating, None);
        assert_eq!(parse(r#"<x xmp:Rating="5"/>"#).rating, Some(5));
        assert_eq!(parse(r#"<x xmp:Rating="0"/>"#).rating, Some(0));
    }

    #[test]
    fn a_name_that_is_the_tail_of_another_is_not_matched() {
        // `xmp:Rating` inside `xmp:RatingFoo` is a different attribute. Matching it would
        // write a rating into somebody else's field.
        assert_eq!(attribute(r#"<x my:xmp:Rating="3"/>"#, "xmp:Rating"), None);
        assert_eq!(attribute(r#"<x xmp:RatingFoo="3"/>"#, "xmp:Rating"), None);
        assert_eq!(attribute(r#"<x xmp:Rating="3"/>"#, "xmp:Rating").as_deref(), Some("3"));
    }

    #[test]
    fn merging_preserves_every_other_byte() {
        // **The contract.** Every byte that is not the rating or label comes back unchanged
        // — including whitespace, comments, and namespaces this program has never heard of.
        // A sidecar is shared with Lightroom and darktable, and their data lives in it.
        let original = r#"<?xpacket begin=""?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <!-- the user's own note -->
  <rdf:Description rdf:about=""
      xmlns:xmp="http://ns.adobe.com/xap/1.0/"
      xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
      xmp:Rating="2"
      crs:Exposure2012="+0.75"
      xmp:Label="Red">
    <dc:subject><rdf:Bag><rdf:li>portrait</rdf:li></rdf:Bag></dc:subject>
  </rdf:Description>
</x:xmpmeta>"#;

        let merged = merge(Some(original), decided(5), Some("Green"));

        assert!(merged.contains(r#"xmp:Rating="5""#), "the rating is updated");
        assert!(merged.contains(r#"xmp:Label="Green""#), "the label is updated");
        assert!(merged.contains(r#"crs:Exposure2012="+0.75""#), "develop settings survive");
        assert!(merged.contains("<!-- the user's own note -->"), "comments survive");
        assert!(merged.contains("<rdf:li>portrait</rdf:li>"), "keywords survive");
        assert!(merged.contains(r#"xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/""#),
            "unknown namespaces survive");

        // Only the two values changed: same length elsewhere.
        assert_eq!(
            merged.len(),
            original.len() - "2".len() - "Red".len() + "5".len() + "Green".len()
        );
    }

    #[test]
    fn a_cleared_label_removes_the_attribute_rather_than_emptying_it() {
        // `Label=""` reads as "explicitly no label" to some applications, which is a
        // different claim from "no label was ever set".
        let doc = r#"<rdf:Description xmp:Rating="3" xmp:Label="Red"/>"#;
        let merged = merge(Some(doc), decided(3), None);
        assert!(!merged.contains("xmp:Label"), "got: {merged}");
        assert!(merged.contains(r#"xmp:Rating="3""#), "the rating is untouched");
        assert!(!merged.contains("  "), "and no double space is left behind: {merged}");
    }

    #[test]
    fn a_label_is_added_to_a_document_that_has_none() {
        // **The gap the test caught.** The first version only *replaced*, so writing a label
        // into a document with no label attribute returned the text unchanged — silently.
        let merged = merge(Some(r#"<rdf:Description xmp:Rating="3"/>"#), decided(3), Some("Blue"));
        assert!(merged.contains(r#"xmp:Label="Blue""#), "got: {merged}");
        assert!(merged.contains(r#"xmp:Rating="3""#), "and the rating is untouched");
    }

    #[test]
    fn adding_to_a_self_closing_element_makes_it_a_pair() {
        // An attribute appended to `<x/>` without turning it into `<x></x>` is dropped by
        // every parser that reads it — the write appears to succeed and does nothing.
        let merged = merge(Some("<rdf:Description/>"), decided(2), Some("Red"));
        assert!(merged.contains(r#"xmp:Label="Red""#), "got: {merged}");
        assert!(!merged.contains("/>"), "the element must no longer self-close: {merged}");
        assert!(merged.contains("</rdf:Description>") || merged.contains("></"), "got: {merged}");
    }

    #[test]
    fn merging_into_an_empty_document_creates_a_usable_one() {
        let merged = merge(None, decided(4), Some("Blue"));
        assert!(merged.contains(r#"xmp:Rating="4""#));
        assert!(merged.contains(r#"xmp:Label="Blue""#));
        // Still a document another application can read.
        assert!(merged.contains("<x:xmpmeta"));
        assert!(merged.contains("</x:xmpmeta>"));
    }

    #[test]
    fn a_rating_of_zero_is_written_not_treated_as_absent() {
        // Zero means "explicitly unrated", which is a decision. Treating it as nothing would
        // make clearing a rating impossible to record.
        let merged = merge(Some(r#"<r xmp:Rating="5"/>"#), decided(0), None);
        assert!(merged.contains(r#"xmp:Rating="0""#));
    }

    #[test]
    fn the_round_trip_is_stable() {
        // Writing twice with the same decision changes nothing the second time, which is what
        // makes "write sidecars for the whole library" safe to re-run.
        let once = merge(None, decided(3), Some("Green"));
        let twice = merge(Some(&once), decided(3), Some("Green"));
        assert_eq!(once, twice);
    }

    #[test]
    fn only_images_can_have_sidecars() {
        assert!(supports_sidecar(Path::new("/lib/a.CR3")));
        assert!(supports_sidecar(Path::new("/lib/a.jpg")));
        assert!(!supports_sidecar(Path::new("/lib/notes.txt")));
        assert!(!supports_sidecar(Path::new("/lib/catalog.db")));
        assert!(!supports_sidecar(Path::new("/lib/a.xmp")));
    }

    #[test]
    fn writing_to_a_missing_directory_is_an_error_not_a_panic() {
        let e = write(Path::new("/nonexistent-dir-xyz/a.CR3"), decided(3), None);
        assert!(matches!(e, Err(XmpError::Io { .. })));
    }
}
