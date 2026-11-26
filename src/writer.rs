//! Who writes a soul's memory: a harness's own command line, run on the
//! user's subscription (goodlux, 2026-11-05: subscription first, and harness
//! independent — ravel must work for a soul that never touches Claude).
//!
//! The memory needs one thing from a model: a structured answer to a prompt.
//! [`Llm`] is that, and nothing else. A harness becomes a writer when its
//! command line can give it under five conditions, the writer contract:
//!
//! 1. **One-shot and non-interactive.** A prompt in, an answer out, then exit.
//! 2. **A structured answer.** A JSON schema, or JSON that can be held to one.
//! 3. **The user's own login.** The subscription the user already has, never
//!    an API key in the environment (an exported key leaks into every child
//!    process and bills per call; it billed goodlux once, 2026-09-30).
//! 4. **No tools, no project.** No file access, no MCP servers, no hooks, and
//!    no project instructions picked up from the working directory.
//! 5. **Nothing written where ravel reads.** A writer's sessions are a reading
//!    aid, not a life. If the harness saves them, they must land outside every
//!    soul's transcript folder, and be deletable.
//!
//! Optional: a resumable session ([`Llm::context_tokens`], [`Llm::reset`]),
//! so a run reads on the way the soul lived instead of waking for every
//! stretch. Without one, every call wakes: correct, and costlier.
//!
//! Built: Claude Code. Planned, as stubs that refuse by name: Codex, Gemini
//! CLI, Antigravity, Hermes. What each one still needs is said in
//! [`Harness::plan`], so a stub's refusal is a roadmap, not a dead end.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::PathBuf;

/// One structured-output call. A trait so tests run without a network.
pub trait Llm: Sync + Send {
    fn model(&self) -> &str;
    /// Returns the parsed JSON and the (input, output) token counts.
    fn json(&self, system: &str, user: &str, schema: &Value) -> Result<(Value, u64, u64)>;

    /// For a model that keeps a session across calls: the tokens its
    /// context held at the last call. 0 for one-shot calls.
    fn context_tokens(&self) -> u64 {
        0
    }

    /// Start the next call in a fresh session. A no-op for one-shot calls.
    fn reset(&self) {}
}

/// The harnesses ravel knows of as memory writers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    ClaudeCode,
    Codex,
    Gemini,
    Antigravity,
    Hermes,
}

impl Harness {
    pub const ALL: [Harness; 5] = [
        Harness::ClaudeCode,
        Harness::Codex,
        Harness::Gemini,
        Harness::Antigravity,
        Harness::Hermes,
    ];

    /// The name the config uses: `memory: { writer: claude-code }`.
    pub fn name(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
            Harness::Gemini => "gemini",
            Harness::Antigravity => "antigravity",
            Harness::Hermes => "hermes",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|h| h.name() == s.trim())
            .ok_or_else(|| {
                anyhow!(
                    "unknown memory writer {s:?}; ravel knows {}",
                    Self::ALL.map(|h| h.name()).join(", ")
                )
            })
    }

    /// The model a writer uses when the config names none.
    pub fn default_model(self) -> &'static str {
        match self {
            // goodlux, 2026-10-15: Haiku. It relived all 20 souls cleanly.
            Harness::ClaudeCode => "claude-haiku-5-5",
            _ => "",
        }
    }

    pub fn built(self) -> bool {
        self == Harness::ClaudeCode
    }

    /// What this harness still needs before it can write, against the writer
    /// contract above. Unchecked claims say so.
    pub fn plan(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "built: `claude -p` with --json-schema, no tools, no settings, an empty working directory, sessions resumed by id and deleted after",
            Harness::Codex => "not built. Needs: `codex exec` (its non-interactive mode) answering to a JSON schema on the ChatGPT login, with tools and sandbox writes off, and its sessions kept out of the transcripts ravel reads. To check: the exact flags, and how it reports a usage limit.",
            Harness::Gemini => "not built. Needs: Gemini CLI in non-interactive mode (`gemini -p`) with JSON output, on the Google login, tools off, run outside any project. To check: whether it holds output to a schema, and whether it can resume a session by id.",
            Harness::Antigravity => "not built. Needs: a headless, one-shot way to prompt Antigravity on the user's login; none is known yet. Until there is one, an Antigravity soul's memory can be written by another harness the user has.",
            Harness::Hermes => "not built. Needs: Hermes Agent's one-shot command-line mode answering in JSON, on whatever model login the user configured for it, with its tools and memory off so it does not write its own notes. To check: the exact flags.",
        }
    }

    /// A writer for this harness, or a refusal naming what is missing.
    pub fn open(self, model: &str) -> Result<Box<dyn Llm>> {
        match self {
            Harness::ClaudeCode => Ok(Box::new(ClaudeCode::new(model)?.in_session())),
            other => anyhow::bail!("memory writer {}: {}", other.name(), other.plan()),
        }
    }
}

