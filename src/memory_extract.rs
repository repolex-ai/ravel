//! Writing the memory index: transcripts → one-line memories → summaries.
//!
//! Reads a soul's mirrors through the same adapters the graph uses (Claude
//! Code, Antigravity, claude.ai), keeps only AUTHORED prose (no tool output,
//! no thinking), takes each distinct turn once, and hands consecutive turns of
//! one conversation to a model in chunks. Then writes every window summary the
//! tree needs, bottom-up. All results go to the append-only logs in
//! `memory_log`; the graph is projected from those.
//!
//! **Money.** Every model call is priced and appended to
//! `~/.config/ravel/memory-spend.tsv` before the next one starts, and no call
//! is made once that ledger reaches `memory_budget_usd` from config.yml. With
//! no budget, or no key file, nothing here calls a model.

use crate::memory::{hour_of, Node, Window};
use crate::memory_log::{children_digest, node_text, tree_of, Memory, MemoryLog, Summary};
use crate::SourceKind;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The model that writes memories and summaries (goodlux, 2026-10-04: Haiku,
/// on the API credits).
pub const MODEL: &str = "claude-haiku-4-5";
/// US dollars per million tokens for [`MODEL`], input and output.
pub const PRICE_IN: f64 = 1.0;
pub const PRICE_OUT: f64 = 5.0;

/// Characters of prose per chunk, and the most any one turn contributes.
const CHUNK_CHARS: usize = 16_000;
const TURN_CHARS: usize = 4_000;
/// A turn younger than this is left for a later pass: its conversation may
/// still be going, and a chunk cut mid-thought makes worse memories.
const SETTLE_SECS: i64 = 30 * 60;
/// Parallel model calls.
const WORKERS: usize = 6;

// ───────────────────────────── the model ─────────────────────────────

/// One structured-output call. A trait so tests run without a network.
pub trait Llm: Sync {
    fn model(&self) -> &str;
    /// Returns the parsed JSON and the (input, output) token counts.
    fn json(
        &self,
        system: &str,
        user: &str,
        schema: &Value,
        max_tokens: u32,
    ) -> Result<(Value, u64, u64)>;
}

/// Claude over the Messages API, raw HTTP (ravel is Rust; there is no
/// official Rust SDK).
pub struct Anthropic {
    key: String,
    http: reqwest::blocking::Client,
}

pub fn key_path() -> PathBuf {
    crate::daemon::config::config_dir().join("anthropic-api-key")
}

impl Anthropic {
    /// The key comes from one file only ravel reads, never from the
    /// environment: an exported key leaks into every child process (it billed
    /// goodlux once, 2026-09-30).
    pub fn from_key_file() -> Result<Self> {
        let p = key_path();
        let key = std::fs::read_to_string(&p)
            .with_context(|| format!("no API key at {}", p.display()))?
            .trim()
            .to_string();
        if key.is_empty() {
            anyhow::bail!("{} is empty", p.display());
        }
        Ok(Anthropic {
            key,
            http: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()?,
        })
    }
}

impl Llm for Anthropic {
    fn model(&self) -> &str {
        MODEL
    }

    fn json(
        &self,
        system: &str,
        user: &str,
        schema: &Value,
        max_tokens: u32,
    ) -> Result<(Value, u64, u64)> {
        let body = json!({
            "model": MODEL,
            "max_tokens": max_tokens,
            "system": system,
            "messages": [{"role": "user", "content": user}],
            "output_config": {"format": {"type": "json_schema", "schema": schema}},
        });
        let mut wait = std::time::Duration::from_secs(5);
        for attempt in 0..6 {
            let r = self
                .http
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", &self.key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send();
            let retry = match &r {
                Err(_) => true,
                Ok(resp) => resp.status().as_u16() == 429 || resp.status().is_server_error(),
            };
            if retry && attempt < 5 {
                std::thread::sleep(wait);
                wait *= 2;
                continue;
            }
            let resp = r?;
            let status = resp.status();
            let v: Value = resp.json()?;
            if !status.is_success() {
                anyhow::bail!("Messages API {status}: {v}");
            }
            let input = v["usage"]["input_tokens"].as_u64().unwrap_or(0);
            let output = v["usage"]["output_tokens"].as_u64().unwrap_or(0);
            let stop = v["stop_reason"].as_str().unwrap_or("");
            if stop != "end_turn" {
                anyhow::bail!("model stopped with {stop:?}; output not used");
            }
            let text = v["content"]
                .as_array()
                .and_then(|c| c.iter().find(|b| b["type"] == "text"))
                .and_then(|b| b["text"].as_str())
                .ok_or_else(|| anyhow!("no text block in response"))?;
            return Ok((serde_json::from_str(text)?, input, output));
        }
        unreachable!()
    }
}

/// The spend ledger: one line per call, `rfc3339 \t soul \t in \t out \t usd`.
pub struct Ledger {
    path: PathBuf,
    budget: f64,
    spent: Mutex<f64>,
}

impl Ledger {
    pub fn open(budget: f64) -> Result<Self> {
        Self::open_at(
            crate::daemon::config::config_dir().join("memory-spend.tsv"),
            budget,
        )
    }

    pub fn open_at(path: PathBuf, budget: f64) -> Result<Self> {
        let spent = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.rsplit('\t').next()?.parse::<f64>().ok())
            .sum();
        Ok(Ledger {
            path,
            budget,
            spent: Mutex::new(spent),
        })
    }

    pub fn spent(&self) -> f64 {
        *self.spent.lock().unwrap()
    }

    fn check(&self) -> Result<()> {
        let s = self.spent();
        if s >= self.budget {
            anyhow::bail!(
                "memory budget reached: ${s:.2} of ${:.2} spent (memory_budget_usd in config.yml)",
                self.budget
            );
        }
        Ok(())
    }

    fn record(&self, soul: &str, input: u64, output: u64) -> Result<()> {
        use std::io::Write;
        let usd = input as f64 / 1e6 * PRICE_IN + output as f64 / 1e6 * PRICE_OUT;
        let mut spent = self.spent.lock().unwrap();
        *spent += usd;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(
            f,
            "{}\t{soul}\t{input}\t{output}\t{usd:.6}",
            chrono::Utc::now().to_rfc3339()
        )?;
        Ok(())
    }
}

