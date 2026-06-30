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

use crate::Event;
use std::collections::BTreeMap;

/// One finding: a detector observed a signal anchored to a span of one event.
/// The Rust mirror of the Python `MonologRow`, trimmed to what the projector
/// needs. `signal` is an ordered string→string map so projection is
/// deterministic (BTreeMap = stable key order = stable hashes = idempotent IRIs).
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
    /// The signal payload: the verbatim finding, lossless. Ordered for stable IRIs.
    pub signal: BTreeMap<String, String>,
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
            let mut signal = BTreeMap::new();
            signal.insert("raw".to_string(), text[start..end].to_string());
            signal.insert("me".to_string(), me);
            signal.insert("content".to_string(), content);
            signal.insert("you".to_string(), you);
            signal.insert("voice".to_string(), e.role.clone());
            out.push(Annotation {
                reader: "emojikey".to_string(),
                event_type: "emojikey/harvest".to_string(),
                ts: e.timestamp.clone(),
                event_id: e.event_id.clone(),
                start,
                end,
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
        Event {
            event_id: "e1".into(),
            parent_id: None,
            role: "assistant".into(),
            timestamp: Some("2026-06-30T00:00:00Z".into()),
            text: Some(text.into()),
        }
    }

    #[test]
    fn matches_canonical_shape() {
        let anns = emojikey_read(&[ev("wrap up 🐐 [ME|🧠🎨8∠45]~[CONTENT|💻🧩9∠15]~[YOU|🎓🌱8∠35] done")]);
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.signal["me"], "🧠🎨8∠45");
        assert_eq!(a.signal["content"], "💻🧩9∠15");
        assert_eq!(a.signal["you"], "🎓🌱8∠35");
        // the span recovers the exact key substring
        let src = "wrap up 🐐 [ME|🧠🎨8∠45]~[CONTENT|💻🧩9∠15]~[YOU|🎓🌱8∠35] done";
        assert_eq!(&src[a.start..a.end], a.signal["raw"]);
    }

    #[test]
    fn case_insensitive_and_minimal() {
        let anns = emojikey_read(&[ev("[me|🐐]~[content|⚙️🌊]~[you|🤝]")]);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].signal["me"], "🐐");
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
        assert_eq!(anns[0].signal["you"], "c");
        assert_eq!(anns[1].signal["me"], "x");
    }
}
