//! Claude Code dialect adapter: real session JSONL → generic `Event`s.
//!
//! This is the ADAPTER, not the engine. It knows the Claude Code line shape
//! (verified empirically Day 39 across 49.7k real lines). It hands the engine
//! generic Events + a partition string; the engine never sees the dialect.

use crate::Event;
use anyhow::Result;
use serde_json::Value;

/// Parse one Claude Code `.jsonl` transcript into generic Events.
///
/// Spike scope: keep only the conversational turns (`type` = user|assistant) —
/// the things readers anchor to. Bookkeeping line-types (file-history-snapshot,
/// permission-mode, …) are faithfully *recognized* but skipped here; the full
/// 15-type projection is later work. We extract the spine (parentUuid), the
/// timestamp (already RFC-3339 in the source), and the concatenated text.
pub fn parse_transcript(jsonl: &str) -> Result<Vec<Event>> {
    let mut events = Vec::new();
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let o: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // tolerate a malformed line, don't abort the file
        };
        let t = o.get("type").and_then(Value::as_str).unwrap_or("");
        if t != "user" && t != "assistant" {
            continue;
        }
        let event_id = match o.get("uuid").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => continue, // no id = no idempotency key = unusable
        };
        let parent_id = o
            .get("parentUuid")
            .and_then(Value::as_str)
            .map(str::to_string);
        let timestamp = o
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string);
        let text = extract_text(o.get("message"));
        events.push(Event {
            event_id,
            parent_id,
            role: t.to_string(),
            timestamp,
            text,
        });
    }
    Ok(events)
}

/// Pull the concatenated text from a `message` object.
/// content is either a plain string (user turns) or a list of blocks
/// (assistant turns; we take the `text`-typed blocks).
fn extract_text(message: Option<&Value>) -> Option<String> {
    let content = message?.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(blocks) = content.as_array() {
        let mut parts = Vec::new();
        for b in blocks {
            if b.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(s) = b.get("text").and_then(Value::as_str) {
                    parts.push(s.to_string());
                }
            }
        }
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    None
}