// ───────────────────────────── sources ─────────────────────────────

/// One turn's prose, ready to read.
#[derive(Debug, Clone)]
pub struct ProseTurn {
    pub id: String,
    pub ts: String,
    pub role: String,
    pub text: String,
}

/// A conversation's turns in order, from one source.
#[derive(Debug)]
pub struct Conversation {
    pub source: &'static str,
    pub id: String,
    /// Where it happened, as far as the source says: a working directory, or
    /// a claude.ai chat's title. Tells the model which project it is reading.
    pub place: String,
    pub turns: Vec<ProseTurn>,
}

fn authored(e: &crate::Event) -> String {
    let Some(text) = &e.text else {
        return String::new();
    };
    if e.text_provenance.is_empty() {
        return text.clone();
    }
    e.text_provenance
        .iter()
        .filter(|s| s.kind == SourceKind::Authored)
        .filter_map(|s| text.get(s.start..s.end))
        .collect::<Vec<_>>()
        .join("\n")
}

fn to_turns(events: &[crate::Event]) -> Vec<ProseTurn> {
    events
        .iter()
        .filter_map(|e| {
            Some(ProseTurn {
                id: e.event_id.clone(),
                ts: e.timestamp.clone()?,
                role: e.role.clone(),
                text: authored(e),
            })
        })
        .collect()
}

