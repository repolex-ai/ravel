//! Reliving a soul's history into its memory, the way OptMem builds one.
//!
//! OptMem's agent writes its own memories while it works and merges them the
//! moment a merge comes due; nothing runs later. A soul's past was not noted
//! that way, so a reader relives it instead: every turn the soul has, from
//! every harness, in one stream ordered by turn time; a few turns at a time;
//! each time shown who the soul is, its memory as it stood at that moment, and
//! what happens next. It notes what the soul would have noted, then answers
//! every merge that came due before it reads on. "Now" only moves forward.
//!
//! The words for what a memory is and how a merge is made are OptMem's
//! (`memo`, the TEMPLATE and `nap_prompt`). Going forward the soul itself runs
//! the same note and merge steps; only the driver differs.

use crate::memory::hour_of;
use crate::memory_extract::{
    account_limit_note, clip, conversations, is_account_limit, relabel_channels, summary_schema,
    unseen_number, Conversation, Ledger, Llm, ProseTurn, Speakers,
};
use crate::memory_log::{
    children_digest, node_text, tree_of, wake_view_fit, Line, Memory, MemoryLog, Summary,
};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

/// OptMem's size for one memory or one merged line.
pub const NOTE_BYTES: usize = 280;
/// Prose shown per step. Small enough that "now" stays close to every turn.
const STRETCH_CHARS: usize = 12_000;
/// The wake view shown each step: the same limits `ravel memory` prints with.
const WAKE_LINES: usize = 96;
const WAKE_CHARS: usize = 24_000;
/// How much of SOUL.md is shown as "who you are".
const SOUL_CHARS: usize = 6_000;

fn note_system(agent: &str) -> String {
    format!(
        "You are {agent}. You are reliving your own past in order, one stretch at a time, as if it were happening now. Each time you are shown who you are, your memory as it stood at that moment, and what happens next.

Your memory works like this. Note something whenever you learn something new, or something worth keeping happens. That covers a task worth real effort, a fact or insight the user teaches you, anything you learn about their life (even indirectly), any event of lasting effect. Do not register redundant memories: if your memory already holds it, do not note it again.

Each note is one line of at most {NOTE_BYTES} bytes. Write as yourself: 'I' is you, {agent}; name everyone else. Turns marked 'you' are yours. Turns marked 'human' are your human's, except text marked as a message from another agent or as pasted, which is someone else's words. Credit what happened to whoever did it. A guess or a plan stays one. Copy names, paths and numbers exactly; invent nothing. Turns from before you had your name, such as old Claude desktop chats, are still your past: note them the same way. Most stretches need no note or one; none is a fine answer.

For each note give the numbers of the turns it rests on."
    )
}

fn merge_system(agent: &str) -> String {
    format!(
        "You are {agent}, compressing your own memory. Compress the lines below, oldest first, into one line of at most {NOTE_BYTES} bytes. Keep what has lasting effect, drop what does not. Invent nothing. Write as yourself: 'I' is you, {agent}."
    )
}

fn note_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "notes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"},
                        "turns": {"type": "array", "items": {"type": "integer"}}
                    },
                    "required": ["text", "turns"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["notes"],
        "additionalProperties": false
    })
}

/// One turn in the soul's stream, with the conversation it came from.
#[derive(Debug, Clone)]
pub struct StreamTurn {
    pub turn: ProseTurn,
    pub conv: usize,
}

/// Every turn with prose, from every conversation, ordered by its own time.
/// Each distinct turn id once (transcript files repeat history).
pub fn stream(convs: &[Conversation]) -> Vec<StreamTurn> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for (ci, c) in convs.iter().enumerate() {
        for t in &c.turns {
            if t.text.trim().is_empty() || !seen.insert(t.id.as_str()) {
                continue;
            }
            out.push(StreamTurn {
                turn: t.clone(),
                conv: ci,
            });
        }
    }
    // Stable on equal times: a conversation's own order is kept.
    out.sort_by(|a, b| a.turn.ts.cmp(&b.turn.ts));
    out
}