/// The harness said the account is out of its allowance for now: Claude
/// Code's five-hour or weekly limit. Every later call fails the same way until
/// the limit resets, so the caller stops and waits instead of retrying.
///
/// Matched loosely, because the wording has changed between Claude Code
/// versions ("Claude AI usage limit reached|<epoch>", "You've hit your
/// limit · resets 3pm") and no copy of the current one was on this machine to
/// test against (2026-11-05).
pub fn is_usage_limit(e: &anyhow::Error) -> bool {
    is_usage_limit_text(&format!("{e:#}"))
}

/// [`is_usage_limit`], on a failure already said in words.
pub fn is_usage_limit_text(m: &str) -> bool {
    let m = m.to_lowercase();
    m.contains("usage limit")
        || m.contains("hit your limit")
        || m.contains("hit your session limit")
        || m.contains("hit your weekly limit")
        || (m.contains("limit") && m.contains("resets"))
}

/// Claude through Claude Code's print mode, on the user's subscription: no
/// API key, nothing billed per call (goodlux, 2026-10-06: the memory index
/// must run on a subscription).
///
/// Each call is one `claude -p` with no tools, no MCP servers, no settings
/// (so no hooks), no skills and no thinking. `ANTHROPIC_API_KEY` is removed
/// from the child's environment so it can never fall through to API billing,
/// and it runs in an empty directory so no project's CLAUDE.md is picked up.
pub struct ClaudeCode {
    model: String,
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
    pub fn new(model: &str) -> Result<Self> {
        let dir = std::env::temp_dir().join("ravel-claude-code");
        std::fs::create_dir_all(&dir)?;
        Ok(ClaudeCode {
            model: model.to_string(),
            dir,
            session: None,
        })
    }

    /// A writer that keeps one session across calls.
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
    let h = format!(
        "{:x}",
        Sha256::digest(format!("{t}-{n}-{}", std::process::id()))
    );
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

    fn json(&self, system: &str, user: &str, schema: &Value) -> Result<(Value, u64, u64)> {
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
        // Thinking off: about a minute and thousands of tokens per call for
        // no better notes (measured 2026-10-06).
        let mut child = Command::new("claude")
            .env("MAX_THINKING_TOKENS", "0")
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
                "claude -p printed no JSON (exit {}): {}{}",
                out.status,
                stdout.chars().take(300).collect::<String>(),
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
        let so = r.get("structured_output").cloned().ok_or_else(|| {
            // Say what came back instead, so a stopped run can be read.
            let said: String = r["result"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(300)
                .collect();
            anyhow!("claude -p returned no structured output; it said: {said:?}")
        })?;
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
        self.session
            .as_ref()
            .map_or(0, |m| m.lock().unwrap().context)
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
                    if f.is_file()
                        && e.file_name()
                            .to_string_lossy()
                            .contains("ravel-claude-code")
                    {
                        let _ = std::fs::remove_file(f);
                    }
                }
            }
        }
        s.context = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_harness_parses_by_name_and_a_stub_refuses_with_its_plan() {
        for h in Harness::ALL {
            assert_eq!(Harness::parse(h.name()).unwrap(), h);
            assert!(!h.plan().is_empty());
            if !h.built() {
                let e = h.open("m").err().expect("a stub must refuse");
                assert!(format!("{e}").contains("not built"), "{e}");
            }
        }
        assert!(Harness::parse("vim").is_err());
    }

    #[test]
    fn a_usage_limit_is_told_from_other_failures() {
        for m in [
            "claude -p failed: Claude AI usage limit reached|1762390800",
            "claude -p failed: You've hit your limit · resets 3pm (America/Los_Angeles)",
            "claude -p failed: 5-hour limit reached ∙ resets 11pm",
        ] {
            assert!(is_usage_limit(&anyhow!("{m}")), "{m}");
        }
        for m in [
            "claude -p returned no structured output; it said: \"done\"",
            "claude -p failed: API Error: Haiku 5.5 can't help with this.",
        ] {
            assert!(!is_usage_limit(&anyhow!("{m}")), "{m}");
        }
    }
}

