//! Readers: signal detectors over `Event`s → `Annotation`s.
//!
//! A reader is the unit of *meaning extraction*. It reads transcript events and
//! emits annotations — each a (detector, anchor-span, signal) finding. This is
//! the Rust port of the Python lab's reader-set, promoted under the gate:
//! a reader earns its place here by being PROVEN GREEN in `/python` first, not
//! by feeling ready.
//!
//! The first promoted reader is `emojikey` — pure regex, no model, no GPU. It
//! HARVESTS inline `[ME|…]~[CONTENT|…]~[YOU|…]` keys already present in the text
//! (pleeb's Day-26 idea: the conversation already carries them). It is the one
//! reader that matches a precise SUBSTRING, so its annotation gets a true
//! `oa:TextPositionSelector` span — exercising the span-anchor path end to end.

use crate::{Event, SourceKind};
use std::collections::BTreeMap;

/// A single signal-leg value, carrying its RDF datatype intent.
///
/// The projector must NOT guess a datatype from a string — "8" is a meaningful
/// string in one reader (an emoji-count glyph) and a number in another (a wave
/// magnitude); only the READER knows which. So a reader that emits a numeric leg
/// declares it here, and text stays text by default (lossless, never coerced).
/// Without this, a magnitude projected as a plain string literal makes
/// `FILTER(?mag > 5)` a STRING comparison that silently returns wrong rows —
/// the exact "looks-like-it-works" trap. `Decimal` holds its lexical form (not
/// an f64) so precision is exact AND the value stays `Ord`/`Eq`/`Hash` for the
/// deterministic-IRI discipline (f64 is neither `Ord` nor hashable).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignalValue {
    /// A verbatim text payload. The default — never coerced to a number.
    Text(String),
    /// An integer leg → projects as an `xsd:integer` typed literal.
    Int(i64),
    /// A decimal leg (lexical form preserved) → projects as `xsd:decimal`.
    Decimal(String),
}

impl SignalValue {
    /// The lexical form for hashing / N-Triples emission.
    pub fn lexical(&self) -> String {
        match self {
            SignalValue::Text(s) => s.clone(),
            SignalValue::Int(n) => n.to_string(),
            SignalValue::Decimal(s) => s.clone(),
        }
    }
    /// A stable type tag so `Text("8")` and `Int(8)` are DISTINCT findings —
    /// folded into the annotation hash and the projected datatype.
    pub fn type_tag(&self) -> u8 {
        match self {
            SignalValue::Text(_) => 0,
            SignalValue::Int(_) => 1,
            SignalValue::Decimal(_) => 2,
        }
    }
    /// The xsd datatype IRI for a typed leg, or None for plain text.
    pub fn xsd_datatype(&self) -> Option<&'static str> {
        match self {
            SignalValue::Text(_) => None,
            SignalValue::Int(_) => Some("http://www.w3.org/2001/XMLSchema#integer"),
            SignalValue::Decimal(_) => Some("http://www.w3.org/2001/XMLSchema#decimal"),
        }
    }
}

/// Convenience: a plain-text leg (the common case — keeps reader code terse).
impl From<&str> for SignalValue {
    fn from(s: &str) -> Self {
        SignalValue::Text(s.to_string())
    }
}
impl From<String> for SignalValue {
    fn from(s: String) -> Self {
        SignalValue::Text(s)
    }
}

