//! Relive part of a soul's history into a memory log outside its `.ravel/`:
//! the test bench for the relive backfill, run by hand before any full run.
//!
//!   ravel-relive plan <soul-repo> <from> <to>
//!   ravel-relive run  <soul-repo> <from> <to> <out-dir> <backend> [max-calls] [minimal]
//!
//! `from` and `to` are RFC 3339 prefixes compared as strings ("2026-06-01").
//! `backend` is `sub:<model>` (Claude Code on the subscription, e.g.
//! `sub:haiku`, or `sub:haiku+think` to keep extended thinking on) or `api:<max-usd>` (Haiku on the API key; `max-usd` is what
//! this run may add to the spend ledger).

use anyhow::{bail, Result};
use ravel::memory_extract::{Anthropic, ClaudeCode, Ledger, Llm, Speakers};
use ravel::memory_log::MemoryLog;
use ravel::memory_relive::{plan, relive, Prompt};
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["plan", repo, from, to] => {
            let (turns, stretches, chars) = plan(&PathBuf::from(repo), from, to)?;
            println!("{turns} turns with prose, {stretches} stretches, {chars} characters");
        }
        ["run", repo, from, to, out, backend, rest @ ..] => {
            let repo = PathBuf::from(repo);
            // Optional, in any order: a call cap (a number) and `minimal`.
            let mut max_calls = usize::MAX;
            let mut style = Prompt::Full;
            for x in rest {
                match *x {
                    "minimal" => style = Prompt::Minimal,
                    n => max_calls = n.parse()?,
                }
            }
            let out = PathBuf::from(out);
            std::fs::create_dir_all(&out)?;
            let (llm, ledger): (Box<dyn Llm>, Ledger) = match backend.split_once(':') {
                Some(("sub", spec)) => (
                    // sub:haiku (no thinking) or sub:haiku+think
                    Box::new(ClaudeCode::new(
                        spec.trim_end_matches("+think"),
                        spec.ends_with("+think"),
                    )?),
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
            let agent = repo
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // Peers: the other souls beside this one.
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
            let sp = Speakers { agent: agent.clone(), peers };
            let log = MemoryLog::at(out);
            let r = relive(&repo, &agent, &sp, llm.as_ref(), &ledger, &log, from, to, max_calls, style)?;
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
        _ => bail!(
            "usage:\n  ravel-relive plan <soul-repo> <from> <to>\n  ravel-relive run <soul-repo> <from> <to> <out-dir> <sub:model or api:max-usd> [max-calls] [minimal]"
        ),
    }
    Ok(())
}
