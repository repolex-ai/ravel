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
//!   side      <soul-repo> <out-dir> <min-notes> [every-nth-small]
//!             a judge's packet for a rebuild: each merge of at least <min-notes>
//!             notes in <out-dir>, beside the soul's own merge of the same notes and
//!             the lines it was built from; plus every nth smaller merge, with its
//!             sources. Markdown on stdout.
//!   notes     <soul-repo> <out-dir> <from> <to> <model>
//!             the notes for the turns in [from, to), written again by today's
//!             reader into <out-dir>: a test of a change to what the writer is
//!             shown or told. <out-dir> starts as the soul's memory as it stood
//!             before <from> (its notes up to there, and their merges), so the
//!             writer wakes as it did then. Run again to resume a stopped run.
//!   notes-side <soul-repo> <out-dir> <from> <to> [key-file]
//!             a judge's packet for `notes`: the soul's own notes for the range
//!             beside the trial's. With a key file the two are A and B, and the
//!             key says which is which. `dump` over the same range is the
//!             transcript to judge them against. Markdown on stdout.
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
                    reword: ravel::daemon::config::Reword::Off,
                });
            }
            let id = ravel::daemon::registry::soul_for(&PathBuf::from(repo).canonicalize()?)?.id;
            let d = std::sync::Arc::new(ravel::daemon::Daemon::open(cfg)?);
            let rt = tokio::runtime::Runtime::new()?;
            let m = rt.block_on(d.memory_pass(&id))?;
            println!("{}", serde_json::to_string_pretty(&m)?);
        }
        ["side", repo, out, min, rest @ ..] => {
            // Blind when a key file is given: the two wordings appear as A and B,
            // in an order set per merge, each with the lines it was built from.
            use ravel::memory::{block_id, parse_block, RAW_MAX};
            let min: usize = min.parse()?;
            let nth: usize = rest.first().map(|n| n.parse()).transpose()?.unwrap_or(0);
            let key_file = rest.get(1).map(PathBuf::from);
            let trial = MemoryLog::at(PathBuf::from(out));
            let own = MemoryLog::for_soul(&PathBuf::from(repo)).summaries()?;
            let notes = trial.memories()?;
            let sums = trial.summaries()?;
            let mut blocks: Vec<(usize, usize)> = sums.keys().filter_map(|k| parse_block(k)).collect();
            blocks.sort_by_key(|&(lo, hi)| (std::cmp::Reverse(hi - lo), lo));
            let get = |m: &std::collections::HashMap<String, ravel::memory_log::Summary>, b| {
                m.get(&block_id(b)).map(|s| s.text.clone()).unwrap_or_default()
            };
            let sources = |m: &std::collections::HashMap<String, ravel::memory_log::Summary>, b: (usize, usize)| {
                let n = b.1 - b.0;
                let mut o = String::new();
                if n <= RAW_MAX {
                    for (k, x) in notes.iter().enumerate().take(b.1).skip(b.0) {
                        o += &format!("- note #{k} {} {}\n", x.date(), x.text);
                    }
                } else {
                    let mid = b.0 + n / 2;
                    o += &format!("- #{} {}\n", block_id((b.0, mid)), get(m, (b.0, mid)));
                    o += &format!("- #{} {}\n", block_id((mid, b.1)), get(m, (mid, b.1)));
                }
                o
            };
            let mut key = String::new();
            let mut small = 0usize;
            for b in blocks {
                let n = b.1 - b.0;
                if n < min {
                    small += 1;
                    if nth == 0 || !small.is_multiple_of(nth) {
                        continue;
                    }
                }
                let id = block_id(b);
                println!("## #{id} ({n} notes, {} to {})\n", notes[b.0].date(), notes[b.1 - 1].date());
                let Some(key_file) = &key_file else {
                    println!("**Trial:** {}\n", get(&sums, b));
                    if let Some(o) = own.get(&id) {
                        println!("**The soul's memory now:** {}\n", o.text);
                    }
                    println!("**Built from:**\n\n{}", sources(&sums, b));
                    continue;
                };
                let _ = key_file;
                use sha2::Digest;
                let flip = sha2::Sha256::digest(id.as_bytes())[0] & 1 == 1;
                let (a, a_name, bb, b_name) = if flip {
                    (&own, "current", &sums, "trial")
                } else {
                    (&sums, "trial", &own, "current")
                };
                key += &format!("{id}\tA={a_name}\tB={b_name}\n");
                println!("**A:** {}\n\nA was built from:\n\n{}", get(a, b), sources(a, b));
                println!("**B:** {}\n\nB was built from:\n\n{}", get(bb, b), sources(bb, b));
            }
            if let Some(k) = key_file {
                std::fs::write(k, key)?;
            }
        }
        ["notes", repo, out, from, to, model] => {
            let repo = PathBuf::from(repo);
            let out = PathBuf::from(out);
            if out.starts_with(repo.join(".ravel")) {
                bail!("<out-dir> is inside the soul's .ravel: a trial never writes where raveld does");
            }
            let trial = MemoryLog::at(out.clone());
            let seed_file = out.join("seed.json");
            if !seed_file.exists() {
                if out.join("memories.jsonl").exists() {
                    bail!("{} holds a memory but no seed.json: not a `notes` trial folder", out.display());
                }
                // The soul's memory as it stood before <from>: its notes up to
                // the first one from the range, and every merge of those notes.
                let own = MemoryLog::for_soul(&repo);
                let notes = own.memories()?;
                let n = notes.iter().position(|m| m.ts.as_str() >= *from).unwrap_or(notes.len());
                trial.append_chunk(&notes[..n], &[])?;
                let mut sums: Vec<_> = own
                    .summaries()?
                    .into_values()
                    .filter(|s| ravel::memory::parse_block(&s.block).is_some_and(|(_, hi)| hi <= n))
                    .collect();
                sums.sort_by(|a, b| a.block.cmp(&b.block));
                for s in &sums {
                    trial.append_summary(s)?;
                }
                std::fs::write(
                    &seed_file,
                    serde_json::json!({ "notes": n, "from": from, "to": to, "soul": repo }).to_string(),
                )?;
                eprintln!("seeded {} with {n} notes and {} merges from before {from}", out.display(), sums.len());
            }
            let sp = speakers(&repo, &cfg);
            let convs = ravel::memory_relive::load(&repo, &sp, &peer_index(&cfg))?;
            let llm = ClaudeCode::new(model)?;
            let r = ravel::memory_relive::relive(
                &repo,
                &sp,
                &llm,
                &trial,
                &convs,
                from,
                to,
                usize::MAX,
                ravel::daemon::memory::SESSION_TOKENS,
                &std::sync::atomic::AtomicBool::new(false),
            )?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            if r.stopped.is_some() {
                std::process::exit(2);
            }
        }
        ["notes-side", repo, out, from, to, rest @ ..] => {
            let out = PathBuf::from(out);
            let seed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(out.join("seed.json"))?)?;
            let n = seed["notes"].as_u64().unwrap_or(0) as usize;
            let own: Vec<_> = MemoryLog::for_soul(&PathBuf::from(repo))
                .memories()?
                .into_iter()
                .filter(|m| m.ts.as_str() >= *from && m.ts.as_str() < *to)
                .collect();
            let trial: Vec<_> = MemoryLog::at(out).memories()?.into_iter().skip(n).collect();
            let list = |ms: &[ravel::memory_log::Memory]| {
                ms.iter().map(|m| format!("- {} {}\n", &m.ts[..16.min(m.ts.len())], m.text)).collect::<String>()
            };
            println!("# Notes for {from} to {to}\n");
            match rest {
                [] => {
                    println!("## The soul's memory now ({} notes)\n\n{}", own.len(), list(&own));
                    println!("## Trial ({} notes)\n\n{}", trial.len(), list(&trial));
                }
                [key_file] => {
                    use sha2::Digest;
                    let flip = sha2::Sha256::digest(format!("{from}{to}").as_bytes())[0] & 1 == 1;
                    let (a, a_name, b, b_name) =
                        if flip { (&own, "current", &trial, "trial") } else { (&trial, "trial", &own, "current") };
                    println!("## A ({} notes)\n\n{}", a.len(), list(a));
                    println!("## B ({} notes)\n\n{}", b.len(), list(b));
                    std::fs::write(key_file, format!("A={a_name}\tB={b_name}\n"))?;
                }
                _ => bail!("one optional argument after <to>: a file to write the A/B key to"),
            }
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
            "usage: cargo run --release --example relive -- plan|dump <soul-repo> <from> <to> | subagents|unread|pass <soul-repo> | notes <soul-repo> <out-dir> <from> <to> <model> | notes-side <soul-repo> <out-dir> <from> <to> [key-file] | rebuild <soul-repo> <out-dir> <model> [extra.txt] | side <soul-repo> <out-dir> <min-notes> [every-nth-small] [key-file]"
        ),
    }
    Ok(())
}
