//! A developer's tool for the memory reader, never installed and never
//! needed: raveld writes every memory itself. Run it from the repo with
//! `cargo run --release --example relive -- <command>`.
//!
//!   plan      <soul-repo> <from> <to>   what a relive over the range would read
//!   dump      <soul-repo> <from> <to>   the stretches, exactly as the writer is shown them
//!   subagents <soul-repo>               conversations the reader drops as the soul's own subagents
//!   unread    <soul-repo>               what raveld's next memory pass would find to read
//!   pass      <soul-repo>               raveld's memory pass on one soul, now, into its real
//!             memory: a trial before the loop is turned on. The config's writer, or
//!             claude-code when the config names none.
//!   rebuild   <soul-repo> <out-dir> <model> [extra.txt]
//!             every merge due over the notes in <out-dir>, cold: a side-by-side
//!             test of the merge wording; extra.txt is added to OptMem's prompt.
//!             <out-dir> must hold a copy of the notes, never the soul's own memory.
//!
//! `from` and `to` are RFC 3339 prefixes compared as strings ("2026-06-01").
//! Peers are the souls in ravel's config, as raveld sees them.

use anyhow::{bail, Result};
use ravel::daemon::config::DaemonConfig;
use ravel::daemon::memory::soul_name;
use ravel::memory_extract::Speakers;
use ravel::memory_log::MemoryLog;
use ravel::peers::PeerIndex;
use ravel::writer::ClaudeCode;
use std::path::PathBuf;

fn speakers(repo: &std::path::Path, cfg: &DaemonConfig) -> Speakers {
    let agent = soul_name(repo);
    Speakers {
        peers: cfg
            .souls
            .iter()
            .map(|p| soul_name(p))
            .filter(|n| *n != agent)
            .collect(),
        agent,
        human: cfg.human.clone().unwrap_or_else(|| "human".to_string()),
    }
}

fn peer_index(cfg: &DaemonConfig) -> PeerIndex {
    let souls: Vec<(PathBuf, String)> = cfg
        .souls
        .iter()
        .map(|p| (p.clone(), soul_name(p)))
        .collect();
    PeerIndex::build(&souls)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let cfg = DaemonConfig::load()?;
    match a.as_slice() {
        ["plan", repo, from, to] => {
            let (turns, stretches, chars) = ravel::memory_relive::plan(&PathBuf::from(repo), from, to)?;
            println!("{turns} turns with prose, {stretches} stretches, {chars} characters");
        }
        ["dump", repo, from, to] => {
            let repo = PathBuf::from(repo);
            let sp = speakers(&repo, &cfg);
            print!("{}", ravel::memory_relive::dump(&repo, &sp, &peer_index(&cfg), from, to)?);
        }
        ["subagents", repo] => {
            let repo = PathBuf::from(repo);
            let sp = speakers(&repo, &cfg);
            let mut convs = ravel::memory_extract::conversations(&repo)?;
            let before: Vec<String> = convs.iter().map(|c| format!("{} {}", c.source, c.id)).collect();
            ravel::memory_relive::label_peers(&mut convs, &sp.agent, &peer_index(&cfg));
            let after: std::collections::HashSet<String> =
                convs.iter().map(|c| format!("{} {}", c.source, c.id)).collect();
            for c in before.iter().filter(|c| !after.contains(*c)) {
                println!("dropped as a subagent: {c}");
            }
        }
        ["unread", repo] => {
            let repo = PathBuf::from(repo);
            let sp = speakers(&repo, &cfg);
            let convs = ravel::memory_relive::load(&repo, &sp, &peer_index(&cfg))?;
            let log = MemoryLog::for_soul(&repo);
            let settled = (chrono::Utc::now() - ravel::daemon::memory::SETTLE)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let (n, chars, oldest) = ravel::memory_relive::unread(&convs, &log, &settled)?;
            let (all, _, _) = ravel::memory_relive::unread(&convs, &log, "9999")?;
            println!("{n} settled turns unread ({chars} characters, oldest {oldest}); {all} unread in all");
        }
        ["pass", repo] => {
            let mut cfg = cfg.clone();
            if cfg.memory.is_none() {
                let h = ravel::writer::Harness::ClaudeCode;
                cfg.memory = Some(ravel::daemon::config::MemoryWriter {
                    harness: h,
                    model: h.default_model().to_string(),
                });
            }
            let id = ravel::daemon::registry::soul_for(&PathBuf::from(repo).canonicalize()?)?.id;
            let d = std::sync::Arc::new(ravel::daemon::Daemon::open(cfg)?);
            let rt = tokio::runtime::Runtime::new()?;
            let m = rt.block_on(d.memory_pass(&id))?;
            println!("{}", serde_json::to_string_pretty(&m)?);
        }
        ["rebuild", repo, out, model, rest @ ..] => {
            let repo = PathBuf::from(repo);
            let out = PathBuf::from(out);
            if out.starts_with(repo.join(".ravel")) {
                bail!("<out-dir> is inside the soul's .ravel: rebuild a copy, never the memory raveld writes");
            }
            let extra = match rest {
                [] => None,
                [f] => Some(std::fs::read_to_string(f)?.trim().to_string()),
                _ => bail!("one optional argument after the model: a file of text to add to the merge prompt"),
            };
            let sp = speakers(&repo, &cfg);
            let llm = ClaudeCode::new(model)?;
            let r = ravel::memory_relive::rebuild(&llm, &repo, &sp.agent, &MemoryLog::at(out), extra.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            if r.stopped.is_some() {
                std::process::exit(2);
            }
        }
        _ => bail!(
            "usage: cargo run --release --example relive -- plan|dump <soul-repo> <from> <to> | subagents <soul-repo> | rebuild <soul-repo> <out-dir> <model> [extra.txt]"
        ),
    }
    Ok(())
}
