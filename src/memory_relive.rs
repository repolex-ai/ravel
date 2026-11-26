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
    clip, conversations, relabel_channels, summary_schema, Conversation, ProseTurn, Speakers,
};
use crate::memory_log::{self, wake_text, wake_view, Memory, MemoryLog, Summary};
use crate::peers::{PeerIndex, Sent};
use crate::writer::{is_usage_limit, Llm};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

/// Prose shown per step. Small enough that "now" stays close to every turn.
pub const STRETCH_CHARS: usize = 24_000;
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
    let rules = format!("Turns marked 'you' are yours. Turns marked '{h}' are {h}'s, your human's, except text marked as a message from another agent or as pasted, which is someone else's words. Turns marked with another agent's name are that agent's. You see what was said, and each tool call as one line in brackets with how it came out (`[ran: … → …]`); at the time, you saw the whole output too. A bracketed line in a turn of yours is you doing that yourself: a check you ran is your own check, not someone's report of one. The numbers in brackets are only for citing turns: a memory must make sense on its own later, so never put a turn number in one.");
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
/// limit or a usage-policy refusal returns at once.
fn call(llm: &dyn Llm, system: &str, user: &str, schema: &Value) -> Result<(Value, u64, u64)> {
    let mut wait = 30;
    for attempt in 1.. {
        match llm.json(system, user, schema) {
            Ok(x) => return Ok(x),
            // The subscription is out of allowance until it resets: every
            // call would fail the same way, so the pass stops and waits.
            Err(e) if is_usage_limit(&e) => return Err(e),
            // The same words get the same refusal: waiting does not help, and
            // the caller decides what a refused stretch becomes.
            Err(e) if is_policy_refusal(&e) => return Err(e),
            // A writer that declines to answer in this session declines again:
            // the caller starts a fresh one instead of waiting.
            Err(e) if is_balk(&e) => return Err(e),
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
        let first = c
            .turns
            .iter()
            .find(|t| t.role == "user" && !t.text.trim().is_empty());
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
    /// Stretches the model's usage policy refused twice, left blank and
    /// logged in `dropped.tsv`.
    pub refused: usize,
    /// The run gave its turn up before the end — its call limit, or raveld
    /// stopping — with nothing wrong. The rest is read next time.
    pub yielded: bool,
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
        out += &format!(
            "==================== stretch {} ====================\n",
            k + 1
        );
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
    soul: &str,
) -> Result<Option<String>> {
    let mut line = first;
    let mut refused: Vec<String> = Vec::new();
    for n in 0..ASKS {
        let block = merge.and_then(|p| {
            p.strip_prefix("Compress memories #")?
                .split_whitespace()
                .next()
        });
        match memory::check(&line)
            .and_then(not_wrapped)
            .and_then(|l| a_memory(l, block))
        {
            Ok(ok) => return Ok(Some(ok)),
            Err(why) if n + 1 < ASKS => {
                refused.push(format!("{why} — {line}"));
                // A session refused three times in a row can dig in: on
                // spaceGOAT one answered "5 lines" with 7, then 9, up to 27.
                // From the fourth ask on, a fresh session gets the whole ask.
                if n == 3 && llm.context_tokens() > 0 {
                    llm.reset();
                }
                // In a session the writer has its answer in front of it, so
                // it gets what OptMem's agent gets: the refusal alone.
                let again = match merge {
                    None => format!("You noted:\n{line}\n\n{why}"),
                    Some(_) if llm.context_tokens() > 0 => why.clone(),
                    Some(prompt) => format!("{prompt}\n\nYou answered:\n{line}\n\n{why}"),
                };
                let cold = || match merge {
                    None => format!("You noted:\n{line}\n\n{why}"),
                    Some(prompt) => format!("{prompt}\n\nYou answered:\n{line}\n\n{why}"),
                };
                let (v, i, o) = call_or_fresh(llm, sys, &again, &cold, &summary_schema())
                    .with_context(|| format!("asking again for a line refused by check: {line}"))?;
                log.append_usage(soul, i, o)?;
                r.input_tokens += i;
                r.output_tokens += o;
                r.reasked += 1;
                line = v["text"].as_str().unwrap_or("").to_string();
            }
            Err(why) => {
                refused.push(format!("{why} — {line}"));
                log.append_dropped(
                    &refused
                        .iter()
                        .map(|x| format!("refused: {x}"))
                        .collect::<Vec<_>>(),
                )?;
                return Ok(None);
            }
        }
    }
    Ok(None)
}

/// A refusal under the model's usage policy, as `claude -p` reports it. Not an
/// outage: the same text will be refused again, so it is never retried
/// more than once.
fn is_policy_refusal(e: &anyhow::Error) -> bool {
    let m = format!("{e:#}");
    m.contains("anthropic.com/legal/aup") || m.contains("can't help with this")
}

/// The writer answered in words instead of the line it owed. On TR1P, deep in
/// a session: "I won't call StructuredOutput again for this. The nine lines
/// are already saved…". The session believes it is done; a fresh one is not.
fn is_balk(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("returned no structured output")
}

/// [`call`], and if a session balks, once more in a fresh session with
/// `cold`: the same ask written for a writer that has not seen it.
fn call_or_fresh(
    llm: &dyn Llm,
    sys: &str,
    user: &str,
    cold: &dyn Fn() -> String,
    schema: &Value,
) -> Result<(Value, u64, u64)> {
    match call(llm, sys, user, schema) {
        Err(e) if is_balk(&e) && llm.context_tokens() > 0 => {
            llm.reset();
            call(llm, sys, &cold(), schema)
        }
        r => r,
    }
}

/// A line that is itself JSON is a writer answering in the shape it was asked
/// for once, not with the line: Sonnet 5.5, re-asked to shorten a note, gave
/// `[{"text": "…", "turns": [5]}]` as the line 14 times on nug3. OptMem's
/// `check` cannot see it (one line, short enough), so it is refused here and
/// asked again like any other refusal, never unwrapped by guesswork.
fn not_wrapped(line: String) -> Result<String, String> {
    let t = line.trim_start();
    let json = (t.starts_with('{') || t.starts_with('['))
        && serde_json::from_str::<Value>(&line).is_ok_and(|v| v.is_object() || v.is_array());
    if json {
        Err("That is JSON. Give the memory itself: one plain line of text.".into())
    } else {
        Ok(line)
    }
}

/// A line that talks about the merging instead of merging. On W3BL0RD, after
/// a too-long refusal, Haiku read OptMem's "N compressions remain after this
/// one" as a budget it had spent, and answered four merges with "I haven't
/// compressed memories #656-659. The compression budget was used up…". It also
/// opened 21 good merges with their own label ("#740-741 compressed: …").
/// `check` passes both. A merge (`block` is its id) may not open with its own
/// label; no line may talk about compressing instead of doing it.
fn a_memory(line: String, block: Option<&str>) -> Result<String, String> {
    let low = line.to_lowercase();
    const META: [&str; 10] = [
        "compression budget",
        "haven't compressed",
        "have not compressed",
        "compressions remain",
        "raise the limit",
        "remain in memory unchanged",
        "not compressing",
        "no line written",
        "unsaved range",
        "already saved as",
    ];
    if META.iter().any(|m| low.contains(m)) {
        return Err("That is about the task, not a memory. There is no budget to save: write the merged line itself.".into());
    }
    if let Some(id) = block {
        let t = low.trim_start();
        if t.starts_with(&format!("#{id}"))
            || t.starts_with("compressed #")
            || t.starts_with("compressed:")
        {
            return Err("Give the line itself, without its number or a label.".into());
        }
    }
    Ok(line)
}

/// Build block `b`'s merge from its notes or halves as they stand in `sums`,
/// with OptMem's prompt (`extra`, when given, appended), the same checks and
/// re-asks as every merge, and append it. `false` when the run must stop
/// (`r.stopped` says why).
#[allow(clippy::too_many_arguments)]
fn merge(
    llm: &dyn Llm,
    sys: &str,
    log: &MemoryLog,
    r: &mut ReliveReport,
    soul: &str,
    memories: &[Memory],
    sums: &mut std::collections::HashMap<String, Summary>,
    b: memory::Block,
    left: usize,
    extra: Option<&str>,
) -> Result<bool> {
    let mut prompt = memory_log::nap_prompt(memories, sums, b, left)?;
    if let Some(x) = extra {
        prompt = format!("{prompt}\n{x}\n");
    }
    let (v, i, o) = match call_or_fresh(llm, sys, &prompt, &|| prompt.clone(), &summary_schema()) {
        Ok(x) => x,
        Err(e) => {
            r.stopped = Some(format!("merge #{}: {e:#}", memory::block_id(b)));
            return Ok(false);
        }
    };
    log.append_usage(soul, i, o)?;
    r.input_tokens += i;
    r.output_tokens += o;
    let first = v["text"].as_str().unwrap_or("").to_string();
    let Some(text) = ask_line(llm, sys, first, Some(&prompt), log, r, soul)? else {
        r.stopped = Some(format!(
            "merge #{}: no line passing the checks after {ASKS} asks",
            memory::block_id(b)
        ));
        return Ok(false);
    };
    let sm = Summary {
        block: memory::block_id(b),
        text,
        model: llm.model().to_string(),
        written: chrono::Utc::now().to_rfc3339(),
        // A rebuild with extra words is a trial, not the current wording.
        wording: Some(
            if extra.is_some() {
                "trial"
            } else {
                memory_log::WORDING
            }
            .to_string(),
        ),
    };
    log.append_summary(&sm)?;
    sums.insert(sm.block.clone(), sm);
    r.merges += 1;
    Ok(true)
}

/// Answer every merge due over the log, in OptMem's order. `false` when the
/// run must stop (`r.stopped` says why).
fn nap(
    llm: &dyn Llm,
    sys: &str,
    log: &MemoryLog,
    r: &mut ReliveReport,
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
        if !take_call(r) {
            return Ok(false);
        }
        if !merge(
            llm,
            sys,
            log,
            r,
            soul,
            &memories,
            &mut sums,
            b,
            todo.len() - 1,
            None,
        )? {
            return Ok(false);
        }
    }
}

/// Every built block `b` and each aligned block above it that was built,
/// smallest first, left to right, as OptMem builds.
fn with_ancestors(
    sums: &std::collections::HashMap<String, Summary>,
    bad: impl Iterator<Item = memory::Block>,
) -> Vec<memory::Block> {
    let mut out: std::collections::BTreeSet<(usize, usize)> = Default::default();
    for b in bad {
        let (mut lo, mut n) = (b.0, b.1 - b.0);
        loop {
            let up = (lo, lo + n);
            if sums.contains_key(&memory::block_id(up)) {
                out.insert((n, lo));
            } else if up != b {
                break;
            }
            n *= 2;
            lo -= lo % n;
        }
    }
    out.into_iter().map(|(n, lo)| (lo, lo + n)).collect()
}

/// The merges `verify` refuses, with every merge built on top of them, in the
/// order they must be rebuilt: smallest first, left to right, as OptMem builds.
pub fn to_repair(log: &MemoryLog) -> Result<Vec<memory::Block>> {
    let sums = log.summaries()?;
    let bad: Vec<memory::Block> = sums
        .iter()
        .filter(|(id, s)| {
            memory::check(&s.text)
                .and_then(not_wrapped)
                .and_then(|l| a_memory(l, Some(id)))
                .is_err()
        })
        .filter_map(|(id, _)| memory::parse_block(id))
        .collect();
    Ok(with_ancestors(&sums, bad.into_iter()))
}

/// The merges written in an older wording than [`memory_log::WORDING`], in
/// the order they must be rebuilt: smallest first, so every merge is rebuilt
/// from halves already in the current wording.
pub fn to_reword(log: &MemoryLog) -> Result<Vec<memory::Block>> {
    let sums = log.summaries()?;
    let old: Vec<memory::Block> = sums
        .iter()
        .filter(|(_, s)| s.wording.as_deref() != Some(memory_log::WORDING))
        .filter_map(|(id, _)| memory::parse_block(id))
        .collect();
    Ok(with_ancestors(&sums, old.into_iter()))
}

fn soul_system(repo: &Path, soul: &str) -> String {
    let soul_md: String = std::fs::read_to_string(repo.join("SOUL.md"))
        .unwrap_or_default()
        .chars()
        .take(SOUL_CHARS)
        .collect();
    system(soul, soul_md.trim())
}

/// Rebuild `todo`'s merges in order, each from its notes or halves as they
/// stand now, with the same prompt and checks as a run. A new line is
/// appended and wins over the old; nothing is deleted, and no turn is read.
/// At most `max_calls` merges; `r.yielded` when more were left.
fn rebuild_blocks(
    llm: &dyn Llm,
    repo: &Path,
    soul: &str,
    log: &MemoryLog,
    todo: &[memory::Block],
    max_calls: usize,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<ReliveReport> {
    let mut r = ReliveReport::default();
    let sys = soul_system(repo, soul);
    let memories = log.memories()?;
    let mut sums = log.summaries()?;
    for (k, &b) in todo.iter().enumerate() {
        if k >= max_calls || stop.load(std::sync::atomic::Ordering::Relaxed) {
            r.yielded = true;
            break;
        }
        // A long run of merges in one session ends, and the next begins fresh.
        if llm.context_tokens() > 90_000 {
            llm.reset();
        }
        if !merge(
            llm,
            &sys,
            log,
            &mut r,
            soul,
            &memories,
            &mut sums,
            b,
            todo.len() - 1 - k,
            None,
        )? {
            break;
        }
    }
    Ok(r)
}

/// Rebuild the merges [`to_repair`] lists.
pub fn repair(llm: &dyn Llm, repo: &Path, soul: &str, log: &MemoryLog) -> Result<ReliveReport> {
    let todo = to_repair(log)?;
    rebuild_blocks(llm, repo, soul, log, &todo, usize::MAX, &Default::default())
}

/// Rebuild up to `max_calls` of the merges [`to_reword`] lists: a memory
/// moves to a new merge wording a few merges a pass, with no script.
pub fn reword(
    llm: &dyn Llm,
    repo: &Path,
    soul: &str,
    log: &MemoryLog,
    max_calls: usize,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<ReliveReport> {
    let todo = to_reword(log)?;
    rebuild_blocks(llm, repo, soul, log, &todo, max_calls, stop)
}

/// [`reword`], with the merges of one size written side by side by up to
/// `workers` calls at once. Merges of one size never read each other: each
/// reads only its two halves, one size down, already rewritten. So a level is
/// asked in parallel, then kept in order, and the next level starts only when
/// it is whole. For a one-shot writer that may be called from several threads
/// at once ([`crate::writer::AnthropicApi`]); a session writer would mix its
/// sessions. At most `limit` merges. A failure stops the run after the level
/// it happened in, keeping that level's good lines.
pub fn reword_parallel(
    llm: &dyn Llm,
    repo: &Path,
    soul: &str,
    log: &MemoryLog,
    workers: usize,
    limit: usize,
    stop: &std::sync::atomic::AtomicBool,
    progress: &dyn Fn(&ReliveReport, usize),
) -> Result<ReliveReport> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let todo: Vec<memory::Block> = to_reword(log)?.into_iter().take(limit).collect();
    let sys = soul_system(repo, soul);
    let memories = log.memories()?;
    let mut sums = log.summaries()?;
    let mut r = ReliveReport::default();
    let mut i = 0;
    while i < todo.len() {
        if stop.load(Ordering::Relaxed) {
            r.yielded = true;
            break;
        }
        let size = todo[i].1 - todo[i].0;
        let j = i + todo[i..].iter().take_while(|b| b.1 - b.0 == size).count();
        let level = &todo[i..j];
        let prompts = level
            .iter()
            .enumerate()
            .map(|(k, &b)| memory_log::nap_prompt(&memories, &sums, b, todo.len() - 1 - (i + k)))
            .collect::<Result<Vec<_>>>()?;
        let next = AtomicUsize::new(0);
        let got = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..workers.max(1).min(level.len()) {
                s.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    if k >= level.len() || stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let out = one_merge_line(llm, &sys, &prompts[k], log, soul);
                    got.lock().unwrap().push((k, out));
                });
            }
        });
        let mut got = got.into_inner().unwrap();
        got.sort_by_key(|(k, _)| *k);
        for (k, out) in got {
            let b = level[k];
            match out {
                Ok((Some(text), rr)) => {
                    r.input_tokens += rr.input_tokens;
                    r.output_tokens += rr.output_tokens;
                    r.reasked += rr.reasked;
                    let sm = Summary {
                        block: memory::block_id(b),
                        text,
                        model: llm.model().to_string(),
                        written: chrono::Utc::now().to_rfc3339(),
                        wording: Some(memory_log::WORDING.to_string()),
                    };
                    log.append_summary(&sm)?;
                    sums.insert(sm.block.clone(), sm);
                    r.merges += 1;
                }
                Ok((None, rr)) => {
                    r.input_tokens += rr.input_tokens;
                    r.output_tokens += rr.output_tokens;
                    r.stopped.get_or_insert(format!(
                        "merge #{}: no line passing the checks after {ASKS} asks",
                        memory::block_id(b)
                    ));
                }
                Err(e) => {
                    r.stopped.get_or_insert(format!("merge #{}: {e:#}", memory::block_id(b)));
                }
            }
        }
        progress(&r, todo.len());
        if r.stopped.is_some() {
            break;
        }
        // A level cut short by `stop` is not whole: the next one would read
        // halves still in the old wording.
        if r.merges < j {
            r.yielded = true;
            break;
        }
        i = j;
    }
    Ok(r)
}

