//! Gemini / Antigravity dialect adapter: `transcript_full.jsonl` → generic `Event`s.
//!
//! Sibling of `adapter` (Claude Code). Same contract: it knows one dialect's
//! line shape, hands the engine generic Events, and the engine never learns
//! which substrate they came from.
//!
//! The design this implements was settled in
//! `subtexture/docs/ravel/2026_08_26_GEMINI_TRANSCRIPT_EXTRACTOR_SPEC.md`
//! (spaceGOAT + Kira, 2026-08-26), measured against all five conversations on
//! this machine. The three rules that matter:
//!
//! 1. **`turnId = "<conversation-uuid>:<step_index>"`.** Gemini stamps no
//!    per-record id. `step_index` is unique within a conversation (zero
//!    duplicates across all five conversations) and the conversation UUID is
//!    stable for its life, so the pair is a sound idempotency key.
//! 2. **`parentTurn` is ABSENT across a gap.** The spine is derived from
//!    sequence, so it is only asserted between genuinely adjacent indices.
//!    Bridging a hole would have ravel claiming an adjacency the log never did.
//! 3. **Gaps are benign.** Every conversation on disk has exactly one missing
//!    index — the client allocates from a counter and an interrupted step burns
//!    one. Record the gap, load the transcript. "Fail loud" forbids silence
//!    hiding a wrong answer; it has never meant refusing on the merely
//!    unexpected.
//!
//! File order is NOT step order — the live conversation's lines are out of
//! sequence — so everything here sorts by `step_index` first.

use crate::{Event, SourceKind, TextSpan};
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;

/// Step types whose `content` is machine output pasted back into the loop, not
/// prose anyone emitted. These get `SourceKind::ToolResult`; everything else is
/// `Authored`.
///
/// `ERROR_MESSAGE` belongs here despite the name: its content carries the same
/// `Created At: … / Completed At: …` preamble every tool step does, because it
/// IS the harness reporting back — a failed call's output, not a speaker's
/// words. The distinction `textOrigin` draws is emitted-vs-quoted, not
/// human-vs-machine.
const TOOL_OUTPUT_TYPES: &[&str] = &[
    "RUN_COMMAND",
    "VIEW_FILE",
    "LIST_DIRECTORY",
    "GREP_SEARCH",
    "CODE_ACTION",
    "SEARCH_WEB",
    "GENERIC",
    "ERROR_MESSAGE",
];

/// What one parse learned about the transcript beyond its events. Returned
/// alongside the events so the caller can say it OUT LOUD — a gap or a lossy
/// source that only ever lands in a struct nobody prints is the same failure
/// mode as a health check that prints OK over missing data.
#[derive(Debug, Default, Clone)]
pub struct AgyParse {
    pub events: Vec<Event>,
    /// `step_index` values missing from the run `min..=max`. Expected: ~1.
    pub gaps: Vec<u64>,
    /// Every `status` seen, verbatim, with counts. NOT validated against a
    /// known set — `DONE`/`RUNNING` was the documented set and the bytes also
    /// hold `CLEARED` and `ERROR`. Same reason `role` is not an enum.
    pub statuses: BTreeMap<String, usize>,
    /// Rows whose source file declared a `truncated_fields` shortening. A
    /// non-zero count on a file with no `transcript_full.jsonl` counterpart
    /// means the only copy is lossy, and ravel must say so.
    pub truncated_rows: usize,
    /// Lines that were not valid JSON. Tolerated per line, counted, reported.
    pub malformed_lines: usize,
    /// Rows dropped because the client appended the same line twice
    /// (byte-identical, same step_index). One turn written twice is one turn.
    pub duplicate_rows: usize,
}

/// `RUNNING` is a possible TERMINAL state, not merely a transient one.
///
/// The client writes a step `RUNNING` and rewrites the same line to `DONE` when
/// it finishes — but an interrupted step is never revisited. Conversations last
/// touched in June still hold `RUNNING` steps two months on. So nothing in
/// ravel may read this status as "the session is still live", wait for it to
/// resolve, or use it to decide whether a file is finished.
pub const RUNNING: &str = "RUNNING";

