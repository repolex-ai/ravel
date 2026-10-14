//! Relive a soul's history into its OptMem memory, one stretch at a time.
//!
//!   ravel-relive plan <soul-repo> <from> <to>
//!   ravel-relive dump <soul-repo> <from> <to>   the stretches as the reader sees them
//!   ravel-relive verify <soul-repo> <out-dir>   check a run end to end; exit 1 on any failure
//!   ravel-relive run  <soul-repo> <from> <to> <out-dir> <backend> [max-calls]
//!
//! `out-dir` is where the memory logs go: the soul's own
//! `.ravel/_ignore/optmem` for a real run, anywhere else for a test.
//!
//! `from` and `to` are RFC 3339 prefixes compared as strings ("2026-06-01").
//! `backend` is `sub:<model>` (Claude Code on the subscription, e.g.
//! `sub:haiku`; `+think` keeps extended thinking on; `+session` keeps one
//! session across stretches, as the soul lived it) or `api:<max-usd>` (Haiku on the API key; `max-usd` is what
//! this run may add to the spend ledger).

use anyhow::{bail, Result};
use ravel::memory_extract::{Anthropic, ClaudeCode, Ledger, Llm, Speakers};
use ravel::memory_log::MemoryLog;
use ravel::memory_relive::{plan, relive};
use std::path::PathBuf;

/// The soul, by its repo's name, and its peers: the other souls beside it.
fn speakers(repo: &std::path::Path) -> Speakers {
    let agent = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let peers: Vec<String> = repo
        .parent()
        .and_then(|p| std::fs::read_dir(p).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| *n != agent && !n.starts_with('.'))
        .collect();
    let human = ravel::daemon::config::DaemonConfig::load()
        .ok()
        .and_then(|c| c.human)
        .unwrap_or_else(|| "human".to_string());
    Speakers { agent, peers, human }
}

/// Every soul's sends, for telling a peer's message from the human's: the
/// souls raveld serves, and the souls beside this one.
fn peer_index(repo: &std::path::Path) -> ravel::peers::PeerIndex {
    let mut dirs: Vec<std::path::PathBuf> = ravel::daemon::config::DaemonConfig::load()
        .map(|c| c.souls)
        .unwrap_or_default();
    if let Some(parent) = repo.parent() {
        for e in std::fs::read_dir(parent).into_iter().flatten().flatten() {
            if e.path().join(".ravel").is_dir() {
                dirs.push(e.path());
            }
        }
    }
    dirs.sort();
    dirs.dedup();
    let souls: Vec<(std::path::PathBuf, String)> = dirs
        .into_iter()
        .map(|d| {
            let n = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            (d, n)
        })
        .collect();
    let idx = ravel::peers::PeerIndex::build(&souls);
    eprintln!("[ravel-relive] {} peer sends indexed across {} souls", idx.len(), souls.len());
    idx
}

/// Context size at which a session reader starts over with a fresh wake:
/// under Haiku 5.5's 100k-token price tier.
const SESSION_TOKENS: u64 = 90_000;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["dump", repo, from, to] => {
            let repo = PathBuf::from(repo);
            let sp = speakers(&repo);
            let idx = peer_index(&repo);
            print!("{}", ravel::memory_relive::dump(&repo, &sp, &idx, from, to)?);
        }
        ["subagents", repo] => {
            // Conversations the reader drops as the soul's own subagents.
            let repo = PathBuf::from(repo);
            let sp = speakers(&repo);
            let idx = peer_index(&repo);
            let mut convs = ravel::memory_extract::conversations(&repo)?;
            let before: Vec<String> = convs.iter().map(|c| format!("{} {}", c.source, c.id)).collect();
            ravel::memory_relive::label_peers(&mut convs, &sp.agent, &idx);
            let after: std::collections::HashSet<String> =
                convs.iter().map(|c| format!("{} {}", c.source, c.id)).collect();
            for c in before.iter().filter(|c| !after.contains(*c)) {
                println!("dropped as a subagent: {c}");
            }
        }
        ["verify", repo, out] => {
            let v = ravel::memory_relive::verify(&PathBuf::from(repo), &MemoryLog::at(PathBuf::from(out)))?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            if !v.failures.is_empty() {
                std::process::exit(1);
            }
        }
        ["plan", repo, from, to] => {
            let (turns, stretches, chars) = plan(&PathBuf::from(repo), from, to)?;
            println!("{turns} turns with prose, {stretches} stretches, {chars} characters");
        }
        ["run", repo, from, to, out, backend, rest @ ..] => {
            let repo = PathBuf::from(repo);
            let max_calls = match rest {
                [] => usize::MAX,
                [n] => n.parse()?,
                _ => bail!("one optional argument after the backend: max-calls"),
            };
            let out = PathBuf::from(out);
            std::fs::create_dir_all(&out)?;
            let (llm, ledger): (Box<dyn Llm>, Ledger) = match backend.split_once(':') {
                Some(("sub", spec)) => (
                    // sub:haiku, sub:haiku+think, sub:haiku+session
                    {
                        let model = spec.split('+').next().unwrap_or(spec);
                        let cc = ClaudeCode::new(model, spec.contains("+think"))?;
                        Box::new(if spec.contains("+session") { cc.in_session() } else { cc })
                    },
                    // Subscription calls are not API spend: count them beside
                    // the output, never in the API ledger.
                    Ledger::open_at(out.join("usage.tsv"), f64::MAX)?,
                ),
                Some(("api", usd)) => {
                    let spent = Ledger::open(f64::MAX)?.spent();
                    (Box::new(Anthropic::from_key_file()?), Ledger::open(spent + usd.parse::<f64>()?)?)
                }
                _ => bail!("backend is sub:<model> or api:<max-usd>"),
            };
            let sp = speakers(&repo);
            let agent = sp.agent.clone();
            let log = MemoryLog::at(out);
            let idx = peer_index(&repo);
            // Session mode: a new session (and a wake) once the context passes this.
            let session_tokens = if backend.contains("+session") { SESSION_TOKENS } else { 0 };
            let r = relive(&repo, &agent, &sp, llm.as_ref(), &ledger, &log, &idx, from, to, max_calls, session_tokens)?;
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
        _ => bail!(
            "usage:\n  ravel-relive plan <soul-repo> <from> <to>\n  ravel-relive run <soul-repo> <from> <to> <out-dir> <sub:model or api:max-usd> [max-calls]"
        ),
    }
    Ok(())
}
