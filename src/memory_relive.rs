//! Reliving a soul's history into its OptMem memory.
//!
//! OptMem's agent notes while it works, and does each merge the moment it
//! comes due. A soul's past was not noted that way, so a reader relives it:
//! every turn the soul has, from every harness, in one stream ordered by turn
//! time, a stretch at a time. Before each stretch the reader wakes as OptMem
//! wakes — `cover(T, 96)` over the notes so far — then notes per OptMem's
//! rule, then answers every merge that came due, in OptMem's order, with
//! OptMem's prompt. "Now" only moves forward.
//!
//! The reader is the soul: the note step and the merge step both run as the
//! soul, with its SOUL.md, because in OptMem the agent merges its own memory
//! (a cold merge rewrote m4rq's "I own SPARQL" as "Rob SPARQL", 2026-10-13).
//!
//! Spec and decision log: spaceGOAT `Soul/Note/ravel-optmem-graph-spec.md`.

use crate::memory::{self, ENTRY_CHARS, WAKE_LINES};
use crate::memory_extract::{
    account_limit_note, clip, conversations, is_account_limit, relabel_channels, summary_schema,
    Conversation, Ledger, Llm, ProseTurn, Speakers,
};
use crate::memory_log::{self, wake_text, wake_view, Memory, MemoryLog, Summary};
use crate::peers::{PeerIndex, Sent};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

/// Prose shown per step. Small enough that "now" stays close to every turn.
const STRETCH_CHARS: usize = 24_000;
/// How much of SOUL.md is shown as who the soul is.
const SOUL_CHARS: usize = 6_000;
/// Asks for a line that fits before the run stops. OptMem's agent is refused
/// until it complies, and models trim bytes a few at a time (Sonnet 5.5:
/// 290, 287, 287, 284, 283 on one merge, 2026-10-13), so the cap is high: it
/// only stops a reader that never complies. That is a defect to stop on, not
/// a block to skip (the tree needs every block).
const ASKS: usize = 12;

/// The soul's standing instructions in both steps: who it is, and OptMem's
/// memory rules (`TEMPLATE`, "While working"), word for word except that a
/// note is an answer here, not a `memo note` call.
fn system(agent: &str, soul_md: &str) -> String {
    format!(
        "You are {agent}.\n\n{soul_md}\n\n## Memory\n\nYour memory is OptMem. It outlives every session, compaction, model and vendor change. Without it you do not know who you are, or what was decided and tried.\n\nRegister a memory (<1 line, max {ENTRY_CHARS} bytes>) whenever you learn something new, or something worth keeping happens. That covers a task worth real effort, a fact or insight the user teaches you, anything you learn about their life (even indirectly), any event of lasting effect.\n\nDo not register redundant memories.\n\nIf you are asked a compression: do it before your next action."
    )
}

/// The relive step: OptMem's wake as it stood, then what happens next.
/// A stretch to relive. `wake` is `Some` at the start of a session (always,
/// for one-shot calls): the soul wakes, then reads on. Within a session it
/// is `None`: the soul is already awake and has the earlier stretches and
/// its own notes and merges in front of it.
fn note_prompt(
    wake: Option<&str>,
    stretch: &[StreamTurn],
    convs: &[Conversation],
    sp: &Speakers,
) -> String {
    let h = &sp.human;
    let rules = format!("Turns marked 'you' are yours. Turns marked '{h}' are {h}'s, your human's, except text marked as a message from another agent or as pasted, which is someone else's words. Turns marked with another agent's name are that agent's. You see what was said, not the tool calls and their output; at the time, you saw those too. The numbers in brackets are only for citing turns: a memory must make sense on its own later, so never put a turn number in one.");
    let mut s = match wake {
        Some(w) => format!(
            "You are reliving your own past in order, as if it were happening now. You have just woken. Your memory, as `memo wake` prints it:\n\n{}\nYou are awake.\n\nWhat happens next. {rules}\n\n",
            if w.is_empty() { "No memories yet." } else { w }
        ),
        None => "What happens next.\n\n".to_string(),
    };
    for (i, st) in stretch.iter().enumerate() {
        let t = &st.turn;
        let user = t.role == "user";
        // Who speaks: the soul, the human by name, or — for a message the
        // sender's own transcript shows it sent — that peer (Kira's review,
        // 2026-10-13: a peer's message must not open with the human's name).
        let (who, text) = match (user, &t.peer) {
            (false, _) => ("you".to_string(), relabel_channels(&t.text)),
            (true, Some(Sent::Whole(p))) => (
                format!("{p} (another agent, a message to you)"),
                relabel_channels(&t.text),
            ),
            (true, Some(Sent::Leading(p))) => (
                format!("{p} (another agent), then more words that arrived with it — {}'s or another agent's", sp.human),
                relabel_channels(&t.text),
            ),
            (true, Some(Sent::Embedded(p))) => (
                format!("{}, then a message from another agent, {p}, that arrived while {} typed", sp.human, sp.human),
                relabel_channels(&t.text),
            ),
            (true, None) => (sp.human.clone(), sp.relabel_human(&t.text)),
        };
        s += &format!(
            "[{}] {} · {}\n{who}: {text}\n\n",
            i + 1,
            local_time(&t.ts),
            where_label(&convs[st.conv]),
        );
    }
    s += "Register the memories this stretch gives you, in order, if any. For each, give the numbers of the turns it rests on.";
    s
}