/// One finding: a detector observed a signal anchored to a span of one event.
/// The Rust mirror of the Python `MonologRow`, trimmed to what the projector
/// needs. `signal` is an ordered string→SignalValue map so projection is
/// deterministic (BTreeMap = stable key order = stable hashes = idempotent IRIs)
/// AND numeric legs carry their datatype (see `SignalValue`).
#[derive(Debug, Clone)]
pub struct Annotation {
    /// Which reader produced this (the detector name).
    pub reader: String,
    /// The finding kind, e.g. "emojikey/harvest". Becomes the reified proposition.
    pub event_type: String,
    /// RFC-3339 timestamp of the source event (prov:generatedAtTime).
    pub ts: Option<String>,
    /// The event this anchors to (its stable id = the join key back to source).
    pub event_id: String,
    /// Sub-event span [start, end) in the event's text — a true locator.
    pub start: usize,
    pub end: usize,
    /// Where in the source the match landed: AUTHORED (live emission) vs
    /// TOOL_RESULT (quoted/pasted). None when the adapter didn't record
    /// provenance. Carried, never dropped — downstream filters, not the reader.
    pub source_kind: Option<SourceKind>,
    /// The signal payload: the verbatim finding, lossless. Ordered for stable
    /// IRIs. Values are `SignalValue` so numeric legs carry their datatype;
    /// text legs (the emojikey case) are `SignalValue::Text`, unchanged in wire.
    pub signal: BTreeMap<String, SignalValue>,
}

/// The emojikey shape: three `[LABEL|payload]` segments (ME, CONTENT, YOU) joined
/// by `~`. Fixed label spine, freeform payload. Ported verbatim from the proven
/// Python `_KEY` regex (case-insensitive, payload = anything but a closing `]`).
const ME: &str = "ME";
const CONTENT: &str = "CONTENT";
const YOU: &str = "YOU";

/// Harvest inline `ME|CONTENT|YOU` emojikeys from an event's text.
///
/// Returns one `Annotation` per key found, with a TRUE byte span (`start`/`end`)
/// of the matched key within the event's text. We hand-scan rather than pull in
/// a regex crate: the grammar is a fixed 3-segment spine, so a small state walk
/// is both dependency-free and exact about the span (the Python reader leans on
/// `re`; here the engine stays lean — a reader this simple needs no engine dep).
pub fn emojikey_read(events: &[Event]) -> Vec<Annotation> {
    let mut out = Vec::new();
    for e in events {
        let Some(text) = &e.text else { continue };
        let mut search_from = 0;
        while let Some((start, end, me, content, you)) = next_key(text, search_from) {
            // emojikey legs are all TEXT payloads (the emoji/glyph strings are
            // meaningfully strings, not numbers — a magnitude reader would emit
            // SignalValue::Int/Decimal instead). Lossless, no coercion.
            let mut signal: BTreeMap<String, SignalValue> = BTreeMap::new();
            signal.insert("raw".to_string(), text[start..end].into());
            signal.insert("me".to_string(), me.into());
            signal.insert("content".to_string(), content.into());
            signal.insert("you".to_string(), you.into());
            signal.insert("voice".to_string(), e.role.clone().into());
            out.push(Annotation {
                reader: "emojikey".to_string(),
                event_type: "emojikey/harvest".to_string(),
                ts: e.timestamp.clone(),
                event_id: e.event_id.clone(),
                start,
                end,
                // classify by where the match STARTS (its origin block)
                source_kind: e.source_kind_at(start),
                signal,
            });
            // advance past this match so overlapping starts can't loop forever
            search_from = end.max(start + 1);
        }
    }
    out
}

/// Find the next emojikey at or after `from`. Returns (start, end, me, content, you)
/// as BYTE offsets into `text`. Strict on the 3-label spine, permissive on payload.
fn next_key(text: &str, from: usize) -> Option<(usize, usize, String, String, String)> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        // a key must begin with `[`; cheap-scan for it
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        if let Some((end, me, content, you)) = try_match_at(text, i) {
            return Some((i, end, me, content, you));
        }
        i += 1;
    }
    None
}

/// Try to match a full key starting exactly at byte index `start` (`text[start]`
/// is `[`). Returns (end, me, content, you) on success. `end` is the byte index
/// just past the closing `]` of the YOU segment.
fn try_match_at(text: &str, start: usize) -> Option<(usize, String, String, String)> {
    let (me, after_me) = segment(text, start, ME)?;
    let after_tilde1 = tilde(text, after_me)?;
    let (content, after_content) = segment(text, after_tilde1, CONTENT)?;
    let after_tilde2 = tilde(text, after_content)?;
    let (you, after_you) = segment(text, after_tilde2, YOU)?;
    Some((after_you, me, content, you))
}

