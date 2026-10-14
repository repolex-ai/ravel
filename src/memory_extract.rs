//! What the memory reader reads, and the model it asks.
//!
//! - The reader: a soul's mirrors through the same adapters the graph uses
//!   (Claude Code, Antigravity, claude.ai), AUTHORED prose only (no tool
//!   output, no thinking), each turn labelled with who spoke it.
//! - The model: Claude Code on the subscription ([`ClaudeCode`]), or the API
//!   with a key file ([`Anthropic`]), behind one [`Llm`] trait.
//!
//! **Money.** Every API call is priced and appended to a ledger before the
//! next one starts, and no call is made once it reaches its budget.
//! Subscription calls are counted in the run's own `usage.tsv`.

use crate::SourceKind;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The model that writes memories and summaries (goodlux, 2026-10-04: Haiku,
/// on the API credits).
pub const MODEL: &str = "claude-haiku-4-5";
/// US dollars per million tokens for [`MODEL`], input and output.
pub const PRICE_IN: f64 = 1.0;
pub const PRICE_OUT: f64 = 5.0;

/// The most of one turn a reader is shown: longer turns keep head and tail.
const TURN_CHARS: usize = 4_000;

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

    /// For a model that keeps a session across calls: the tokens its
    /// context held at the last call. 0 for one-shot calls.
    fn context_tokens(&self) -> u64 {
        0
    }

    /// Start the next call in a fresh session. A no-op for one-shot calls.
    fn reset(&self) {}
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

/// Claude through Claude Code's print mode, on the user's subscription: no
/// API key, nothing billed per call (goodlux, 2026-10-06: the memory index
/// must run on a subscription; the API key is only for spending credits).
///
/// Each call is one `claude -p` with no tools, no MCP servers, no settings
/// (so no hooks), no skills and no saved session — a saved session would be
/// a transcript ravel reads back in. `ANTHROPIC_API_KEY` is removed from the
/// child's environment so it can never fall through to API billing, and it
/// runs in an empty directory so no project's CLAUDE.md is picked up.
pub struct ClaudeCode {
    model: String,
    /// Claude Code turns extended thinking on by default: sharper notes, but
    /// about a minute and thousands of tokens per call (measured 2026-10-06).
    think: bool,
    dir: PathBuf,
    /// Session mode: every call continues one Claude Code session, the way
    /// the soul lived it, until [`Llm::reset`]. `None`: one-shot calls.
    session: Option<std::sync::Mutex<Sess>>,
}

#[derive(Default)]
struct Sess {
    id: Option<String>,
    context: u64,
}

impl ClaudeCode {
    pub fn new(model: &str, think: bool) -> Result<Self> {
        let dir = std::env::temp_dir().join("ravel-claude-code");
        std::fs::create_dir_all(&dir)?;
        Ok(ClaudeCode {
            model: model.to_string(),
            think,
            dir,
            session: None,
        })
    }

    /// A reader that keeps one session across calls.
    pub fn in_session(mut self) -> Self {
        self.session = Some(std::sync::Mutex::new(Sess::default()));
        self
    }
}

/// A fresh v4-shaped UUID, from the clock and a counter (no uuid crate).
fn new_uuid() -> String {
    use sha2::{Digest, Sha256};
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let h = format!("{:x}", Sha256::digest(format!("{t}-{n}-{}", std::process::id())));
    format!(
        "{}-{}-4{}-a{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32]
    )
}

impl Llm for ClaudeCode {
    fn model(&self) -> &str {
        &self.model
    }

    fn json(
        &self,
        system: &str,
        user: &str,
        schema: &Value,
        _max_tokens: u32,
    ) -> Result<(Value, u64, u64)> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        // Session mode: the first call opens a session, later ones resume it.
        let session_arg = self.session.as_ref().map(|m| {
            let mut s = m.lock().unwrap();
            match &s.id {
                Some(id) => ("--resume", id.clone()),
                None => {
                    let id = new_uuid();
                    s.id = Some(id.clone());
                    ("--session-id", id)
                }
            }
        });
        let mut cmd = Command::new("claude");
        if !self.think {
            cmd.env("MAX_THINKING_TOKENS", "0");
        }
        let mut child = cmd
            .current_dir(&self.dir)
            .env_remove("ANTHROPIC_API_KEY")
            .args(["-p", "--model", &self.model, "--system-prompt", system])
            .args([
                "--tools",
                "",
                "--strict-mcp-config",
                "--setting-sources",
                "",
            ])
            .arg("--disable-slash-commands")
            .args(match &session_arg {
                Some((flag, id)) => vec![flag.to_string(), id.clone()],
                None => vec!["--no-session-persistence".to_string()],
            })
            .args([
                "--output-format",
                "json",
                "--json-schema",
                &schema.to_string(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("run claude (Claude Code) — is it installed and logged in?")?;
        child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("no stdin for claude"))?
            .write_all(user.as_bytes())?;
        let out = child.wait_with_output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let v: Value = serde_json::from_str(&stdout).with_context(|| {
            format!(
                "claude -p printed no JSON (exit {}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
                    .chars()
                    .take(500)
                    .collect::<String>()
            )
        })?;
        let r = match &v {
            Value::Array(a) => a.last().cloned().unwrap_or_default(),
            other => other.clone(),
        };
        if r["is_error"].as_bool().unwrap_or(true) {
            anyhow::bail!(
                "claude -p failed: {}",
                r["result"]
                    .as_str()
                    .unwrap_or(&r.to_string())
                    .chars()
                    .take(500)
                    .collect::<String>()
            );
        }
        // The session's opening record says which credential it used.
        let key_source = match &v {
            Value::Array(a) => a
                .iter()
                .find(|m| m["subtype"] == "init")
                .and_then(|m| m["apiKeySource"].as_str())
                .unwrap_or("")
                .to_string(),
            _ => String::new(),
        };
        if key_source != "none" {
            anyhow::bail!("claude -p used an API key, not the subscription; stopping");
        }
        let so = r
            .get("structured_output")
            .cloned()
            .ok_or_else(|| anyhow!("claude -p returned no structured output"))?;
        let u = &r["usage"];
        let input = u["input_tokens"].as_u64().unwrap_or(0)
            + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
            + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
        let output = u["output_tokens"].as_u64().unwrap_or(0);
        if let Some(m) = &self.session {
            m.lock().unwrap().context = input + output;
        }
        Ok((so, input, output))
    }

    fn context_tokens(&self) -> u64 {
        self.session.as_ref().map_or(0, |m| m.lock().unwrap().context)
    }

    /// Close the session: the next call opens a new one. The closed
    /// session's file is deleted — it is a reading aid, not a transcript of
    /// anyone's life, and must never be mirrored back in.
    fn reset(&self) {
        let Some(m) = &self.session else { return };
        let mut s = m.lock().unwrap();
        if let Some(id) = s.id.take() {
            if let Some(home) = std::env::var_os("HOME") {
                let projects = PathBuf::from(home).join(".claude/projects");
                for e in std::fs::read_dir(&projects).into_iter().flatten().flatten() {
                    let f = e.path().join(format!("{id}.jsonl"));
                    if f.is_file() && e.file_name().to_string_lossy().contains("ravel-claude-code") {
                        let _ = std::fs::remove_file(f);
                    }
                }
            }
        }
        s.context = 0;
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

    pub(crate) fn check(&self) -> Result<()> {
        let s = self.spent();
        if s >= self.budget {
            anyhow::bail!(
                "memory budget reached: ${s:.2} of ${:.2} spent (memory_budget_usd in config.yml)",
                self.budget
            );
        }
        Ok(())
    }

    pub(crate) fn record(&self, soul: &str, input: u64, output: u64) -> Result<()> {
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
    /// "user" (the human's side of the session) or "assistant" (the soul),
    /// whatever the harness calls them.
    pub role: String,
    pub text: String,
    /// The peer soul that sent this user turn, when the sender's own
    /// transcript shows the send (see `peers`).
    pub peer: Option<crate::peers::Sent>,
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

/// Claude Code's compaction summary arrives as a user turn that opens with
/// this sentence. It is a model's summary of earlier turns that are already
/// read on their own: a copy of a copy, never a source (4m41th34's check,
/// 2026-10-05).
const COMPACTION_OPENING: &str = "This session is being continued from a previous conversation";

/// Harness records that arrive as user turns but are no one's words: slash
/// commands and their output, background-task and system notices, skill
/// bodies, fork and workflow prompts, interruption markers. Matched on how
/// the turn starts (counted over W4R3Z, spaceGOAT and lUX, 2026-10-13).
const HARNESS_OPENINGS: &[&str] = &[
    "<task-notification",
    "<command-name",
    "<command-message",
    "<command-args",
    "<local-command-stdout",
    "<local-command-stderr",
    "<local-command-caveat",
    "Caveat: The messages below were generated by the user while running local commands",
    "<system-reminder",
    "<bash-stdout",
    "<bash-stderr",
    "<fork-boilerplate",
    "[Workflow harness",
    "[SYSTEM NOTIFICATION",
    "[Request interrupted by user",
    "Base directory for this skill",
    "[earlier conversation truncated",
    "[Usage limit approaching",
];

/// A user turn's own words, or empty when it is harness noise. `<bash-input>`
/// is the human running a shell command: kept, said plainly. A slash command
/// alone on its line (`/compact`) is the harness's.
fn human_words(t: &str) -> String {
    let t = strip_reminders(t);
    let s = t.trim_start();
    if HARNESS_OPENINGS.iter().any(|o| s.starts_with(o)) {
        return String::new();
    }
    if let Some(cmd) = s
        .strip_prefix("<bash-input>")
        .and_then(|r| r.split("</bash-input>").next())
    {
        return format!("(ran in the shell) {}", cmd.trim());
    }
    if s.starts_with('/') && !s.contains(char::is_whitespace) {
        return String::new();
    }
    t
}

/// Text with every `<system-reminder>…</system-reminder>` block removed:
/// the harness's notes riding inside a human turn.
fn strip_reminders(t: &str) -> String {
    let mut out = String::new();
    let mut rest = t;
    while let Some(i) = rest.find("<system-reminder>") {
        out += &rest[..i];
        match rest[i..].find("</system-reminder>") {
            Some(j) => rest = &rest[i + j + "</system-reminder>".len()..],
            None => {
                rest = "";
            }
        }
    }
    out += rest;
    out
}

/// An Antigravity USER_INPUT's own words: the `<USER_REQUEST>` body. The
/// `<ADDITIONAL_METADATA>` (local time) and `<USER_SETTINGS_CHANGE>` (model
/// switch) blocks around it are the harness's.
fn agy_request(t: &str) -> String {
    match t.split_once("<USER_REQUEST>") {
        Some((_, rest)) => rest
            .split("</USER_REQUEST>")
            .next()
            .unwrap_or("")
            .trim()
            .to_string(),
        None => t.trim().to_string(),
    }
}

fn to_turns(events: &[crate::Event], source: &str) -> Vec<ProseTurn> {
    events
        .iter()
        .filter_map(|e| {
            // One vocabulary for both harnesses. Antigravity says
            // USER_EXPLICIT / MODEL / SYSTEM; its SYSTEM records are task
            // results, scheduled-task firings and compaction checkpoints,
            // never anyone's words (4RX, 2026-10-13: read as the soul's own
            // turns, they made its Gemini session look like a stranger).
            let role = match (source, e.role.as_str()) {
                ("agy", "USER_EXPLICIT") => "user",
                ("agy", "MODEL") => "assistant",
                ("agy", _) => return None,
                (_, r) => r,
            };
            let mut t = authored(e);
            if t.trim_start().starts_with(COMPACTION_OPENING) {
                t.clear();
            }
            if role == "user" {
                t = if source == "agy" { agy_request(&t) } else { human_words(&t) };
            }
            Some(ProseTurn {
                id: e.event_id.clone(),
                ts: e.timestamp.clone()?,
                role: role.to_string(),
                text: t,
                peer: None,
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
            // A subagent's or workflow's transcript is not the soul's own
            // life. OptMem: a subagent must never write memory — its notes
            // would arrive duplicated and wrong.
            if p.components().any(|c| c.as_os_str() == "subagents") {
                continue;
            }
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
                turns: to_turns(&ev, "claude-code"),
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
                turns: to_turns(&parsed.events, "agy"),
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
                turns: to_turns(&crate::claude_ai::conversation_events(&c), "claude.ai"),
            });
        }
    }
    Ok(out)
}

/// Who can speak in a soul's conversation besides the human: the soul
/// itself, and its peers (every other soul raveld serves).
pub struct Speakers {
    pub agent: String,
    pub peers: Vec<String>,
    /// The human, by name ("Rob"), or "human" when the config names none.
    pub human: String,
}

fn name_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

impl Speakers {
    /// A human turn, with what is not the human's own words marked: peer
    /// messages that arrived through a channel wrapper, pastes the human
    /// relayed, and peer messages typed straight into the session (herdr
    /// delivers `agent prompt` as bare input, so only the sender's sign-on
    /// line says who it is — w3bl0rd, 2026-10-05).
    pub(crate) fn relabel_human(&self, text: &str) -> String {
        let t = relabel_channels(text);
        let t = relabel_pastes(&t);
        let t = mark_quote_backs(&t);
        if t.starts_with("[message from another agent") {
            return t;
        }
        match self.typed_sender(&t) {
            Some(name) => format!("[message from another agent, {name}:]\n{t}"),
            None => t,
        }
    }

    /// The sender named on the first line by the squad's sign-on habits —
    /// "A — B." (to A, from B), "A → B:" (from A, to B), "X here" — when it is
    /// a known peer. Leaky by nature, so it fires only on a peer's name.
    fn typed_sender(&self, text: &str) -> Option<String> {
        let line: String = text.lines().next()?.chars().take(200).collect();
        let me = name_key(&self.agent);
        let known = |cand: &str| -> Option<String> {
            let k = name_key(cand);
            if k.is_empty() || k == me {
                return None;
            }
            self.peers.iter().find(|p| name_key(p) == k).cloned()
        };
        let words: Vec<&str> = line.split_whitespace().collect();
        // "X here", "X here (Y)"
        for (i, w) in words.iter().enumerate() {
            if name_key(w) == "here" && i > 0 {
                if let Some(n) = known(words[i - 1]) {
                    return Some(n);
                }
                if let Some(n) = words.get(i + 1).and_then(|p| known(p)) {
                    return Some(n);
                }
            }
        }
        // "A → B:" — the sender is A
        for arrow in ["→", "->"] {
            if let Some((a, _)) = line.split_once(arrow) {
                if let Some(n) = a.split_whitespace().last().and_then(known) {
                    return Some(n);
                }
            }
        }
        // "A — B." — the sender is B
        for dash in [" — ", " – "] {
            if let Some((_, b)) = line.split_once(dash) {
                if let Some(n) = b.split_whitespace().next().and_then(known) {
                    return Some(n);
                }
            }
        }
        None
    }
}

/// The human's habit of quoting earlier words and answering on the same line
/// (`"…" <- my comment`) made the extractor credit the quoted words, usually
/// the agent's, to the human (w4r3z's check, 2026-10-05). Mark those lines.
fn mark_quote_backs(text: &str) -> String {
    text.lines()
        .map(|l| {
            let t = l.trim_start();
            let quoted = t.starts_with('"') || t.starts_with('\u{201c}');
            let reply = t.contains("\" <-") || t.contains("\u{201d} <-") || t.contains("\"<-");
            if quoted && reply {
                format!("[the human quoting earlier words, then replying after <-:] {t}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A paste is the human relaying someone else's words, not speaking.
fn relabel_pastes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let open = rest.find("<pasted_content");
        let close = rest.find("</pasted_content");
        let next = match (open, close) {
            (Some(o), Some(c)) => Some((o.min(c), o < c)),
            (Some(o), None) => Some((o, true)),
            (None, Some(c)) => Some((c, false)),
            (None, None) => None,
        };
        let Some((i, opening)) = next else { break };
        out.push_str(&rest[..i]);
        let end = rest[i..].find('>').map(|e| i + e + 1).unwrap_or(rest.len());
        out.push_str(if opening {
            "[pasted by the human — someone else's words, not the human's own:]"
        } else {
            "[end of paste]"
        });
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
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
pub(crate) fn relabel_channels(text: &str) -> String {
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

pub(crate) fn clip(s: &str) -> String {
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

pub(crate) fn summary_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"text": {"type": "string"}},
        "required": ["text"],
        "additionalProperties": false
    })
}

/// The API refusing because the ACCOUNT is out of allowance (a Console spend
/// limit, or credits), not because of this request. Every later call would
/// fail the same way, so the run stops instead of failing chunk after chunk.
/// Seen 2026-10-05: "You have reached your specified API usage limits. You
/// will regain access on 2026-11-01 at 00:00 UTC."
pub(crate) fn is_account_limit(e: &anyhow::Error) -> bool {
    let m = format!("{e:#}");
    m.contains("API usage limits") || m.contains("credit balance is too low")
}

pub(crate) fn account_limit_note(e: &anyhow::Error) -> String {
    let m = format!("{e:#}");
    let detail = m
        .split("\"message\":\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .unwrap_or("usage limit reached");
    format!("the Anthropic account refused: {detail}")
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

    #[test]
    fn a_long_turn_is_clipped_on_a_character_boundary() {
        let s = "é".repeat(5000);
        let c = clip(&s);
        assert!(c.len() < s.len() && c.contains("[…]"));
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

    fn squad() -> Speakers {
        Speakers {
            agent: "W3BL0RD".into(),
            peers: ["spaceGOAT", "W4R3Z", "lUX", "kira", "TR1P.L3X"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            human: "Rob".into(),
        }
    }

    #[test]
    fn a_typed_peer_message_is_recognised_by_its_sign_on() {
        let w = squad();
        // to w3bl0rd, from w4r3z
        assert_eq!(
            w.typed_sender("w3bl0rd — w4r3z. The tab names are wrong."),
            Some("W4R3Z".into())
        );
        // from spacegoat, to w3bl0rd
        assert_eq!(
            w.typed_sender("spaceGOAT → w3bl0rd: thanks"),
            Some("spaceGOAT".into())
        );
        assert_eq!(
            w.typed_sender("Hi W3BL0RD, kira here. Parse status:"),
            Some("kira".into())
        );
        assert_eq!(
            w.typed_sender("Hi spaceGOAT, Selkie here (lUX). Rob asked"),
            Some("lUX".into())
        );
        // the human, and names that are not peers, stay the human's
        assert_eq!(w.typed_sender("ok, start"), None);
        assert_eq!(w.typed_sender("I'm here — let's go"), None);
        assert_eq!(
            w.typed_sender("w3bl0rd here"),
            None,
            "the agent's own name is not a peer"
        );
        assert!(w
            .relabel_human("w3bl0rd — w4r3z. hi")
            .starts_with("[message from another agent, W4R3Z:]\n"));
    }

    #[test]
    fn a_paste_is_marked_as_someone_elses_words() {
        let w = squad();
        let t = "look at this\n\n<pasted_content id=\"5463\">\nw3bl0rd → spaceGOAT: done\n</pasted_content id=\"5463\">\nthoughts?";
        let r = w.relabel_human(t);
        assert!(r.contains("[pasted by the human — someone else's words, not the human's own:]\nw3bl0rd → spaceGOAT: done\n[end of paste]"), "{r}");
        assert!(
            !r.contains("<pasted_content") && !r.starts_with("[message from"),
            "the human typed the first line"
        );
    }

    #[test]
    fn a_quote_back_is_marked_as_not_the_humans_words() {
        let w = squad();
        let r = w.relabel_human(
            "\"paid API providers configurable\" <- this will just be a passthrough from iris",
        );
        assert!(
            r.starts_with("[the human quoting earlier words, then replying after <-:] \"paid"),
            "{r}"
        );
        assert_eq!(
            w.relabel_human("plain words <- no quote"),
            "plain words <- no quote"
        );
    }

    #[test]
    fn a_compaction_summary_is_never_a_source() {
        let e = crate::Event {
            event_id: "c1".into(),
            parent_id: None,
            role: "user".into(),
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            text: Some("This session is being continued from a previous conversation that ran out of context. Summary: ...".into()),
            text_provenance: vec![],
            thinking: None,
        };
        let t = to_turns(&[e], "claude-code");
        assert_eq!(t.len(), 1, "still marked read");
        assert!(t[0].text.is_empty(), "but never given to the model");
    }
}

#[cfg(test)]
mod reader_tests {
    use super::*;

    #[test]
    fn harness_records_are_no_ones_words() {
        for t in [
            "<task-notification><task-id>x</task-id></task-notification>",
            "<command-name>/compact</command-name>",
            "<local-command-stdout>done</local-command-stdout>",
            "Caveat: The messages below were generated by the user while running local commands. DO NOT respond",
            "[Request interrupted by user for tool use]",
            "Base directory for this skill: /x",
            "<fork-boilerplate>you are a fork</fork-boilerplate>",
            "/compact",
            "<system-reminder>only a reminder</system-reminder>",
        ] {
            assert_eq!(human_words(t).trim(), "", "{t}");
        }
        assert_eq!(
            human_words("fix it please<system-reminder>noise</system-reminder> now"),
            "fix it please now"
        );
        assert_eq!(
            human_words("<bash-input>git status</bash-input>"),
            "(ran in the shell) git status"
        );
        assert_eq!(human_words("/compact keep the plan"), "/compact keep the plan");
    }

    #[test]
    fn an_antigravity_request_is_its_body_and_system_records_are_dropped() {
        let raw = "<USER_REQUEST>\nsure fire the engines!\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: x\n</ADDITIONAL_METADATA>";
        assert_eq!(agy_request(raw), "sure fire the engines!");
        let ev = |role: &str, text: &str| crate::Event {
            event_id: format!("e-{role}"),
            parent_id: None,
            role: role.into(),
            timestamp: Some("2026-09-24T08:28:36Z".into()),
            text: Some(text.into()),
            text_provenance: vec![],
            thinking: None,
        };
        let t = to_turns(
            &[ev("USER_EXPLICIT", raw), ev("MODEL", "On it."), ev("SYSTEM", "task finished")],
            "agy",
        );
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].role.as_str(), t[0].text.as_str()), ("user", "sure fire the engines!"));
        assert_eq!(t[1].role, "assistant");
    }
}