/// One merge's line, asked and checked, not yet kept.
fn one_merge_line(
    llm: &dyn Llm,
    sys: &str,
    prompt: &str,
    log: &MemoryLog,
    soul: &str,
) -> Result<(Option<String>, ReliveReport)> {
    let mut rr = ReliveReport::default();
    let (v, i, o) = call_or_fresh(llm, sys, prompt, &|| prompt.to_string(), &summary_schema())?;
    log.append_usage(soul, i, o)?;
    rr.input_tokens += i;
    rr.output_tokens += o;
    let first = v["text"].as_str().unwrap_or("").to_string();
    let line = ask_line(llm, sys, first, Some(prompt), log, &mut rr, soul)?;
    Ok((line, rr))
}

/// Build every merge due over `log`'s notes, cold, from nothing but the notes:
/// a side-by-side test of the merge wording. `extra`, when given, is appended
/// to the merge prompt, so the only difference between two rebuilds of the
/// same notes is that text. No turn is read.
pub fn rebuild(
    llm: &dyn Llm,
    repo: &Path,
    soul: &str,
    log: &MemoryLog,
    extra: Option<&str>,
) -> Result<ReliveReport> {
    let mut r = ReliveReport::default();
    let sys = soul_system(repo, soul);
    let memories = log.memories()?;
    let mut sums = log.summaries()?;
    loop {
        let todo = memory_log::pending(&sums, memories.len());
        let Some(&b) = todo.first() else { return Ok(r) };
        if !merge(
            llm,
            &sys,
            log,
            &mut r,
            soul,
            &memories,
            &mut sums,
            b,
            todo.len() - 1,
            extra,
        )? {
            return Ok(r);
        }
    }
}