/// Match `[ <ws> LABEL <ws> | <ws> PAYLOAD <ws> ]` at `pos`. Case-insensitive
/// label. Payload = anything up to the next `]` (no nesting), trimmed.
/// Returns (payload, index-just-past-`]`).
fn segment(text: &str, pos: usize, label: &str) -> Option<(String, usize)> {
    let b = text.as_bytes();
    let mut i = pos;
    if i >= b.len() || b[i] != b'[' {
        return None;
    }
    i += 1;
    i = skip_ws(b, i);
    // case-insensitive label match
    let lb = label.as_bytes();
    if i + lb.len() > b.len() {
        return None;
    }
    for (k, &lc) in lb.iter().enumerate() {
        if b[i + k].to_ascii_uppercase() != lc {
            return None;
        }
    }
    i += lb.len();
    i = skip_ws(b, i);
    if i >= b.len() || b[i] != b'|' {
        return None;
    }
    i += 1;
    // payload runs to the next `]`
    let payload_start = i;
    while i < b.len() && b[i] != b']' {
        i += 1;
    }
    if i >= b.len() {
        return None; // unterminated
    }
    let payload = text[payload_start..i].trim().to_string();
    i += 1; // past `]`
    Some((payload, i))
}

/// Match optional-ws `~` optional-ws at `pos`; return index just past it.
fn tilde(text: &str, pos: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = skip_ws(b, pos);
    if i >= b.len() || b[i] != b'~' {
        return None;
    }
    i += 1;
    Some(skip_ws(b, i))
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t' || b[i] == b'\n' || b[i] == b'\r') {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(text: &str) -> Event {
        // whole-text authored provenance, so source_kind resolves in tests
        let span = crate::TextSpan {
            start: 0,
            end: text.len(),
            kind: SourceKind::Authored,
        };
        Event {
            event_id: "e1".into(),
            parent_id: None,
            role: "assistant".into(),
            timestamp: Some("2026-06-30T00:00:00Z".into()),
            text: Some(text.into()),
            text_provenance: vec![span],
            thinking: None,
        }
    }

    #[test]
    fn matches_canonical_shape() {
        let anns = emojikey_read(&[ev(
            "wrap up 🐐 [ME|🧠🎨8∠45]~[CONTENT|💻🧩9∠15]~[YOU|🎓🌱8∠35] done",
        )]);
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.signal["me"].lexical(), "🧠🎨8∠45");
        assert_eq!(a.signal["content"].lexical(), "💻🧩9∠15");
        assert_eq!(a.signal["you"].lexical(), "🎓🌱8∠35");
        // emojikey legs stay TEXT (never coerced to numbers)
        assert_eq!(a.signal["me"], SignalValue::Text("🧠🎨8∠45".into()));
        // the span recovers the exact key substring
        let src = "wrap up 🐐 [ME|🧠🎨8∠45]~[CONTENT|💻🧩9∠15]~[YOU|🎓🌱8∠35] done";
        assert_eq!(&src[a.start..a.end], a.signal["raw"].lexical());
    }

    #[test]
    fn case_insensitive_and_minimal() {
        let anns = emojikey_read(&[ev("[me|🐐]~[content|⚙️🌊]~[you|🤝]")]);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].signal["me"].lexical(), "🐐");
    }

    #[test]
    fn ignores_ordinary_brackets_and_wrong_order() {
        assert!(emojikey_read(&[ev("see file.py:[12] and the [TODO] list")]).is_empty());
        // right labels, wrong order -> not a key
        assert!(emojikey_read(&[ev("[YOU|a]~[CONTENT|b]~[ME|c]")]).is_empty());
    }

    #[test]
    fn multiple_keys_in_one_text() {
        let anns = emojikey_read(&[ev(
            "[ME|a]~[CONTENT|b]~[YOU|c] ... later ... [ME|x]~[CONTENT|y]~[YOU|z]",
        )]);
        assert_eq!(anns.len(), 2);
        assert_eq!(anns[0].signal["you"].lexical(), "c");
        assert_eq!(anns[1].signal["me"].lexical(), "x");
    }
}