/// Every conversation in a soul's mirrors.
pub fn conversations(repo: &Path) -> Result<Vec<Conversation>> {
    let mut out = Vec::new();
    let cc = repo.join(crate::sync::TRANSCRIPTS_SUBDIR);
    if cc.is_dir() {
        for p in crate::sync::walk_jsonl(&cc) {
            let Ok(s) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(ev) = crate::adapter::parse_transcript(&s) else {
                continue;
            };
            let id = p
                .strip_prefix(&cc)
                .unwrap_or(&p)
                .to_string_lossy()
                .into_owned();
            let place = first_cwd(&s)
                .map(|c| format!("working directory {c}"))
                .unwrap_or_default();
            out.push(Conversation {
                source: "claude-code",
                id,
                place,
                turns: to_turns(&ev),
            });
        }
    }
    let agy = repo.join(crate::sync::AGY_TRANSCRIPTS_SUBDIR);
    if agy.is_dir() {
        for e in std::fs::read_dir(&agy)?.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let id = p
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let Ok(s) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(parsed) = crate::agy::parse_transcript(&s, &id) else {
                continue;
            };
            out.push(Conversation {
                source: "agy",
                id,
                place: String::new(),
                turns: to_turns(&parsed.events),
            });
        }
    }
    let shelf = repo.join(crate::sync::CLAUDE_AI_SUBDIR);
    if shelf.is_dir() {
        let mut best: HashMap<String, (String, crate::claude_ai::Conversation)> = HashMap::new();
        let mut exports: Vec<PathBuf> = std::fs::read_dir(&shelf)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| crate::claude_ai::is_export_dir(p))
            .collect();
        exports.sort();
        for e in exports {
            for c in crate::claude_ai::read_conversations(&e)? {
                let k = crate::claude_ai::version_key(&c);
                if best.get(&c.uuid).is_none_or(|(bk, _)| k > *bk) {
                    best.insert(c.uuid.clone(), (k, c));
                }
            }
        }
        for (id, (_, c)) in best {
            let place = if c.name.is_empty() {
                "a Claude desktop chat".to_string()
            } else {
                format!("a Claude desktop chat titled {:?}", c.name)
            };
            out.push(Conversation {
                source: "claude.ai",
                id,
                place,
                turns: to_turns(&crate::claude_ai::conversation_events(&c)),
            });
        }
    }
    Ok(out)
}

/// A run of consecutive turns from one conversation, to be read in one call.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub source: &'static str,
    pub conversation: String,
    pub place: String,
    pub turns: Vec<ProseTurn>,
}

/// The first `"cwd":"…"` in a Claude Code transcript.
fn first_cwd(jsonl: &str) -> Option<String> {
    let i = jsonl.find("\"cwd\":\"")? + 7;
    let rest = &jsonl[i..];
    Some(rest[..rest.find('"')?].to_string())
}

/// Peer messages reach a session as user-role turns wrapped in
/// `<channel source=… from_cwd=… …>`. Read raw, they look like the human
/// speaking, and the model credits the reader with what a peer said
/// (w3bl0rd's accuracy check, 2026-10-05). Name the sender instead.
fn relabel_channels(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("<channel ") {
        out.push_str(&rest[..i]);
        // The tag ends at the first '>' outside a quoted attribute value.
        let mut quoted = false;
        let end = rest[i..].char_indices().find_map(|(k, ch)| match ch {
            '"' => {
                quoted = !quoted;
                None
            }
            '>' if !quoted => Some(k),
            _ => None,
        });
        let Some(end) = end else {
            out.push_str(&rest[i..]);
            return out;
        };
        let tag = &rest[i..i + end];
        let attr = |name: &str| {
            let k = format!("{name}=\"");
            tag.find(&k).and_then(|j| {
                let v = &tag[j + k.len()..];
                v.find('"').map(|e| v[..e].to_string())
            })
        };
        let who = attr("from_cwd")
            .and_then(|c| {
                c.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .map(str::to_string)
            })
            .or_else(|| attr("from_id"))
            .unwrap_or_else(|| "unnamed".into());
        out.push_str(&format!("[message from another agent, {who}:]"));
        rest = &rest[i + end + 1..];
    }
    out.push_str(rest);
    out.replace("</channel>", "")
}

fn clip(s: &str) -> String {
    if s.len() <= TURN_CHARS {
        return s.to_string();
    }
    let mut head = TURN_CHARS * 3 / 4;
    while !s.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = s.len() - TURN_CHARS / 4;
    while !s.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{} […] {}", &s[..head], &s[tail..])
}