fn local_time(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M %:z")
                .to_string()
        })
        .unwrap_or_else(|_| ts.to_string())
}

/// One model call, tried up to four times with a growing pause: a rate limit
/// or a dropped connection should wait, not end a soul's run. An account
/// refusal ends it at once.
fn call(llm: &dyn Llm, system: &str, user: &str, schema: &Value) -> Result<(Value, u64, u64)> {
    let mut wait = 30;
    for attempt in 1.. {
        match llm.json(system, user, schema, 4096) {
            Ok(x) => return Ok(x),
            Err(e) if is_account_limit(&e) => anyhow::bail!(account_limit_note(&e)),
            Err(e) if attempt >= 4 => return Err(e),
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_secs(wait));
                wait *= 2;
            }
        }
    }
    unreachable!()
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

/// Mark every user turn a peer sent, from the sender's side, and drop
/// conversations that are the soul's own subagents (their first input is the
/// prompt the soul opened them with). OptMem: a subagent never writes memory.
pub fn label_peers(convs: &mut Vec<Conversation>, me: &str, idx: &PeerIndex) {
    convs.retain(|c| {
        let first = c.turns.iter().find(|t| t.role == "user" && !t.text.trim().is_empty());
        !first.is_some_and(|t| idx.spawned(me, &t.ts, &t.text))
    });
    for c in convs.iter_mut() {
        for t in c.turns.iter_mut().filter(|t| t.role == "user") {
            t.peer = idx.sender_of(me, &t.ts, &t.text);
        }
    }
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
    /// Lines sent back for failing OptMem's `check` (too long, several lines).
    pub reasked: usize,
    /// Sessions ended for size and begun again with a wake (session mode).
    pub sessions: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub stopped: Option<String>,
}