/// A soul's conversations as the reader sees them: every mirror read, each
/// peer's message marked from the sender's side, the soul's own subagents
/// dropped. The slow part of a pass (every soul's sends are read), and the
/// only part that reads the mirrors, so raveld holds its gate for this alone.
pub fn load(repo: &Path, sp: &Speakers, peers: &PeerIndex) -> Result<Vec<Conversation>> {
    let mut convs = conversations(repo)?;
    label_peers(&mut convs, &sp.agent, peers);
    Ok(convs)
}

/// The same reader's view as [`load`], holding only conversations with turns
/// the memory has not read, each from the place the pass before left it
/// (`at`), and only the sends made since the oldest of those turns. What a
/// pass costs then follows what is new, not how long the soul has lived.
/// `souls` is every served soul as (repo, name): any of them may be a sender.
pub fn load_unread(
    repo: &Path,
    sp: &Speakers,
    done: &HashSet<String>,
    at: &mut crate::memory_extract::ReadUpto,
    souls: &[(std::path::PathBuf, String)],
) -> Result<Vec<Conversation>> {
    let unread = crate::memory_extract::conversations_unread(repo, done, at)?;
    let first_input = |c: &Conversation| {
        c.turns
            .iter()
            .find(|t| t.role == "user" && !t.text.trim().is_empty())
            .cloned()
    };
    // The oldest turn whose sender has to be found: an unread turn of the
    // human's side, or the first input of a conversation seen from its start.
    let since = unread
        .iter()
        .flat_map(|n| {
            let first = if n.from_start { first_input(&n.conv) } else { None };
            n.conv
                .turns
                .iter()
                .filter(|t| t.role == "user" && !t.text.trim().is_empty() && !done.contains(&t.id))
                .cloned()
                .chain(first)
        })
        .filter_map(|t| crate::peers::unix(&t.ts))
        .min();
    let idx = match since {
        Some(t) => PeerIndex::since(souls, t),
        None => PeerIndex::default(),
    };
    let mut convs = Vec::new();
    for n in unread {
        // The soul's own subagent: never read, and not opened again.
        if n.from_start
            && first_input(&n.conv).is_some_and(|t| idx.spawned(&sp.agent, &t.ts, &t.text))
        {
            at.is_subagent(&n);
            continue;
        }
        let mut c = n.conv;
        for t in c.turns.iter_mut().filter(|t| t.role == "user") {
            t.peer = idx.sender_of(&sp.agent, &t.ts, &t.text);
        }
        convs.push(c);
    }
    at.save()?;
    Ok(convs)
}