/// Chunks of turns not yet read. Each distinct turn id appears once across
/// all chunks (transcript files repeat history; #17). Turns younger than
/// [`SETTLE_SECS`] are held back.
pub fn pending_chunks(
    convs: &[Conversation],
    done: &HashSet<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Chunk> {
    let mut claimed: HashSet<&str> = HashSet::new();
    let mut chunks = Vec::new();
    let mut ordered: Vec<&Conversation> = convs.iter().collect();
    ordered.sort_by(|a, b| {
        (a.turns.first().map(|t| t.ts.as_str()), &a.id)
            .cmp(&(b.turns.first().map(|t| t.ts.as_str()), &b.id))
    });
    for c in ordered {
        let mut cur: Vec<ProseTurn> = Vec::new();
        let mut chars = 0usize;
        for t in &c.turns {
            if done.contains(&t.id) || !claimed.insert(t.id.as_str()) {
                continue;
            }
            let settled = chrono::DateTime::parse_from_rfc3339(&t.ts)
                .map(|ts| (now - ts.with_timezone(&chrono::Utc)).num_seconds() >= SETTLE_SECS)
                .unwrap_or(false);
            if !settled {
                claimed.remove(t.id.as_str());
                break; // nothing after an unsettled turn is settled either
            }
            let mut t = t.clone();
            t.text = clip(t.text.trim());
            if chars + t.text.len() > CHUNK_CHARS && !cur.is_empty() {
                chunks.push(Chunk {
                    source: c.source,
                    conversation: c.id.clone(),
                    place: c.place.clone(),
                    turns: std::mem::take(&mut cur),
                });
                chars = 0;
            }
            chars += t.text.len();
            cur.push(t);
        }
        if !cur.is_empty() {
            chunks.push(Chunk {
                source: c.source,
                conversation: c.id.clone(),
                place: c.place.clone(),
                turns: cur,
            });
        }
    }
    chunks
}

// ───────────────────────────── prompts ─────────────────────────────

const EXTRACT_SYSTEM: &str = "You read a stretch of a conversation and write down what is worth remembering about the people and agents in it and their work — the way a colleague's notebook would, not an encyclopedia.

Who is who: 'agent' turns are the AI agent named in the header. 'human' turns are usually the human the agent works for, but a human turn that starts with [message from another agent, NAME:] is a message from that other agent, and a human turn whose text says who sent it ('w3bl0rd here', 'from spaceGOAT') is from that sender. Credit every action, finding and decision to whoever actually did it. Reading or being told something is not doing it.

Keep: what was done, built, changed, shipped or broken; decisions and who made them; results and measurements; problems hit and how they were solved; preferences, rules and corrections given; plans and open questions; people, projects and places.

Do not keep general knowledge an agent explained (what a library is, how a technique works, a list of options) unless someone acted on it — then keep the action and its outcome. Do not keep someone merely asking a question.

Each memory is one line, at most 200 characters, plain past tense. It names who acted and which project, repository or thing it concerns (use the working directory or chat title in the header when the text does not say). Copy names, paths, commands, versions and numbers exactly as written; if a detail is not in the text, leave it out — never fill a gap. Fold small steps toward one result into one memory. Most stretches yield zero to four memories; zero is a fine answer.

For each memory give the numbers of the turns it rests on.";

const SUMMARY_SYSTEM: &str = "You merge lines from a memory log, oldest first, into ONE line of at most 200 characters that keeps what matters most: the decisions, results and things made, who did them, and the projects they belong to, with their real names and numbers. Never credit one actor with what another did. Never add what the lines do not say. If the lines repeat each other, say it once.";

fn extract_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "memories": {
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
        "required": ["memories"],
        "additionalProperties": false
    })
}

fn summary_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"text": {"type": "string"}},
        "required": ["text"],
        "additionalProperties": false
    })
}

