//! Relive part of a soul's history into a memory log outside its `.ravel/`:
//! the test bench for the relive backfill, run by hand before any full run.
//!
//!   ravel-relive plan <soul-repo> <from> <to>
//!   ravel-relive run  <soul-repo> <from> <to> <out-dir> <max-usd> [max-calls]
//!
//! `from` and `to` are RFC 3339 prefixes compared as strings ("2026-06-01").
//! `max-usd` is what this run may add to the spend ledger; nothing is called
//! once it is reached.

use anyhow::{bail, Result};
use ravel::memory_extract::{Anthropic, Ledger, Speakers};
use ravel::memory_log::MemoryLog;
use ravel::memory_relive::{plan, relive};
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["plan", repo, from, to] => {
            let (turns, stretches, chars) = plan(&PathBuf::from(repo), from, to)?;
            println!("{turns} turns with prose, {stretches} stretches, {chars} characters");
        }
        ["run", repo, from, to, out, usd, rest @ ..] => {
            let repo = PathBuf::from(repo);
            let max_usd: f64 = usd.parse()?;
            let max_calls: usize = match rest {
                [] => usize::MAX,
                [n] => n.parse()?,
                _ => bail!("too many arguments"),
            };
            let out = PathBuf::from(out);
            if out.components().any(|c| c.as_os_str() == ".ravel") {
                bail!("{} is inside a .ravel directory; write test output elsewhere", out.display());
            }
            let spent = Ledger::open(f64::MAX)?.spent();
            let ledger = Ledger::open(spent + max_usd)?;
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
            let llm = Anthropic::from_key_file()?;
            let log = MemoryLog::at(out);
            let r = relive(&repo, &agent, &sp, &llm, &ledger, &log, from, to, max_calls)?;
            let usd = r.input_tokens as f64 / 1e6 + r.output_tokens as f64 * 5.0 / 1e6;
            println!("{}", serde_json::to_string_pretty(&r)?);
            println!("cost ${usd:.2}");
        }
        _ => bail!(
            "usage:\n  ravel-relive plan <soul-repo> <from> <to>\n  ravel-relive run <soul-repo> <from> <to> <out-dir> <max-usd> [max-calls]"
        ),
    }
    Ok(())
}