/// What is left to read before `to`: (turns, prose characters, the oldest
/// such turn's time). How raveld decides a pass is worth a call.
pub fn unread(convs: &[Conversation], log: &MemoryLog, to: &str) -> Result<(usize, usize, String)> {
    let done = log.extracted()?;
    let st: Vec<StreamTurn> = stream(convs)
        .into_iter()
        .filter(|s| s.turn.ts.as_str() < to && !done.contains(&s.turn.id))
        .collect();
    let chars = st.iter().map(|s| clip(s.turn.text.trim()).len()).sum();
    let oldest = st.first().map(|s| s.turn.ts.clone()).unwrap_or_default();
    Ok((st.len(), chars, oldest))
}

/// Relive a soul's turns in `[from, to)` (RFC 3339 strings) into `log`,
/// resuming after the turns `log` has already read. `convs` is [`load`]'s.
#[allow(clippy::too_many_arguments)]
pub fn relive(
    repo: &Path,
    sp: &Speakers,
    llm: &dyn Llm,
    log: &MemoryLog,
    convs: &[Conversation],
    from: &str,
    to: &str,
    max_calls: usize,
    session_tokens: u64,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<ReliveReport> {
    let mut r = ReliveReport::default();
    let sys = soul_system(repo, &sp.agent);
    let done = log.extracted()?;
    let all: Vec<StreamTurn> = stream(convs)
        .into_iter()
        .filter(|s| s.turn.ts.as_str() >= from && s.turn.ts.as_str() < to)
        .collect();
    let st: Vec<StreamTurn> = all
        .into_iter()
        .filter(|s| !done.contains(&s.turn.id))
        .collect();
    // The stream's size: what has been read and what this run may read.
    // (`convs` may hold only the conversations with something unread.)
    log.write_stream_size(done.len() + st.len())?;
    let mut calls = 0usize;
    let mut take_call = |r: &mut ReliveReport| -> bool {
        // Giving the turn up is not a failure: the rest is read next pass.
        if calls >= max_calls || stop.load(std::sync::atomic::Ordering::Relaxed) {
            r.yielded = true;
            return false;
        }
        calls += 1;
        true
    };

    // Merges left over from a run that stopped mid-nap come first: OptMem's
    // wake will not print with a needed block missing.
    if !nap(llm, &sys, log, &mut r, &sp.agent, &mut take_call)? {
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
        let wake_now = || -> Result<String> {
            let memories = log.memories()?;
            let sums = log.summaries()?;
            Ok(wake_text(&wake_view(
                &memories,
                &sums,
                memories.len(),
                WAKE_LINES,
            )))
        };
        // 2. Note. A stretch is never put off to read later: memory is built
        //    in order, so a gap read later would land behind notes that came
        //    after it. The one exception is a stretch the model's usage
        //    policy refuses twice, once in a fresh session: it is marked read
        //    with no notes, and the gap is logged (goodlux, 2026-10-15: leave
        //    blanks, don't fight the classifier).
        let mut refusals = 0;
        let mut balked = false;
        let answer = loop {
            let wake = if llm.context_tokens() == 0 {
                Some(wake_now()?)
            } else {
                None
            };
            let prompt = note_prompt(wake.as_deref(), &stretch, convs, sp);
            if !take_call(&mut r) {
                return Ok(r);
            }
            match call(llm, &sys, &prompt, &note_schema()) {
                Ok(x) => break Some(x),
                Err(e) if is_policy_refusal(&e) && refusals == 0 => {
                    refusals += 1;
                    llm.reset();
                }
                // A balk is asked once more in a fresh session, which wakes.
                Err(e) if is_balk(&e) && !balked => {
                    balked = true;
                    llm.reset();
                }
                Err(e) if is_policy_refusal(&e) => {
                    llm.reset();
                    let ids: Vec<String> = stretch.iter().map(|s| s.turn.id.clone()).collect();
                    log.append_dropped(&[format!(
                        "refused by the model's usage policy twice, so left blank: the stretch of {} turns from {} to {}: {}",
                        stretch.len(),
                        stretch[0].turn.ts,
                        stretch[stretch.len() - 1].turn.ts,
                        format!("{e:#}").lines().next().unwrap_or("")
                    )])?;
                    log.append_chunk(&[], &ids)?;
                    r.turns += stretch.len();
                    r.stretches += 1;
                    r.refused += 1;
                    break None;
                }
                Err(e) => {
                    r.stopped = Some(format!("stretch at {}: {e:#}", stretch[0].turn.ts));
                    return Ok(r);
                }
            }
        };
        let Some((v, i, o)) = answer else { continue };
        log.append_usage(&sp.agent, i, o)?;
        r.input_tokens += i;
        r.output_tokens += o;
        let mut notes = Vec::new();
        let mut dropped = Vec::new();
        for n in v["notes"].as_array().cloned().unwrap_or_default() {
            let raw = n["text"].as_str().unwrap_or("").to_string();
            if raw.trim().is_empty() {
                continue;
            }
            let Some(text) = ask_line(llm, &sys, raw.clone(), None, log, &mut r, &sp.agent)? else {
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
        if !nap(llm, &sys, log, &mut r, &sp.agent, &mut take_call)? {
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
        if let Err(e) = memory::check(&m.text)
            .and_then(not_wrapped)
            .and_then(|l| a_memory(l, None))
        {
            v.failures.push(format!("note #{i}: {e}"));
        }
        if m.turns.is_empty() {
            v.failures.push(format!("note #{i} cites no turn"));
        }
    }
    for (id, s) in &sums {
        match memory::parse_block(id) {
            None => v
                .failures
                .push(format!("block {id:?} is not an OptMem block")),
            Some((_, hi)) if hi > ms.len() => v.failures.push(format!(
                "block {id} reaches past the log ({} notes)",
                ms.len()
            )),
            _ => {}
        }
        if let Err(e) = memory::check(&s.text)
            .and_then(not_wrapped)
            .and_then(|l| a_memory(l, Some(id)))
        {
            v.failures.push(format!("block {id}: {e}"));
        }
    }
    for b in memory_log::pending(&sums, ms.len()) {
        v.failures.push(format!(
            "block #{} is due and not built",
            memory::block_id(b)
        ));
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
                v.failures
                    .push(format!("note #{i} cites {t}, not a turn of this soul"));
            }
        }
    }
    // Wake is OptMem's cover.
    let wake = wake_view(&ms, &sums, ms.len(), WAKE_LINES);
    v.wake_lines = wake.len();
    let want = memory::cover(ms.len(), WAKE_LINES);
    if wake.len() != want.len() {
        v.failures.push(format!(
            "wake has {} lines, cover {}",
            wake.len(),
            want.len()
        ));
    }
    for l in wake.iter().filter(|l| l.pending) {
        v.failures
            .push(format!("wake line {} has no merge", l.label));
    }
    // The projection loads, and its edges land on real nodes.
    let names = crate::names::Names::for_soul(repo)?;
    let nt = memory_log::project_nt(&names, &ms, &sums);
    v.nt_triples = nt.lines().count();
    let mem = oxigraph::store::Store::new()?;
    mem.load_from_reader(oxigraph::io::RdfFormat::NTriples, nt.as_bytes())?;
    let dangling = crate::graph::query_rows(
        &mem,
        &format!(
            "SELECT (COUNT(*) AS ?n) WHERE {{ ?m <https://repolex.ai/ontology/ravel/relatedToId> ?c . FILTER(STRSTARTS(STR(?c), \"{}\")) FILTER NOT EXISTS {{ ?c a <https://repolex.ai/ontology/ravel/Memory> }} }}",
            names.memory("")
        ),
    )?;
    let n: usize = dangling
        .first()
        .and_then(|r| r.get("n"))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    if n > 0 {
        v.failures
            .push(format!("{n} block edges point at no Memory node"));
    }
    // Every cited turn is a Turn node in the soul's own store.
    let store_dir = repo.join(crate::sync::STORE_SUBDIR);
    if store_dir.join("CURRENT").exists() {
        let _hold = crate::graph::reading(&store_dir);
        let store = crate::graph::open_read_only(&store_dir)?;
        cited.sort();
        cited.dedup();
        let mut found = 0;
        for chunk in cited.chunks(200) {
            let values: String = chunk
                .iter()
                .map(|t| format!("<{}> ", names.turn(t)))
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
        v.failures
            .push("the soul has no store to check cited turns against".into());
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

    /// On a real soul, read-only: the unread turns the reader is handed are
    /// the same whether every transcript is read whole or only what is new.
    /// `RAVEL_TRIAL_REPO` is the soul, `RAVEL_TRIAL_SOULS` every sender's
    /// repo (colon-separated), `RAVEL_TRIAL_DIR` a folder outside any soul
    /// for the places file. `RAVEL_TRIAL_UNREAD_SINCE` treats turns from that
    /// time on as unread; `RAVEL_TRIAL_NEW_ONLY=1` skips the whole reading
    /// (for measuring the new one's memory alone).
    #[test]
    #[ignore]
    fn real_unread_is_the_same_read_whole_or_from_its_place() {
        let repo = std::path::PathBuf::from(std::env::var("RAVEL_TRIAL_REPO").unwrap());
        let dir = std::path::PathBuf::from(std::env::var("RAVEL_TRIAL_DIR").unwrap());
        assert!(!dir.starts_with(&repo), "the trial folder must be outside the soul");
        let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
        let souls: Vec<(std::path::PathBuf, String)> = std::env::var("RAVEL_TRIAL_SOULS")
            .unwrap()
            .split(':')
            .map(|p| (p.into(), name(Path::new(p))))
            .collect();
        let sp = Speakers {
            agent: name(&repo),
            peers: souls.iter().map(|s| s.1.clone()).filter(|n| *n != name(&repo)).collect(),
            human: "human".into(),
        };
        let mut done = MemoryLog::for_soul(&repo).extracted().unwrap();
        let since = std::env::var("RAVEL_TRIAL_UNREAD_SINCE").ok();
        let view = |convs: &[Conversation], done: &HashSet<String>| -> Vec<String> {
            stream(convs)
                .into_iter()
                .filter(|s| !done.contains(&s.turn.id))
                .map(|s| {
                    format!(
                        "{} {} {} {:?} [{}] {}",
                        s.turn.ts,
                        s.turn.id,
                        s.turn.role,
                        s.turn.peer,
                        where_label(&convs[s.conv]),
                        s.turn.text
                    )
                })
                .collect()
        };
        let new_only = std::env::var("RAVEL_TRIAL_NEW_ONLY").is_ok();
        let whole = if new_only {
            None
        } else {
            let t = std::time::Instant::now();
            let idx = PeerIndex::build(&souls);
            let convs = load(&repo, &sp, &idx).unwrap();
            if let Some(since) = &since {
                for s in stream(&convs) {
                    if s.turn.ts.as_str() >= since.as_str() {
                        done.remove(&s.turn.id);
                    }
                }
            }
            let v = view(&convs, &done);
            eprintln!("whole: {} unread turns in {:.1?}", v.len(), t.elapsed());
            Some(v)
        };
        let file = dir.join("read-upto.tsv");
        for pass in 1..=3 {
            let t = std::time::Instant::now();
            let mut at = crate::memory_extract::ReadUpto::open(file.clone(), 0);
            let convs = load_unread(&repo, &sp, &done, &mut at, &souls).unwrap();
            let v = view(&convs, &done);
            eprintln!(
                "pass {pass}: {} unread turns from {} conversations in {:.2?}",
                v.len(),
                convs.len(),
                t.elapsed()
            );
            if let Some(w) = &whole {
                for (a, b) in w.iter().zip(&v) {
                    assert_eq!(a, b, "pass {pass}");
                }
                assert_eq!(w.len(), v.len(), "pass {pass}");
            }
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
        fn json(&self, _: &str, user: &str, _: &Value) -> Result<(Value, u64, u64)> {
            if let Some(rest) = user.strip_prefix("Compress memories #") {
                let id = rest.split_whitespace().next().unwrap().to_string();
                let mut n = self.0.lock().unwrap();
                *n += 1;
                let text = if *n == 1 {
                    "x".repeat(300)
                } else {
                    format!("S{id}")
                };
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
        let sp = Speakers {
            agent: "soul".into(),
            peers: vec![],
            human: "human".into(),
        };
        let fake = Fake(std::sync::Mutex::new(0));
        let convs = load(&repo, &sp, &PeerIndex::default()).unwrap();
        let r = relive(
            &repo,
            &sp,
            &fake,
            &log,
            &convs,
            "2026",
            "2027",
            100,
            0,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
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

    /// Refuses, as the usage policy does, any note prompt that shows a turn
    /// marked FORBIDDEN; otherwise notes one line per turn and merges plainly.
    struct Refuser(std::sync::Mutex<usize>);
    impl Llm for Refuser {
        fn model(&self) -> &str {
            "fake"
        }
        fn json(&self, _: &str, user: &str, _: &Value) -> Result<(Value, u64, u64)> {
            if let Some(rest) = user.strip_prefix("Compress memories #") {
                let id = rest.split_whitespace().next().unwrap().to_string();
                return Ok((json!({ "text": format!("S{id}") }), 1, 1));
            }
            if user.contains("FORBIDDEN") {
                *self.0.lock().unwrap() += 1;
                anyhow::bail!("claude -p failed: API Error: Haiku 5.5 can't help with this. Start a new session to continue.\n\nLearn more: https://www.anthropic.com/legal/aup");
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
    fn a_writer_that_balks_once_is_asked_again_and_the_run_reads_on() {
        struct Balker(std::sync::Mutex<bool>);
        impl Llm for Balker {
            fn model(&self) -> &str {
                "fake"
            }
            fn json(&self, _: &str, user: &str, _: &Value) -> Result<(Value, u64, u64)> {
                if let Some(rest) = user.strip_prefix("Compress memories #") {
                    let id = rest.split_whitespace().next().unwrap().to_string();
                    return Ok((json!({ "text": format!("S{id}") }), 1, 1));
                }
                let mut first = self.0.lock().unwrap();
                if *first {
                    *first = false;
                    anyhow::bail!("claude -p returned no structured output; it said: \"I won't call StructuredOutput again\"");
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
        let base = std::env::temp_dir().join(format!("ravel-relive-balk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("soul");
        let mirror = repo.join(crate::sync::TRANSCRIPTS_SUBDIR).join("p");
        std::fs::create_dir_all(&mirror).unwrap();
        let rec = |k: usize| {
            json!({"type":"user","uuid":format!("u{k}"),"parentUuid":if k==0 {String::new()} else {format!("u{}",k-1)},
                   "timestamp":format!("2026-06-01T10:0{k}:00Z"),"sessionId":"s",
                   "message":{"role":"user","content":"z".repeat(9_000)}})
            .to_string()
        };
        std::fs::write(
            mirror.join("s1.jsonl"),
            (0..6).map(rec).collect::<Vec<_>>().join("\n"),
        )
        .unwrap();
        let log = MemoryLog::at(base.join("out"));
        let sp = Speakers {
            agent: "soul".into(),
            peers: vec![],
            human: "human".into(),
        };
        let llm = Balker(std::sync::Mutex::new(true));
        let convs = load(&repo, &sp, &PeerIndex::default()).unwrap();
        let r = relive(
            &repo,
            &sp,
            &llm,
            &log,
            &convs,
            "2026",
            "2027",
            100,
            0,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        assert!(r.stopped.is_none(), "{r:?}");
        assert_eq!(r.notes, 6, "{r:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_stretch_refused_twice_is_left_blank_logged_and_read_past() {
        let base = std::env::temp_dir().join(format!("ravel-relive-refuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("soul");
        let mirror = repo.join(crate::sync::TRANSCRIPTS_SUBDIR).join("p");
        std::fs::create_dir_all(&mirror).unwrap();
        // 12 turns clipped to ~4,000 characters: the first stretch carries the
        // refused turn, the ones after it do not.
        let rec = |k: usize| {
            let body = if k == 0 {
                format!("FORBIDDEN {}", "z".repeat(9_000))
            } else {
                "z".repeat(9_000)
            };
            json!({"type":"user","uuid":format!("u{k}"),"parentUuid":if k==0 {String::new()} else {format!("u{}",k-1)},
                   "timestamp":format!("2026-06-01T10:{k:02}:00Z"),"sessionId":"s",
                   "message":{"role":"user","content":body}})
            .to_string()
        };
        std::fs::write(
            mirror.join("s1.jsonl"),
            (0..12).map(rec).collect::<Vec<_>>().join("\n"),
        )
        .unwrap();
        let log = MemoryLog::at(base.join("out"));
        let sp = Speakers {
            agent: "soul".into(),
            peers: vec![],
            human: "human".into(),
        };
        let llm = Refuser(std::sync::Mutex::new(0));
        let convs = load(&repo, &sp, &PeerIndex::default()).unwrap();
        let r = relive(
            &repo,
            &sp,
            &llm,
            &log,
            &convs,
            "2026",
            "2027",
            100,
            0,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        assert!(r.stopped.is_none(), "{r:?}");
        assert_eq!(r.refused, 1, "{r:?}");
        assert_eq!(
            *llm.0.lock().unwrap(),
            2,
            "asked once more in a fresh session, then left blank"
        );
        assert_eq!(
            r.turns, 12,
            "every turn is marked read, the refused ones included"
        );
        assert!(r.notes > 0 && r.notes < 12, "{r:?}");
        let dropped = std::fs::read_to_string(base.join("out").join("dropped.tsv")).unwrap();
        assert!(
            dropped.contains("refused by the model's usage policy twice"),
            "{dropped}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_line_wrapped_in_json_is_refused_and_plain_lines_pass() {
        assert!(
            not_wrapped(r#"[{"text": "2026-07-11 Rescued docs", "turns": [5]}]"#.into()).is_err()
        );
        assert!(not_wrapped(r#"{"text": "x"}"#.into()).is_err());
        // RDF 1.2 annotation syntax and bracketed prose are not JSON
        assert!(not_wrapped("{| |} annotation syntax asserts the base triple".into()).is_ok());
        assert!(not_wrapped("[draft] the spec is unruled".into()).is_ok());
    }

    #[test]
    fn repair_lists_a_bad_merge_and_every_built_merge_above_it_smallest_first() {
        let dir = std::env::temp_dir().join(format!("ravel-repair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = MemoryLog::at(dir.clone());
        let sm = |b: &str, t: &str| Summary {
            block: b.into(),
            text: t.into(),
            model: "m".into(),
            written: "w".into(),
            wording: Some(memory_log::WORDING.into()),
        };
        for (b, t) in [
            ("0-1", "a"),
            ("2-3", "I haven't compressed memories #2-3."),
            ("4-5", "c"),
            ("6-7", "d"),
            ("0-3", "ab"),
            ("4-7", "cd"),
            ("0-7", "abcd"),
        ] {
            log.append_summary(&sm(b, t)).unwrap();
        }
        assert_eq!(to_repair(&log).unwrap(), vec![(2, 4), (0, 4), (0, 8)]);
        // the newer line wins: once replaced, nothing is left to repair
        for (b, t) in [("2-3", "b"), ("0-3", "ab"), ("0-7", "abcd")] {
            log.append_summary(&sm(b, t)).unwrap();
        }
        assert!(to_repair(&log).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merges_in_an_older_wording_are_rewritten_a_few_a_pass_smallest_first() {
        let dir = std::env::temp_dir().join(format!("ravel-reword-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = MemoryLog::at(dir.join("out"));
        let notes: Vec<Memory> = (0..4)
            .map(|k| Memory {
                id: format!("m{k}"),
                ts: format!("2026-01-0{}T00:00:00Z", k + 1),
                text: format!("note {k}"),
                turns: vec![format!("t{k}")],
                model: "m".into(),
            })
            .collect();
        log.append_chunk(&notes, &[]).unwrap();
        let sm = |b: &str, w: Option<&str>| Summary {
            block: b.into(),
            text: format!("old {b}"),
            model: "m".into(),
            written: "w".into(),
            wording: w.map(str::to_string),
        };
        // 2-3 is already current; 0-1 is not, so 0-1 and 0-3 above it are due.
        log.append_summary(&sm("0-1", None)).unwrap();
        log.append_summary(&sm("2-3", Some(memory_log::WORDING)))
            .unwrap();
        log.append_summary(&sm("0-3", None)).unwrap();
        assert_eq!(to_reword(&log).unwrap(), vec![(0, 2), (0, 4)]);
        let fake = Fake(std::sync::Mutex::new(1));
        let stop = std::sync::atomic::AtomicBool::new(false);
        let r = reword(&fake, &dir, "soul", &log, 1, &stop).unwrap();
        assert_eq!((r.merges, r.yielded), (1, true), "{r:?}");
        assert_eq!(to_reword(&log).unwrap(), vec![(0, 4)]);
        let r = reword(&fake, &dir, "soul", &log, 1, &stop).unwrap();
        assert_eq!(r.merges, 1);
        assert!(to_reword(&log).unwrap().is_empty());
        let sums = log.summaries().unwrap();
        assert_eq!(sums["0-3"].text, "S0-3", "rebuilt, and the newer line wins");
        assert!(memory_log::nap_prompt(&notes, &sums, (0, 4), 0)
            .unwrap()
            .ends_with(&format!("{}\n", memory_log::STORY)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_merge_that_talks_about_merging_or_wears_its_label_is_refused() {
        let w3 = "I haven't compressed memories #656-659. The compression budget was used up by the 660-661 request.";
        assert!(a_memory(w3.into(), Some("656-659")).is_err());
        assert!(a_memory(
            "#740-741 compressed: Flinch-test: liveness".into(),
            Some("740-741")
        )
        .is_err());
        assert!(a_memory(
            "Compressed #3730-3731: unread eQMARL quotes".into(),
            Some("3730-3731")
        )
        .is_err());
        // another block's or an issue's number inside the text is fine
        assert!(a_memory(
            "Phases (#52-55): worldview, ontology".into(),
            Some("2952-2953")
        )
        .is_ok());
        assert!(a_memory("#116 closed: openiris 9f51407".into(), None).is_ok());
        let lux = "Not compressing this one. #13028-13029 and #13030-13031 are already saved as two compressed lines";
        assert!(a_memory(lux.into(), Some("13028-13031")).is_err());
        assert!(a_memory(
            "No line written. #13040-13043 are already saved.".into(),
            Some("13040-13047")
        )
        .is_err());
        // a real memory about compressing code is fine
        assert!(a_memory(
            "2024-11-04: My unrun recompress one-liner blanked Rob's memories".into(),
            Some("826-827")
        )
        .is_ok());
    }
}