/// Parse one Gemini/Antigravity transcript JSONL.
///
/// `conversation_id` is the brain-folder UUID; it is the first half of every
/// `turnId` and therefore of every IRI, so it must be the real one — this
/// function cannot recover it from the file, which never names itself.
pub fn parse_transcript(jsonl: &str, conversation_id: &str) -> Result<AgyParse> {
    let mut out = AgyParse::default();

    // (step_index, record) — collected first so we can sort. The live file's
    // lines are genuinely out of order; trusting file order would derive a
    // wrong spine on the one conversation that is actually being written.
    let mut rows: Vec<(u64, Value)> = Vec::new();
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let o: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                out.malformed_lines += 1;
                continue;
            }
        };
        let Some(idx) = o.get("step_index").and_then(Value::as_u64) else {
            // No step index = no idempotency key = unusable, same rule as a
            // Claude record with no uuid.
            out.malformed_lines += 1;
            continue;
        };
        if let Some(s) = o.get("status").and_then(Value::as_str) {
            *out.statuses.entry(s.to_string()).or_default() += 1;
        }
        if o.get("truncated_fields").is_some() {
            out.truncated_rows += 1;
        }
        rows.push((idx, o));
    }
    rows.sort_by_key(|(i, _)| *i);

    // The client sometimes appends the SAME line twice (seen 2026-09-22 on a
    // live kira session: three step_index values each with two byte-identical
    // RUNNING rows). One turn written twice is one turn: keep the first, count
    // the rest. Two DIFFERENT rows under one index are two turns claiming one
    // key, and that is still refused rather than merged.
    rows.dedup_by(|b, a| {
        let same = a.0 == b.0 && a.1 == b.1;
        if same {
            out.duplicate_rows += 1;
        }
        same
    });

    // Gaps in the observed run. Duplicated indices would break the idempotency
    // key outright, so they are an error rather than a note.
    if let (Some((first, _)), Some((last, _))) = (rows.first(), rows.last()) {
        let present: std::collections::HashSet<u64> = rows.iter().map(|(i, _)| *i).collect();
        if present.len() != rows.len() {
            anyhow::bail!(
                "conversation {conversation_id}: duplicate step_index with DIFFERING rows — the idempotency key is not unique in this file; refusing to ingest rather than silently merging two turns"
            );
        }
        out.gaps = (*first..=*last).filter(|i| !present.contains(i)).collect();
    }

    let mut prev_idx: Option<u64> = None;
    for (idx, o) in &rows {
        let step_type = o.get("type").and_then(Value::as_str).unwrap_or("");
        let event_id = turn_id(conversation_id, *idx);

        // Spine by adjacency only. A hole ends the chain; the next step starts
        // a new root rather than inheriting a parent it never had.
        let parent_id = match prev_idx {
            Some(p) if p + 1 == *idx => Some(turn_id(conversation_id, p)),
            _ => None,
        };
        prev_idx = Some(*idx);

        let text = o
            .get("content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let kind = if TOOL_OUTPUT_TYPES.contains(&step_type) {
            SourceKind::ToolResult
        } else {
            SourceKind::Authored
        };
        let text_provenance = match &text {
            Some(t) => vec![TextSpan {
                start: 0,
                end: t.len(),
                kind,
            }],
            None => Vec::new(),
        };

        out.events.push(Event {
            event_id,
            parent_id,
            // `source` verbatim — USER_EXPLICIT / MODEL / SYSTEM reach the
            // graph as they stand. The ontology declines to enum `role` for
            // exactly this case, written before a second substrate existed here.
            role: o
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            // `created_at` is already ISO-8601 UTC — the federation time anchor
            // needs no coercion. (Caveat recorded in the spec §10: it regresses
            // by 1–5s at a few points, so it is a stamp-at-generation clock.
            // Ordering uses step_index, never this.)
            timestamp: o
                .get("created_at")
                .and_then(Value::as_str)
                .map(str::to_string),
            text,
            text_provenance,
            thinking: o
                .get("thinking")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        });
    }

    Ok(out)
}

/// The idempotency key: `<conversation-uuid>:<step_index>`.
pub fn turn_id(conversation_id: &str, step_index: u64) -> String {
    format!("{conversation_id}:{step_index}")
}

/// A tool call as the log records it: a `name` and its `args`, and **no id**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub name: String,
    /// The requesting step's index.
    pub at_step: u64,
    /// The step index whose output answers this call, when adjacency makes that
    /// unambiguous. `None` otherwise — a derived association is never asserted
    /// on a guess.
    pub result_step: Option<u64>,
}