/// The stream cut into steps of at most [`STRETCH_CHARS`] of prose.
pub fn stretches(stream: Vec<StreamTurn>) -> Vec<Vec<StreamTurn>> {
    let mut out = Vec::new();
    let mut cur: Vec<StreamTurn> = Vec::new();
    let mut chars = 0;
    for mut s in stream {
        s.turn.text = clip(s.turn.text.trim());
        if chars + s.turn.text.len() > STRETCH_CHARS && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            chars = 0;
        }
        chars += s.turn.text.len();
        cur.push(s);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Where a turn happened, in words: the harness, and the place it names.
fn where_label(c: &Conversation) -> String {
    let session: String = Path::new(&c.id)
        .file_stem()
        .map(|s| s.to_string_lossy().chars().take(8).collect())
        .unwrap_or_default();
    let place = match c.source {
        "claude-code" => format!(
            "Claude Code, {}",
            if c.place.is_empty() {
                "unknown directory"
            } else {
                &c.place
            }
        ),
        "agy" => "Antigravity (Gemini)".to_string(),
        "claude.ai" => format!("Claude desktop app, {}", c.place.trim_start_matches("a ")),
        other => other.to_string(),
    };
    format!("{place}, session {session}")
}

fn wake_text(lines: &[Line]) -> String {
    if lines.is_empty() {
        return "(empty: you have no memories yet)".to_string();
    }
    lines
        .iter()
        .map(|l| {
            let day = |s: &str| s.get(..16).unwrap_or(s).replace('T', " ");
            if l.from == l.to {
                format!("{}  {}", day(&l.from), l.text)
            } else {
                format!("{} → {}  {}", day(&l.from), day(&l.to), l.text)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn note_prompt(
    soul_md: &str,
    wake: &[Line],
    stretch: &[StreamTurn],
    convs: &[Conversation],
    sp: &Speakers,
) -> String {
    let at = stretch.first().map(|s| s.turn.ts.as_str()).unwrap_or("?");
    let mut s = format!(
        "WHO YOU ARE\n{soul_md}\n\nYOUR MEMORY AS IT STOOD AT {at}\n{}\n\nWHAT HAPPENS NEXT\n",
        wake_text(wake)
    );
    for (i, st) in stretch.iter().enumerate() {
        let t = &st.turn;
        let user = t.role == "user";
        s += &format!(
            "[{}] {} · {}\n{}: {}\n\n",
            i + 1,
            t.ts,
            where_label(&convs[st.conv]),
            if user { "human" } else { "you" },
            if user {
                sp.relabel_human(&t.text)
            } else {
                relabel_channels(&t.text)
            }
        );
    }
    s
}

fn memory_id(first_turn: &str, text: &str) -> String {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(format!("{first_turn}\u{0}{text}").as_bytes());
    format!("m{}", &format!("{h:x}")[..15])
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ReliveReport {
    pub turns: usize,
    pub stretches: usize,
    pub notes: usize,
    pub dropped: usize,
    pub merges: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub failed: Vec<String>,
    pub stopped: Option<String>,
}

/// What a relive over `[from, to)` would read, without calling anything:
/// (turns, stretches, prose characters).
pub fn plan(repo: &Path, from: &str, to: &str) -> Result<(usize, usize, usize)> {
    let convs = conversations(repo)?;
    let st: Vec<StreamTurn> = stream(&convs)
        .into_iter()
        .filter(|s| s.turn.ts.as_str() >= from && s.turn.ts.as_str() < to)
        .collect();
    let n = st.len();
    let parts = stretches(st);
    let chars = parts.iter().flatten().map(|s| s.turn.text.len()).sum();
    Ok((n, parts.len(), chars))
}

/// Relive a soul's turns in `[from, to)` (RFC 3339 strings) into `log`,
/// resuming after the turns `log` has already read.
#[allow(clippy::too_many_arguments)]
pub fn relive(
    repo: &Path,
    soul: &str,
    sp: &Speakers,
    llm: &dyn Llm,
    ledger: &Ledger,
    log: &MemoryLog,
    from: &str,
    to: &str,
    max_calls: usize,
) -> Result<ReliveReport> {
    let mut r = ReliveReport::default();
    let soul_md: String = std::fs::read_to_string(repo.join("SOUL.md"))
        .unwrap_or_default()
        .chars()
        .take(SOUL_CHARS)
        .collect();
    let convs = conversations(repo)?;
    let done = log.extracted()?;
    let st: Vec<StreamTurn> = stream(&convs)
        .into_iter()
        .filter(|s| s.turn.ts.as_str() >= from && s.turn.ts.as_str() < to)
        .filter(|s| !done.contains(&s.turn.id))
        .collect();
    let mut calls = 0usize;
    let mut take_call = |r: &mut ReliveReport| -> bool {
        if let Err(e) = ledger.check() {
            r.stopped.get_or_insert(e.to_string());
            return false;
        }
        if calls >= max_calls {
            r.stopped
                .get_or_insert(format!("call limit for this run ({max_calls}) reached"));
            return false;
        }
        calls += 1;
        true
    };

    for stretch in stretches(st) {
        // 1. Note: what the soul would have written down, reading this now.
        let memories = log.memories()?;
        let sums = log.summaries()?;
        let first_hour = hour_of(&stretch[0].turn.ts).unwrap_or(0);
        let wake = wake_view_fit(&memories, &sums, first_hour, WAKE_LINES, WAKE_CHARS);
        let prompt = note_prompt(&soul_md, &wake, &stretch, &convs, sp);
        if !take_call(&mut r) {
            return Ok(r);
        }
        let (v, i, o) = match llm.json(&note_system(&sp.agent), &prompt, &note_schema(), 2048) {
            Ok(x) => x,
            Err(e) if is_account_limit(&e) => {
                r.stopped = Some(account_limit_note(&e));
                return Ok(r);
            }
            Err(e) => {
                r.failed
                    .push(format!("stretch at {}: {e:#}", stretch[0].turn.ts));
                continue;
            }
        };
        ledger.record(soul, i, o)?;
        r.input_tokens += i;
        r.output_tokens += o;
        let mut notes = Vec::new();
        let mut dropped = Vec::new();
        for n in v["notes"].as_array().cloned().unwrap_or_default() {
            let text = n["text"].as_str().unwrap_or("").trim().to_string();
            if text.is_empty() {
                continue;
            }
            if let Some(num) = unseen_number(&text, &prompt) {
                dropped.push(format!("{num}: {text}"));
                continue;
            }
            let mut cited: Vec<&ProseTurn> = n["turns"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|k| k.as_u64())
                .filter_map(|k| stretch.get((k as usize).checked_sub(1)?))
                .map(|s| &s.turn)
                .collect();
            cited.sort_by(|a, b| a.ts.cmp(&b.ts));
            cited.dedup_by(|a, b| a.id == b.id);
            let Some(first) = cited.first() else { continue };
            notes.push(Memory {
                id: memory_id(&first.id, &text),
                ts: first.ts.clone(),
                text,
                turns: cited.iter().map(|t| t.id.clone()).collect(),
                model: llm.model().to_string(),
            });
        }
        let ids: Vec<String> = stretch.iter().map(|s| s.turn.id.clone()).collect();
        log.append_dropped(&dropped)?;
        log.append_chunk(&notes, &ids)?;
        r.turns += stretch.len();
        r.stretches += 1;
        r.notes += notes.len();
        r.dropped += dropped.len();

        // 2. Merge: every window that closed by now, children first, before
        //    reading on — OptMem's "each merge happens on the spot".
        let now_hour = hour_of(&stretch[stretch.len() - 1].turn.ts).unwrap_or(0);
        let memories = log.memories()?;
        let mut sums = log.summaries()?;
        let (tree, by_id) = tree_of(&memories);
        for w in tree.summary_windows(now_hour) {
            let Some(d) = children_digest(&tree, w, &by_id, &sums) else {
                continue;
            };
            if sums.get(&w.id()).is_some_and(|s| s.digest == d) {
                continue;
            }
            let lines: Vec<String> = tree
                .children(w)
                .iter()
                .filter_map(|c| node_text(c, &by_id, &sums))
                .collect();
            if !take_call(&mut r) {
                return Ok(r);
            }
            let (v, i, o) = match llm.json(
                &merge_system(&sp.agent),
                &lines.join("\n"),
                &summary_schema(),
                1024,
            ) {
                Ok(x) => x,
                Err(e) if is_account_limit(&e) => {
                    r.stopped = Some(account_limit_note(&e));
                    return Ok(r);
                }
                Err(e) => {
                    r.failed.push(format!("{}: {e:#}", w.id()));
                    continue;
                }
            };
            ledger.record(soul, i, o)?;
            r.input_tokens += i;
            r.output_tokens += o;
            let sm = Summary {
                window: w.id(),
                digest: d,
                text: v["text"].as_str().unwrap_or("").trim().to_string(),
                model: llm.model().to_string(),
                written: chrono::Utc::now().to_rfc3339(),
            };
            log.append_summary(&sm)?;
            sums.insert(sm.window.clone(), sm);
            r.merges += 1;
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(id: &str, ts: &str, text: &str) -> ProseTurn {
        ProseTurn {
            id: id.into(),
            ts: ts.into(),
            role: "user".into(),
            text: text.into(),
        }
    }

    fn conv(id: &str, source: &'static str, turns: Vec<ProseTurn>) -> Conversation {
        Conversation {
            source,
            id: id.into(),
            place: String::new(),
            turns,
        }
    }

    #[test]
    fn overlapping_sessions_interleave_by_turn_time() {
        // A long Claude Code session that started first, and an Antigravity
        // one that ran inside it: the stream must never go back in time.
        let convs = vec![
            conv(
                "cc",
                "claude-code",
                vec![
                    turn("a1", "2026-06-01T10:00:00Z", "x"),
                    turn("a2", "2026-06-01T16:00:00Z", "x"),
                ],
            ),
            conv("agy", "agy", vec![turn("b1", "2026-06-01T12:00:00Z", "y")]),
        ];
        let ids: Vec<String> = stream(&convs).into_iter().map(|s| s.turn.id).collect();
        assert_eq!(ids, ["a1", "b1", "a2"]);
    }

    #[test]
    fn a_repeated_turn_and_an_empty_turn_are_read_once_and_never() {
        let convs = vec![
            conv(
                "one",
                "claude-code",
                vec![turn("a", "2026-06-01T10:00:00Z", "x")],
            ),
            conv(
                "two",
                "claude-code",
                vec![
                    turn("a", "2026-06-01T10:00:00Z", "x"),
                    turn("e", "2026-06-01T11:00:00Z", "  "),
                ],
            ),
        ];
        let ids: Vec<String> = stream(&convs).into_iter().map(|s| s.turn.id).collect();
        assert_eq!(ids, ["a"]);
    }

    struct Fake;
    impl Llm for Fake {
        fn model(&self) -> &str {
            "fake"
        }
        fn json(&self, system: &str, user: &str, _: &Value, _: u32) -> Result<(Value, u64, u64)> {
            if system.contains("compressing") {
                return Ok((
                    json!({"text": format!("merged {} lines", user.lines().count())}),
                    1,
                    1,
                ));
            }
            // One note per turn shown, each resting on its own turn.
            let n = user
                .lines()
                .filter(|l| l.starts_with('[') && l.contains("] 20"))
                .count();
            let notes: Vec<Value> = (1..=n)
                .map(|k| json!({"text": format!("I noted turn {k}"), "turns": [k]}))
                .collect();
            Ok((json!({ "notes": notes }), 1, 1))
        }
    }

    #[test]
    fn merges_come_due_as_time_passes_not_at_the_end() {
        let base = std::env::temp_dir().join(format!("ravel-relive-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("soul");
        let mirror = repo.join(crate::sync::TRANSCRIPTS_SUBDIR).join("p");
        std::fs::create_dir_all(&mirror).unwrap();
        // Turns clipped to about 4,000 characters, so the first two (hour 10)
        // share a stretch and the third (hour 11) gets its own. Hour 10's two
        // notes make it a node; it is still open after the first stretch and
        // must be merged once the 11:30 turn has been read.
        let big = "z".repeat(10_000);
        let rec = |uuid: &str, parent: &str, ts: &str| {
            json!({"type":"user","uuid":uuid,"parentUuid":parent,"timestamp":ts,"sessionId":"s",
                   "message":{"role":"user","content":big}})
            .to_string()
        };
        std::fs::write(
            mirror.join("s1.jsonl"),
            [
                rec("u1", "", "2026-06-01T10:00:00Z"),
                rec("u2", "u1", "2026-06-01T10:10:00Z"),
                rec("u3", "u2", "2026-06-01T11:30:00Z"),
            ]
            .join("\n"),
        )
        .unwrap();
        let log = MemoryLog::at(base.join("out"));
        let ledger = Ledger::open_at(base.join("spend.tsv"), 1.0).unwrap();
        let sp = Speakers {
            agent: "soul".into(),
            peers: vec![],
        };
        let r = relive(&repo, "s", &sp, &Fake, &ledger, &log, "2026", "2027", 100).unwrap();
        assert_eq!(r.stretches, 2, "{r:?}");
        assert_eq!(r.notes, 3);
        // Hour 10 holds two notes, so it is a node, and it closed once the
        // 11:30 turn was read.
        assert_eq!(r.merges, 1, "{r:?}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
