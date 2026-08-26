//! Claude Code dialect adapter: real session JSONL → generic `Event`s.
//!
//! This is the ADAPTER, not the engine. It knows the Claude Code line shape
//! (verified empirically Day 39 across 49.7k real lines). It hands the engine
//! generic Events + a partition string; the engine never sees the dialect.

use crate::{Event, SourceKind, TextSpan};
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
        let (text, text_provenance) = extract_text(o.get("message"));
        events.push(Event {
            event_id,
            parent_id,
            role: t.to_string(),
            timestamp,
            text,
            text_provenance,
            thinking: extract_thinking(o.get("message")),
        });
    }
    Ok(events)
}

/// Pull the concatenated text from a `message` object, WITH a provenance map.
///
/// `content` is either a plain string (the human's typed turn → AUTHORED) or a
/// list of blocks. We now extract EVERY text-bearing block, not just `text`
/// blocks, and tag each with its `SourceKind` so readers can tell a live
/// emission from one merely quoted inside a tool result:
///   - `text`         → Authored   (human text, or the assistant's prose)
///   - `tool_result`  → ToolResult (tool output pasted back — quoted material)
///
/// Blocks are joined by `\n`; the provenance spans track each block's byte range
/// in the joined string so an offset maps back to its origin. (`tool_use` inputs
/// are machine JSON, not prose — skipped; readers anchor to prose.)
///
/// `thinking` blocks are NOT joined into `text` — reasoning is not spoken prose
/// and must not pollute what a reader scans. They go to `Event::thinking` via
/// [`extract_thinking`]; before 2026-08-26 they were silently discarded by the
/// catch-all arm below, which was a lossless-ingest violation with 750 blocks
/// behind it in spaceGOAT's own mirror alone.
fn extract_text(message: Option<&Value>) -> (Option<String>, Vec<TextSpan>) {
    let Some(content) = message.and_then(|m| m.get("content")) else {
        return (None, Vec::new());
    };
    // human turn: content is a plain string → all authored.
    if let Some(s) = content.as_str() {
        if s.is_empty() {
            return (None, Vec::new());
        }
        let span = TextSpan { start: 0, end: s.len(), kind: SourceKind::Authored };
        return (Some(s.to_string()), vec![span]);
    }
    let Some(blocks) = content.as_array() else {
        return (None, Vec::new());
    };
    let mut out = String::new();
    let mut spans = Vec::new();
    for b in blocks {
        let btype = b.get("type").and_then(Value::as_str).unwrap_or("");
        let (piece, kind) = match btype {
            "text" => (
                b.get("text").and_then(Value::as_str).map(str::to_string),
                SourceKind::Authored,
            ),
            "tool_result" => (extract_tool_result_text(b), SourceKind::ToolResult),
            _ => (None, SourceKind::Authored), // tool_use etc.: no prose
        };
        let Some(piece) = piece else { continue };
        if piece.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n'); // separator is not part of any span
        }
        let start = out.len();
        out.push_str(&piece);
        spans.push(TextSpan { start, end: out.len(), kind });
    }
    if out.is_empty() {
        (None, Vec::new())
    } else {
        (Some(out), spans)
    }
}

/// Pull `thinking` blocks out of a `message`, joined by a blank line when a
/// record carries several. Separate from [`extract_text`] on purpose: these
/// share a record with spoken prose but are not part of it.
fn extract_thinking(message: Option<&Value>) -> Option<String> {
    let blocks = message.and_then(|m| m.get("content")).and_then(Value::as_array)?;
    let parts: Vec<&str> = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("thinking"))
        .filter_map(|b| b.get("thinking").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// A `tool_result` block's `content` is itself either a string or a list of
/// sub-blocks (each a `{type:"text", text:...}` or an image, etc.). Pull the
/// text out; images and other non-text sub-blocks contribute nothing.
fn extract_tool_result_text(block: &Value) -> Option<String> {
    let content = block.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(items) = content.as_array() {
        let mut parts = Vec::new();
        for it in items {
            if it.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(s) = it.get("text").and_then(Value::as_str) {
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