fn chunk_prompt(c: &Chunk, agent: &str) -> String {
    let place = if c.place.is_empty() {
        String::new()
    } else {
        format!(" It took place in {}.", c.place)
    };
    let mut s = format!(
        "The agent in this conversation is {agent}.{place} Source: {} conversation {}. Turns {} to {}.\n\n",
        c.source,
        c.conversation,
        c.turns.first().map(|t| t.ts.as_str()).unwrap_or("?"),
        c.turns.last().map(|t| t.ts.as_str()).unwrap_or("?")
    );
    for (i, t) in c.turns.iter().enumerate() {
        if t.text.is_empty() {
            continue;
        }
        let who = if t.role == "user" { "human" } else { "agent" };
        s += &format!(
            "[{}] {who} ({}):\n{}\n\n",
            i + 1,
            t.ts,
            relabel_channels(&t.text)
        );
    }
    s
}

fn memory_id(first_turn: &str, text: &str) -> String {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(format!("{first_turn}\u{0}{text}").as_bytes());
    format!("m{}", &format!("{h:x}")[..15])
}

/// Read one chunk into memories. A chunk with no prose costs nothing.
pub fn extract_chunk(llm: &dyn Llm, c: &Chunk, agent: &str) -> Result<(Vec<Memory>, u64, u64)> {
    if c.turns.iter().all(|t| t.text.is_empty()) {
        return Ok((Vec::new(), 0, 0));
    }
    let (v, i, o) = llm.json(
        EXTRACT_SYSTEM,
        &chunk_prompt(c, agent),
        &extract_schema(),
        4096,
    )?;
    let mut out = Vec::new();
    for m in v["memories"].as_array().cloned().unwrap_or_default() {
        let text = m["text"].as_str().unwrap_or("").trim().to_string();
        if text.is_empty() {
            continue;
        }
        let mut turns: Vec<&ProseTurn> = m["turns"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|n| n.as_u64())
            .filter_map(|n| c.turns.get((n as usize).checked_sub(1)?))
            .collect();
        turns.sort_by(|a, b| a.ts.cmp(&b.ts));
        turns.dedup_by(|a, b| a.id == b.id);
        // A memory that cites no real turn cannot be followed back; drop it
        // rather than pin it on a guess.
        let Some(first) = turns.first() else { continue };
        out.push(Memory {
            id: memory_id(&first.id, &text),
            ts: first.ts.clone(),
            text,
            turns: turns.iter().map(|t| t.id.clone()).collect(),
            model: llm.model().to_string(),
        });
    }
    Ok((out, i, o))
}

/// The API refusing because the ACCOUNT is out of allowance (a Console spend
/// limit, or credits), not because of this request. Every later call would
/// fail the same way, so the run stops instead of failing chunk after chunk.
/// Seen 2026-10-05: "You have reached your specified API usage limits. You
/// will regain access on 2026-11-01 at 00:00 UTC."
fn is_account_limit(e: &anyhow::Error) -> bool {
    let m = format!("{e:#}");
    m.contains("API usage limits") || m.contains("credit balance is too low")
}