/// Claude through the Anthropic API, billed per call against the account's
/// API credits. **Not a memory writer**: the writer contract above forbids an
/// API key, and the daemon never opens one. It exists for one job, run by
/// hand: `raveld reword-api`, moving an old memory to the current merge
/// wording all at once instead of a few merges a pass (goodlux, 2026-11-25:
/// the rewording backlog on the Max plan's API credits, the ongoing writing
/// stays on the subscription).
///
/// The key is read from the macOS keychain (`security`, account `ravel`,
/// service [`AnthropicApi::KEYCHAIN_SERVICE`]) when the writer opens, held in
/// memory only, and never put in the environment, so no child can inherit it.
/// One-shot: no session, so every call carries only its own prompt.
pub struct AnthropicApi {
    model: String,
    key: String,
    http: reqwest::blocking::Client,
}

impl AnthropicApi {
    pub const KEYCHAIN_SERVICE: &'static str = "ravel-anthropic-api-key";

    pub fn from_keychain(model: &str) -> Result<Self> {
        let out = std::process::Command::new("security")
            .args(["find-generic-password", "-a", "ravel", "-s", Self::KEYCHAIN_SERVICE, "-w"])
            .output()
            .context("running `security` to read the API key")?;
        anyhow::ensure!(
            out.status.success(),
            "no API key in the keychain under account `ravel`, service `{}`",
            Self::KEYCHAIN_SERVICE
        );
        let key: String = String::from_utf8(out.stdout)?.split_whitespace().collect();
        anyhow::ensure!(!key.is_empty(), "the keychain entry `{}` is empty", Self::KEYCHAIN_SERVICE);
        Ok(AnthropicApi {
            model: model.to_string(),
            key,
            http: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()?,
        })
    }
}

impl Llm for AnthropicApi {
    fn model(&self) -> &str {
        &self.model
    }

    fn json(&self, system: &str, user: &str, schema: &Value) -> Result<(Value, u64, u64)> {
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 4096,
            "system": system,
            "messages": [{"role": "user", "content": user}],
            // Thinking on (adaptive, Haiku 5.5's default). Tried off at
            // effort low, 2026-11-25: one-shot and unable to think, Haiku sent
            // the same 309-byte line back to all twelve "too long" asks.
            "output_config": {"effort": "medium", "format": {"type": "json_schema", "schema": schema}},
        });
        let res = self
            .http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .context("calling the Anthropic API")?;
        let status = res.status();
        let v: Value = res.json().context("reading the Anthropic API's answer")?;
        anyhow::ensure!(status.is_success(), "Anthropic API {status}: {}", v["error"]["message"]);
        if v["stop_reason"] == "refusal" {
            anyhow::bail!("the model can't help with this (API refusal: {})", v["stop_details"]);
        }
        let text = v["content"]
            .as_array()
            .and_then(|c| c.iter().find(|b| b["type"] == "text"))
            .and_then(|b| b["text"].as_str())
            .ok_or_else(|| anyhow!("Anthropic API answer has no text: stop_reason {}", v["stop_reason"]))?;
        let u = &v["usage"];
        let input = u["input_tokens"].as_u64().unwrap_or(0)
            + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
            + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
        let parsed: Value = serde_json::from_str(text).context("the API's answer is not JSON")?;
        Ok((parsed, input, u["output_tokens"].as_u64().unwrap_or(0)))
    }
}