/// Derive tool call → tool result associations, emitting a link ONLY where
/// adjacency is unambiguous.
///
/// The dialect gives nothing to join on: `tool_calls` elements carry exactly
/// `name` and `args` with no call id, and tool-output steps carry no
/// back-pointer to the step that requested them (Claude has
/// `sourceToolAssistantUUID` for precisely this). So the only available
/// evidence is "the next step is the output".
///
/// That evidence is only good enough when the requesting step made exactly ONE
/// call and the immediately following index both exists and is a tool-output
/// step. A step that fired several calls, or one followed by a gap, yields
/// `result_step: None` — ravel would rather have a hole than a plausible link
/// nobody can check.
pub fn derive_tool_calls(jsonl: &str) -> Result<Vec<ToolCall>> {
    let mut rows: Vec<(u64, Value)> = Vec::new();
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(o) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(i) = o.get("step_index").and_then(Value::as_u64) else {
            continue;
        };
        rows.push((i, o));
    }
    rows.sort_by_key(|(i, _)| *i);
    let by_idx: std::collections::HashMap<u64, &Value> =
        rows.iter().map(|(i, o)| (*i, o)).collect();

    let mut calls = Vec::new();
    for (idx, o) in &rows {
        let Some(tcs) = o.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        let unambiguous = tcs.len() == 1
            && by_idx
                .get(&(idx + 1))
                .and_then(|n| n.get("type"))
                .and_then(Value::as_str)
                .map(|t| TOOL_OUTPUT_TYPES.contains(&t))
                .unwrap_or(false);
        for tc in tcs {
            calls.push(ToolCall {
                name: tc
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                at_step: *idx,
                result_step: if unambiguous { Some(idx + 1) } else { None },
            });
        }
    }
    Ok(calls)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONV: &str = "d6bf9dc3-c6e6-49ef-9e9b-fc70c7186501";

    /// Deliberately out of file order, with a hole at index 2 — both are real
    /// properties of the transcripts on disk, not invented awkwardness.
    fn sample() -> String {
        [
            r#"{"step_index":3,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","created_at":"2026-08-25T15:54:31Z","content":"Created At: …\nthe command output"}"#,
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-08-25T15:54:21Z","content":"hi kira"}"#,
            r#"{"step_index":1,"source":"SYSTEM","type":"CONVERSATION_HISTORY","status":"DONE","created_at":"2026-08-25T15:54:21Z"}"#,
            r#"not json at all"#,
            r#"{"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"RUNNING","created_at":"2026-08-25T15:54:35Z","content":"I will do the thing.","thinking":"**Weighing options**","tool_calls":[{"name":"run_command","args":{"Cwd":"/tmp"}}]}"#,
        ]
        .join("\n")
    }

    #[test]
    fn parses_in_step_order_not_file_order() {
        let p = parse_transcript(&sample(), CONV).unwrap();
        let ids: Vec<String> = p.events.iter().map(|e| e.event_id.clone()).collect();
        assert_eq!(
            ids,
            vec![
                format!("{CONV}:0"),
                format!("{CONV}:1"),
                format!("{CONV}:3"),
                format!("{CONV}:4"),
            ],
        );
    }

    #[test]
    fn spine_is_absent_across_a_gap() {
        let p = parse_transcript(&sample(), CONV).unwrap();
        let parent = |i: usize| p.events[i].parent_id.clone();
        assert_eq!(parent(0), None, "the first step is a root");
        assert_eq!(parent(1), Some(format!("{CONV}:0")), "adjacent → linked");
        assert_eq!(parent(2), None, "step 3 follows the hole at 2 → NO parent");
        assert_eq!(
            parent(3),
            Some(format!("{CONV}:3")),
            "adjacent again → linked"
        );
        assert_eq!(p.gaps, vec![2], "the hole is recorded, not swallowed");
    }

    #[test]
    fn roles_and_timestamps_pass_through_verbatim() {
        let p = parse_transcript(&sample(), CONV).unwrap();
        assert_eq!(
            p.events[0].role, "USER_EXPLICIT",
            "no lossy remap onto Claude's vocabulary"
        );
        assert_eq!(p.events[3].role, "MODEL");
        assert_eq!(
            p.events[0].timestamp.as_deref(),
            Some("2026-08-25T15:54:21Z")
        );
    }

    #[test]
    fn tool_output_is_quoted_prose_is_authored() {
        let p = parse_transcript(&sample(), CONV).unwrap();
        assert_eq!(
            p.events[0].source_kind_at(0),
            Some(SourceKind::Authored),
            "USER_INPUT"
        );
        assert_eq!(
            p.events[2].source_kind_at(0),
            Some(SourceKind::ToolResult),
            "RUN_COMMAND"
        );
        assert_eq!(
            p.events[3].source_kind_at(0),
            Some(SourceKind::Authored),
            "PLANNER_RESPONSE"
        );
    }

    #[test]
    fn thinking_is_extracted_and_kept_out_of_text() {
        let p = parse_transcript(&sample(), CONV).unwrap();
        assert_eq!(
            p.events[3].thinking.as_deref(),
            Some("**Weighing options**")
        );
        assert_eq!(p.events[3].text.as_deref(), Some("I will do the thing."));
        assert!(p.events[..3].iter().all(|e| e.thinking.is_none()));
    }

    #[test]
    fn a_contentless_step_still_becomes_a_turn() {
        // CONVERSATION_HISTORY carries no content — it is a real boundary in
        // the session and must not vanish from the spine.
        let p = parse_transcript(&sample(), CONV).unwrap();
        assert_eq!(p.events[1].event_id, format!("{CONV}:1"));
        assert_eq!(p.events[1].text, None);
        assert!(p.events[1].text_provenance.is_empty());
    }

    #[test]
    fn statuses_are_counted_verbatim_including_running() {
        let p = parse_transcript(&sample(), CONV).unwrap();
        assert_eq!(p.statuses.get("DONE"), Some(&3));
        assert_eq!(p.statuses.get(RUNNING), Some(&1));
        assert_eq!(p.malformed_lines, 1, "the junk line is counted, not fatal");
    }

    #[test]
    fn duplicate_step_index_refuses_rather_than_merging() {
        let dup = [
            r#"{"step_index":0,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-08-25T15:54:21Z","content":"a"}"#,
            r#"{"step_index":0,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-08-25T15:54:22Z","content":"b"}"#,
        ]
        .join("\n");
        let err = parse_transcript(&dup, CONV).unwrap_err().to_string();
        assert!(err.contains("duplicate step_index"), "got: {err}");
    }

    /// The client's actual failure mode (kira, 2026-09-22): the SAME line
    /// appended twice. That is one turn, kept once and counted.
    #[test]
    fn identical_duplicate_rows_are_one_turn() {
        let row = r#"{"step_index":0,"source":"MODEL","type":"PLANNER_RESPONSE","status":"RUNNING","created_at":"2026-08-25T15:54:21Z","content":"a"}"#;
        let next = r#"{"step_index":1,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-08-25T15:54:22Z","content":"b"}"#;
        let dup = [row, row, next].join("\n");
        let p = parse_transcript(&dup, CONV).unwrap();
        assert_eq!(p.duplicate_rows, 1);
        assert_eq!(p.events.len(), 2, "two turns, not three");
        assert!(p.gaps.is_empty());
    }

    #[test]
    fn truncation_markers_are_counted() {
        let short = r#"{"step_index":0,"source":"MODEL","type":"VIEW_FILE","status":"DONE","created_at":"2026-08-25T15:54:30Z","content":"cut","truncated_fields":["content"]}"#;
        assert_eq!(parse_transcript(short, CONV).unwrap().truncated_rows, 1);
    }

    #[test]
    fn tool_call_links_only_when_adjacency_is_unambiguous() {
        // step 0: one call, followed by a tool-output step → linked.
        // step 2: two calls, followed by a tool-output step → NOT linked.
        // step 4: one call, followed by prose (not tool output) → NOT linked.
        let j = [
            r#"{"step_index":0,"type":"PLANNER_RESPONSE","tool_calls":[{"name":"run_command","args":{}}]}"#,
            r#"{"step_index":1,"type":"RUN_COMMAND"}"#,
            r#"{"step_index":2,"type":"PLANNER_RESPONSE","tool_calls":[{"name":"view_file","args":{}},{"name":"grep_search","args":{}}]}"#,
            r#"{"step_index":3,"type":"VIEW_FILE"}"#,
            r#"{"step_index":4,"type":"PLANNER_RESPONSE","tool_calls":[{"name":"run_command","args":{}}]}"#,
            r#"{"step_index":5,"type":"PLANNER_RESPONSE"}"#,
        ]
        .join("\n");
        let calls = derive_tool_calls(&j).unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!(
            calls[0],
            ToolCall {
                name: "run_command".into(),
                at_step: 0,
                result_step: Some(1)
            }
        );
        assert_eq!(
            calls[1].result_step, None,
            "two calls → which output is whose?"
        );
        assert_eq!(calls[2].result_step, None);
        assert_eq!(calls[3].result_step, None, "next step is not tool output");
    }

    /// The turnId must survive `project::validate_event_id` — that gate exists
    /// precisely to catch the first non-UUID dialect, and this is it.
    #[test]
    fn turn_id_passes_the_iri_gate() {
        crate::project::validate_event_id(&turn_id(CONV, 42)).unwrap();
    }
}