/// The stretches over `[from, to)` exactly as the reader is shown them,
/// without calling anything: the check that what goes in is right.
pub fn dump(repo: &Path, sp: &Speakers, peers: &PeerIndex, from: &str, to: &str) -> Result<String> {
    let mut convs = conversations(repo)?;
    label_peers(&mut convs, &sp.agent, peers);
    let st: Vec<StreamTurn> = stream(&convs)
        .into_iter()
        .filter(|s| s.turn.ts.as_str() >= from && s.turn.ts.as_str() < to)
        .collect();
    let mut out = String::new();
    for (k, stretch) in stretches(st).into_iter().enumerate() {
        out += &format!("==================== stretch {} ====================\n", k + 1);
        out += &note_prompt(Some("(wake)"), &stretch, &convs, sp);
        out += "\n";
    }
    Ok(out)
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

/// Ask until the answer passes OptMem's `check`: the first ask, then up to
/// [`ASKS`]` - 1` more. A note is asked again with the refused line and
/// OptMem's message — in OptMem each note is its own `memo note` call, so
/// the refusal names one line. A merge (`merge` is its prompt) is asked with
/// the refusal alone inside a session, where the prompt is in front of the
/// writer; read cold, with the prompt and the refused line.
/// `None` when it never fits.
#[allow(clippy::too_many_arguments)]
fn ask_line(
    llm: &dyn Llm,
    sys: &str,
    first: String,
    merge: Option<&str>,
    log: &MemoryLog,
    r: &mut ReliveReport,
    ledger: &Ledger,
    soul: &str,
) -> Result<Option<String>> {
    let mut line = first;
    let mut refused: Vec<String> = Vec::new();
    for n in 0..ASKS {
        match memory::check(&line) {
            Ok(ok) => return Ok(Some(ok)),
            Err(why) if n + 1 < ASKS => {
                refused.push(format!("{why} — {line}"));
                // In a session the writer has its answer in front of it, so
                // it gets what OptMem's agent gets: the refusal alone.
                let again = match merge {
                    None => format!("You noted:\n{line}\n\n{why}"),
                    Some(_) if llm.context_tokens() > 0 => why.clone(),
                    Some(prompt) => format!("{prompt}\n\nYou answered:\n{line}\n\n{why}"),
                };
                let (v, i, o) = call(llm, sys, &again, &summary_schema())?;
                ledger.record(soul, i, o)?;
                r.input_tokens += i;
                r.output_tokens += o;
                r.reasked += 1;
                line = v["text"].as_str().unwrap_or("").to_string();
            }
            Err(why) => {
                refused.push(format!("{why} — {line}"));
                log.append_dropped(&refused.iter().map(|x| format!("refused: {x}")).collect::<Vec<_>>())?;
                return Ok(None);
            }
        }
    }
    Ok(None)
}

/// Answer every merge due over the log, in OptMem's order. `false` when the
/// run must stop (`r.stopped` says why).
fn nap(
    llm: &dyn Llm,
    sys: &str,
    log: &MemoryLog,
    r: &mut ReliveReport,
    ledger: &Ledger,
    soul: &str,
    take_call: &mut dyn FnMut(&mut ReliveReport) -> bool,
) -> Result<bool> {
    let memories = log.memories()?;
    let mut sums = log.summaries()?;
    let t = memories.len();
    loop {
        let todo = memory_log::pending(&sums, t);
        let Some(&b) = todo.first() else {
            return Ok(true);
        };
        let prompt = memory_log::nap_prompt(&memories, &sums, b, todo.len() - 1)?;
        if !take_call(r) {
            return Ok(false);
        }
        let (v, i, o) = match call(llm, sys, &prompt, &summary_schema()) {
            Ok(x) => x,
            Err(e) => {
                r.stopped = Some(format!("merge #{}: {e:#}", memory::block_id(b)));
                return Ok(false);
            }
        };
        ledger.record(soul, i, o)?;
        r.input_tokens += i;
        r.output_tokens += o;
        let first = v["text"].as_str().unwrap_or("").to_string();
        let Some(text) = ask_line(llm, sys, first, Some(&prompt), log, r, ledger, soul)? else {
            r.stopped = Some(format!(
                "merge #{}: no line passing OptMem's check after {ASKS} asks",
                memory::block_id(b)
            ));
            return Ok(false);
        };
        let sm = Summary {
            block: memory::block_id(b),
            text,
            model: llm.model().to_string(),
            written: chrono::Utc::now().to_rfc3339(),
        };
        log.append_summary(&sm)?;
        sums.insert(sm.block.clone(), sm);
        r.merges += 1;
    }
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
    peers: &PeerIndex,
    from: &str,
    to: &str,
    max_calls: usize,
    session_tokens: u64,
) -> Result<ReliveReport> {
    let mut r = ReliveReport::default();
    let soul_md: String = std::fs::read_to_string(repo.join("SOUL.md"))
        .unwrap_or_default()
        .chars()
        .take(SOUL_CHARS)
        .collect();
    let sys = system(&sp.agent, soul_md.trim());
    let mut convs = conversations(repo)?;
    label_peers(&mut convs, &sp.agent, peers);
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

    // Merges left over from a run that stopped mid-nap come first: OptMem's
    // wake will not print with a needed block missing.
    if !nap(llm, &sys, log, &mut r, ledger, soul, &mut take_call)? {
        llm.reset();
        return Ok(r);
    }
    // The reading starts by waking.
    llm.reset();
    for stretch in stretches(st) {
        // 1. Wake, as OptMem would print it now — at the start of every
        //    session (every call, read cold). A session past its budget
        //    ends, and the next begins by waking: OptMem's compaction.
        if session_tokens > 0 && llm.context_tokens() > session_tokens {
            llm.reset();
            r.sessions += 1;
        }
        let wake = (llm.context_tokens() == 0).then(|| -> Result<String> {
            let memories = log.memories()?;
            let sums = log.summaries()?;
            Ok(wake_text(&wake_view(&memories, &sums, memories.len(), WAKE_LINES)))
        });
        let wake = wake.transpose()?;
        // 2. Note. A stretch is never skipped: memory is built in order, so a
        //    gap read later would land behind notes that came after it.
        let prompt = note_prompt(wake.as_deref(), &stretch, &convs, sp);
        if !take_call(&mut r) {
            return Ok(r);
        }
        let (v, i, o) = match call(llm, &sys, &prompt, &note_schema()) {
            Ok(x) => x,
            Err(e) => {
                r.stopped = Some(format!("stretch at {}: {e:#}", stretch[0].turn.ts));
                return Ok(r);
            }
        };
        ledger.record(soul, i, o)?;
        r.input_tokens += i;
        r.output_tokens += o;
        let mut notes = Vec::new();
        let mut dropped = Vec::new();
        for n in v["notes"].as_array().cloned().unwrap_or_default() {
            let raw = n["text"].as_str().unwrap_or("").to_string();
            if raw.trim().is_empty() {
                continue;
            }
            let Some(text) = ask_line(llm, &sys, raw.clone(), None, log, &mut r, ledger, soul)?
            else {
                dropped.push(format!("failed OptMem's check after {ASKS} asks: {raw}"));
                continue;
            };
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
            let Some(first) = cited.first() else {
                dropped.push(format!("cites no turn of the stretch: {text}"));
                continue;
            };
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
        // 3. Every merge that came due, before reading on.
        if !nap(llm, &sys, log, &mut r, ledger, soul, &mut take_call)? {
            llm.reset();
            return Ok(r);
        }
    }
    llm.reset();
    Ok(r)
}

/// What `verify` found: counts, and every failure by name. A run is good
/// only when `failures` is empty.
#[derive(Debug, Default, serde::Serialize)]
pub struct Verified {
    pub notes: usize,
    pub blocks: usize,
    pub turn_links: usize,
    pub wake_lines: usize,
    pub nt_triples: usize,
    pub turns_in_store: Option<usize>,
    pub failures: Vec<String>,
}

/// Check a relive's output end to end against OptMem's rules and the soul's
/// own records: every line passes `check`; every block due is built; every
/// note cites turns of the soul's stream; the wake is OptMem's cover; the
/// projection loads; and every cited turn is a Turn in the soul's store.
pub fn verify(repo: &Path, log: &MemoryLog) -> Result<Verified> {
    let mut v = Verified::default();
    let ms = log.memories()?;
    let sums = log.summaries()?;
    v.notes = ms.len();
    v.blocks = sums.len();
    for (i, m) in ms.iter().enumerate() {
        if let Err(e) = memory::check(&m.text) {
            v.failures.push(format!("note #{i}: {e}"));
        }
        if m.turns.is_empty() {
            v.failures.push(format!("note #{i} cites no turn"));
        }
    }
    for (id, s) in &sums {
        match memory::parse_block(id) {
            None => v.failures.push(format!("block {id:?} is not an OptMem block")),
            Some((_, hi)) if hi > ms.len() => {
                v.failures.push(format!("block {id} reaches past the log ({} notes)", ms.len()))
            }
            _ => {}
        }
        if let Err(e) = memory::check(&s.text) {
            v.failures.push(format!("block {id}: {e}"));
        }
    }
    for b in memory_log::pending(&sums, ms.len()) {
        v.failures.push(format!("block #{} is due and not built", memory::block_id(b)));
    }
    // Cited turns are turns of the soul's own stream.
    let convs = conversations(repo)?;
    let ids: HashSet<&str> = convs
        .iter()
        .flat_map(|c| c.turns.iter().map(|t| t.id.as_str()))
        .collect();
    let mut cited: Vec<&str> = Vec::new();
    for (i, m) in ms.iter().enumerate() {
        for t in &m.turns {
            v.turn_links += 1;
            cited.push(t);
            if !ids.contains(t.as_str()) {
                v.failures.push(format!("note #{i} cites {t}, not a turn of this soul"));
            }
        }
    }
    // Wake is OptMem's cover.
    let wake = wake_view(&ms, &sums, ms.len(), WAKE_LINES);
    v.wake_lines = wake.len();
    let want = memory::cover(ms.len(), WAKE_LINES);
    if wake.len() != want.len() {
        v.failures.push(format!("wake has {} lines, cover {}", wake.len(), want.len()));
    }
    for l in wake.iter().filter(|l| l.pending) {
        v.failures.push(format!("wake line {} has no merge", l.label));
    }
    // The projection loads, and its edges land on real nodes.
    let nt = memory_log::project_nt(&ms, &sums);
    v.nt_triples = nt.lines().count();
    let mem = oxigraph::store::Store::new()?;
    mem.load_from_reader(oxigraph::io::RdfFormat::NTriples, nt.as_bytes())?;
    let dangling = crate::graph::query_rows(
        &mem,
        "SELECT (COUNT(*) AS ?n) WHERE { ?m <https://repolex.ai/ontology/git-lex/relatedToId> ?c . FILTER(STRSTARTS(STR(?c), \"https://repolex.ai/ravel/Memory/\")) FILTER NOT EXISTS { ?c a <https://repolex.ai/ontology/ravel/Memory> } }",
    )?;
    let n: usize = dangling
        .first()
        .and_then(|r| r.get("n"))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    if n > 0 {
        v.failures.push(format!("{n} block edges point at no Memory node"));
    }
    // Every cited turn is a Turn node in the soul's own store.
    let store_dir = repo.join(crate::sync::STORE_SUBDIR);
    if store_dir.join("CURRENT").exists() {
        let store = crate::graph::open_read_only(&store_dir)?;
        cited.sort();
        cited.dedup();
        let mut found = 0;
        for chunk in cited.chunks(200) {
            let values: String = chunk
                .iter()
                .map(|t| format!("<{}Turn/{t}> ", crate::RAVEL_BASE))
                .collect();
            let q = format!(
                "SELECT (COUNT(DISTINCT ?t) AS ?n) WHERE {{ VALUES ?t {{ {values} }} GRAPH ?g {{ ?t a <https://repolex.ai/ontology/ravel/Turn> }} }}"
            );
            found += crate::graph::query_rows(&store, &q)?
                .first()
                .and_then(|r| r.get("n"))
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
        }
        v.turns_in_store = Some(found);
        if found != cited.len() {
            v.failures.push(format!(
                "{} of {} cited turns are not Turn nodes in the soul's store",
                cited.len() - found,
                cited.len()
            ));
        }
    } else {
        v.failures.push("the soul has no store to check cited turns against".into());
    }
    Ok(v)
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
            peer: None,
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

    /// Notes one line per turn shown; merges answer with the block id, and
    /// answers the first merge too long once, to exercise OptMem's re-ask.
    struct Fake(std::sync::Mutex<usize>);
    impl Llm for Fake {
        fn model(&self) -> &str {
            "fake"
        }
        fn json(&self, _: &str, user: &str, _: &Value, _: u32) -> Result<(Value, u64, u64)> {
            if let Some(rest) = user.strip_prefix("Compress memories #") {
                let id = rest.split_whitespace().next().unwrap().to_string();
                let mut n = self.0.lock().unwrap();
                *n += 1;
                let text = if *n == 1 { "x".repeat(300) } else { format!("S{id}") };
                return Ok((json!({ "text": text }), 1, 1));
            }
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
    fn every_due_merge_is_built_in_optmem_order_before_reading_on() {
        let base = std::env::temp_dir().join(format!("ravel-relive-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("soul");
        let mirror = repo.join(crate::sync::TRANSCRIPTS_SUBDIR).join("p");
        std::fs::create_dir_all(&mirror).unwrap();
        // 6 turns of ~10,000 characters, clipped to ~4,000: two stretches of
        // ~5 and ~1 turns, one note each.
        let big = "z".repeat(10_000);
        let rec = |k: usize| {
            json!({"type":"user","uuid":format!("u{k}"),"parentUuid":if k==0 {String::new()} else {format!("u{}",k-1)},
                   "timestamp":format!("2026-06-01T10:0{k}:00Z"),"sessionId":"s",
                   "message":{"role":"user","content":big}})
            .to_string()
        };
        std::fs::write(
            mirror.join("s1.jsonl"),
            (0..6).map(rec).collect::<Vec<_>>().join("\n"),
        )
        .unwrap();
        let log = MemoryLog::at(base.join("out"));
        let ledger = Ledger::open_at(base.join("spend.tsv"), 1.0).unwrap();
        let sp = Speakers {
            agent: "soul".into(),
            peers: vec![],
            human: "human".into(),
        };
        let fake = Fake(std::sync::Mutex::new(0));
        let r = relive(&repo, "s", &sp, &fake, &ledger, &log, &PeerIndex::default(), "2026", "2027", 100, 0).unwrap();
        assert!(r.stopped.is_none(), "{r:?}");
        assert_eq!(r.notes, 6, "{r:?}");
        // 6 notes: blocks 0-1, 2-3, 4-5, 0-3 — exactly OptMem's pending set.
        let sums = log.summaries().unwrap();
        let mut ids: Vec<&str> = sums.keys().map(String::as_str).collect();
        ids.sort();
        assert_eq!(ids, ["0-1", "0-3", "2-3", "4-5"]);
        assert_eq!(r.merges, 4);
        assert_eq!(r.reasked, 1, "the too-long first merge was asked again");
        assert!(memory_log::pending(&sums, 6).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