fn account_limit_note(e: &anyhow::Error) -> String {
    let m = format!("{e:#}");
    let detail = m
        .split("\"message\":\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .unwrap_or("usage limit reached");
    format!("the Anthropic account refused: {detail}")
}

// ───────────────────────────── the run ─────────────────────────────

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct RunReport {
    pub chunks: usize,
    pub memories: usize,
    pub summaries: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub failed: Vec<String>,
    /// Set when the run stopped early (budget, or `max_calls`).
    pub stopped: Option<String>,
}

/// Bring one soul's memory index up to date: read pending turns, then write
/// every summary that is missing or stale. `max_calls` bounds one run so a
/// backfill proceeds in steps instead of one unbounded job.
pub fn run_soul(
    repo: &Path,
    soul: &str,
    llm: &dyn Llm,
    ledger: &Ledger,
    max_calls: usize,
) -> Result<RunReport> {
    let log = MemoryLog::for_soul(repo);
    // The soul's name, as its repo directory spells it: who "agent" is.
    let agent = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| soul.to_string());
    let report = Mutex::new(RunReport::default());
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let take_call = || -> Result<()> {
        ledger.check()?;
        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= max_calls {
            anyhow::bail!("call limit for this run ({max_calls}) reached");
        }
        Ok(())
    };

    // 1. Memories.
    let done = log.extracted()?;
    let chunks = pending_chunks(&conversations(repo)?, &done, chrono::Utc::now());
    let queue = Mutex::new(chunks.into_iter());
    let write = Mutex::new(());
    std::thread::scope(|s| {
        for _ in 0..WORKERS {
            s.spawn(|| loop {
                let Some(c) = queue.lock().unwrap().next() else {
                    break;
                };
                let needs_call = c.turns.iter().any(|t| !t.text.is_empty());
                if needs_call {
                    if let Err(e) = take_call() {
                        report.lock().unwrap().stopped.get_or_insert(e.to_string());
                        break;
                    }
                }
                match extract_chunk(llm, &c, &agent) {
                    Ok((ms, i, o)) => {
                        let _g = write.lock().unwrap();
                        let ids: Vec<String> = c.turns.iter().map(|t| t.id.clone()).collect();
                        let rec = (|| -> Result<()> {
                            if i + o > 0 {
                                ledger.record(soul, i, o)?;
                            }
                            log.append_chunk(&ms, &ids)
                        })();
                        let mut r = report.lock().unwrap();
                        match rec {
                            Ok(()) => {
                                r.chunks += 1;
                                r.memories += ms.len();
                                r.input_tokens += i;
                                r.output_tokens += o;
                            }
                            Err(e) => r.failed.push(format!("{}: {e:#}", c.conversation)),
                        }
                    }
                    Err(e) => {
                        let mut r = report.lock().unwrap();
                        if is_account_limit(&e) {
                            r.stopped.get_or_insert(account_limit_note(&e));
                            break;
                        }
                        r.failed
                            .push(format!("{} {}: {e:#}", c.source, c.conversation));
                    }
                }
            });
        }
    });
    if report.lock().unwrap().stopped.is_some() {
        return Ok(report.into_inner().unwrap());
    }

    // 2. Summaries, one level at a time so every child is current before its
    //    parent is written.
    let memories = log.memories()?;
    let now_hour = hour_of(&chrono::Utc::now().to_rfc3339()).unwrap_or(i64::MAX);
    let (tree, by_id) = tree_of(&memories);
    let all = tree.summary_windows(now_hour);
    let mut sums = log.summaries()?;
    for level in 0..=crate::memory::MAX_LEVEL {
        let todo: Vec<(Window, String, String)> = all
            .iter()
            .filter(|w| w.level == level)
            .filter_map(|&w| {
                let d = children_digest(&tree, w, &by_id, &sums)?;
                if sums.get(&w.id()).is_some_and(|s| s.digest == d) {
                    return None;
                }
                let lines: Vec<String> = tree
                    .children(w)
                    .iter()
                    .filter_map(|c: &Node| node_text(c, &by_id, &sums))
                    .collect();
                Some((w, d, lines.join("\n")))
            })
            .collect();
        let queue = Mutex::new(todo.into_iter());
        let written = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..WORKERS {
                s.spawn(|| loop {
                    let Some((w, d, text)) = queue.lock().unwrap().next() else {
                        break;
                    };
                    if let Err(e) = take_call() {
                        report.lock().unwrap().stopped.get_or_insert(e.to_string());
                        break;
                    }
                    match llm.json(SUMMARY_SYSTEM, &text, &summary_schema(), 2048) {
                        Ok((v, i, o)) => {
                            let sm = Summary {
                                window: w.id(),
                                digest: d,
                                text: v["text"].as_str().unwrap_or("").trim().to_string(),
                                model: llm.model().to_string(),
                                written: chrono::Utc::now().to_rfc3339(),
                            };
                            let _g = write.lock().unwrap();
                            let rec = ledger
                                .record(soul, i, o)
                                .and_then(|_| log.append_summary(&sm));
                            let mut r = report.lock().unwrap();
                            match rec {
                                Ok(()) => {
                                    r.summaries += 1;
                                    r.input_tokens += i;
                                    r.output_tokens += o;
                                    written.lock().unwrap().push(sm);
                                }
                                Err(e) => r.failed.push(format!("{}: {e:#}", w.id())),
                            }
                        }
                        Err(e) => {
                            let mut r = report.lock().unwrap();
                            if is_account_limit(&e) {
                                r.stopped.get_or_insert(account_limit_note(&e));
                                break;
                            }
                            r.failed.push(format!("{}: {e:#}", w.id()));
                        }
                    }
                });
            }
        });
        for sm in written.into_inner().unwrap() {
            sums.insert(sm.window.clone(), sm);
        }
        if report.lock().unwrap().stopped.is_some() {
            break;
        }
    }
    Ok(report.into_inner().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes one memory per chunk citing its first turn, and summaries that
    /// join their lines with " + ".
    struct Fake;
    impl Llm for Fake {
        fn model(&self) -> &str {
            "fake"
        }
        fn json(&self, system: &str, user: &str, _s: &Value, _m: u32) -> Result<(Value, u64, u64)> {
            if system == EXTRACT_SYSTEM {
                let first = user.lines().find(|l| l.starts_with("[1]")).unwrap_or("");
                Ok((
                    json!({"memories": [{"text": format!("saw {first}"), "turns": [1]}, {"text": "cites nothing real", "turns": [99]}]}),
                    100,
                    10,
                ))
            } else {
                Ok((
                    json!({"text": user.lines().collect::<Vec<_>>().join(" + ")}),
                    50,
                    5,
                ))
            }
        }
    }

    fn turn(id: &str, ts: &str, text: &str) -> ProseTurn {
        ProseTurn {
            id: id.into(),
            ts: ts.into(),
            role: "user".into(),
            text: text.into(),
        }
    }

    #[test]
    fn a_turn_repeated_across_files_is_read_once_and_unsettled_turns_wait() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let a = Conversation {
            source: "claude-code",
            id: "a".into(),
            place: String::new(),
            turns: vec![
                turn("t1", "2026-01-01T10:00:00Z", "x"),
                turn("t2", "2026-01-01T10:01:00Z", "y"),
            ],
        };
        // a resumed session repeats t1 and t2, then goes on; t4 is too fresh
        let b = Conversation {
            source: "claude-code",
            id: "b".into(),
            place: String::new(),
            turns: vec![
                turn("t1", "2026-01-01T10:00:00Z", "x"),
                turn("t2", "2026-01-01T10:01:00Z", "y"),
                turn("t3", "2026-01-01T11:00:00Z", "z"),
                turn("t4", "2026-01-01T11:50:00Z", "w"),
            ],
        };
        let chunks = pending_chunks(&[a, b], &HashSet::new(), now);
        let ids: Vec<&str> = chunks
            .iter()
            .flat_map(|c| c.turns.iter().map(|t| t.id.as_str()))
            .collect();
        assert_eq!(ids, vec!["t1", "t2", "t3"]);
    }

    #[test]
    fn a_long_turn_is_clipped_on_a_character_boundary() {
        let s = "é".repeat(5000);
        let c = clip(&s);
        assert!(c.len() < s.len() && c.contains("[…]"));
    }

    #[test]
    fn a_run_writes_memories_then_summaries_and_charges_the_ledger() {
        let base = std::env::temp_dir().join("ravel-memory-run-test");
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("soul");
        let shelf = repo.join(crate::sync::CLAUDE_AI_SUBDIR).join("export-1");
        crate::claude_ai::tests::write_fixture(&shelf, true);
        let ledger = Ledger::open_at(base.join("spend.tsv"), 10.0).unwrap();
        let r = run_soul(&repo, "test", &Fake, &ledger, 100).unwrap();
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(r.chunks, 2, "two conversations with prose");
        assert_eq!(r.memories, 2, "the memory citing turn 99 is dropped");
        assert!(ledger.spent() > 0.0);
        // A second run finds nothing to do and spends nothing.
        let before = ledger.spent();
        let r2 = run_soul(&repo, "test", &Fake, &ledger, 100).unwrap();
        assert_eq!((r2.chunks, r2.memories, r2.summaries), (0, 0, 0));
        assert_eq!(ledger.spent(), before);
        // The log reads back.
        let ms = MemoryLog::for_soul(&repo).memories().unwrap();
        assert_eq!(ms.len(), 2);
        assert!(ms.iter().all(|m| !m.turns.is_empty()));
    }

    #[test]
    fn no_call_is_made_past_the_budget() {
        let base = std::env::temp_dir().join("ravel-memory-budget-test");
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("soul");
        crate::claude_ai::tests::write_fixture(
            &repo.join(crate::sync::CLAUDE_AI_SUBDIR).join("e"),
            true,
        );
        let ledger = Ledger::open_at(base.join("spend.tsv"), 0.0).unwrap();
        let r = run_soul(&repo, "test", &Fake, &ledger, 100).unwrap();
        assert!(r.stopped.as_deref().unwrap_or("").contains("budget"));
        assert_eq!((r.chunks, r.memories), (0, 0));
        assert!(!base.join("spend.tsv").exists(), "nothing was charged");
    }

    struct Refused;
    impl Llm for Refused {
        fn model(&self) -> &str {
            "refused"
        }
        fn json(&self, _: &str, _: &str, _: &Value, _: u32) -> Result<(Value, u64, u64)> {
            anyhow::bail!("Messages API 400 Bad Request: {{\"error\":{{\"message\":\"You have reached your specified API usage limits. You will regain access on 2026-11-01 at 00:00 UTC.\",\"type\":\"invalid_request_error\"}}}}")
        }
    }

    #[test]
    fn an_account_refusal_stops_the_run_instead_of_failing_every_chunk() {
        let base = std::env::temp_dir().join("ravel-memory-refused-test");
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("soul");
        crate::claude_ai::tests::write_fixture(
            &repo.join(crate::sync::CLAUDE_AI_SUBDIR).join("e"),
            true,
        );
        let ledger = Ledger::open_at(base.join("spend.tsv"), 10.0).unwrap();
        let r = run_soul(&repo, "test", &Refused, &ledger, 100).unwrap();
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        let stop = r.stopped.unwrap_or_default();
        assert!(
            stop.starts_with(
                "the Anthropic account refused: You have reached your specified API usage limits"
            ),
            "{stop}"
        );
        // Nothing read is recorded as read: the turns wait for the next pass.
        assert!(MemoryLog::for_soul(&repo).extracted().unwrap().is_empty());
    }

    #[test]
    fn a_peer_message_is_labelled_with_its_sender() {
        let t = "<channel source=\"plugin:subtext:subtext\" from_id=\"6l8p\" from_summary=\"Day 26 > stuff\" from_cwd=\"/Users/rob/repos/7R1PL3F0RC3/spaceGOAT\" sent_at=\"x\">\nRead the calibration section.\n</channel>";
        let r = relabel_channels(t);
        assert!(
            r.starts_with("[message from another agent, spaceGOAT:]"),
            "{r}"
        );
        assert!(r.contains("Read the calibration section."));
        assert!(!r.contains("<channel") && !r.contains("</channel>"));
        assert_eq!(relabel_channels("plain text"), "plain text");
    }

    #[test]
    fn the_header_names_the_agent_and_the_place() {
        let c = Chunk {
            source: "claude-code",
            conversation: "x".into(),
            place: "working directory /repos/W3BL0RD".into(),
            turns: vec![turn("t1", "2026-01-01T00:00:00Z", "hi")],
        };
        let p = chunk_prompt(&c, "W3BL0RD");
        assert!(p.starts_with("The agent in this conversation is W3BL0RD. It took place in working directory /repos/W3BL0RD."), "{p}");
        assert_eq!(
            first_cwd(r#"{"x":1,"cwd":"/a/b","y":2}"#).as_deref(),
            Some("/a/b")
        );
    }
}
