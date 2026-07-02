//! Weave — ingest many sessions into ONE persistent graph, then see it.
//!
//! Walks a directory of Claude Code `.jsonl` session logs, runs the emojikey
//! reader over each, and ingests every session's annotations into its own named
//! graph inside a persistent on-disk oxigraph store. Then runs the cross-session
//! query and prints what it found — the "see graphs of sessions" payoff.
//!
//! Run:
//!   cargo run --bin weave-ingest -- <store-dir> <sessions-dir> [--authored-only]
//!
//! e.g.
//!   cargo run --bin weave-ingest -- /tmp/weave-store \
//!       ~/.claude/projects/-Users-dev-repos-SQUAD-spaceGOAT
//!
//! Idempotent: re-running over the same sessions replaces each session's graph
//! in place (deterministic IRIs + per-transcript named-graph clear-on-reload).

use anyhow::{Context, Result};
use weave::{adapter, graph, reader};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let store_dir = args.next().context("usage: weave-ingest <store-dir> <sessions-dir> [--authored-only]")?;
    let sessions_dir = args.next().context("missing <sessions-dir>")?;
    let authored_only = args.any(|a| a == "--authored-only");

    let store = graph::open(&store_dir)?;
    // Partition mirrors Pool's urn:soul:<sha>: shape; opaque to the engine. In a
    // real soul adapter this is the soul sha; here one demo partition for the box.
    let partition = "urn:soul:demo-ingest-sha:Weave/Turn/";

    // --- walk the session logs ---
    let mut sessions = 0usize;
    let mut total_keys = 0usize;
    let entries = std::fs::read_dir(&sessions_dir)
        .with_context(|| format!("reading sessions dir {sessions_dir}"))?;
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect();
    paths.sort();

    for path in &paths {
        let jsonl = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("skip {}: {e}", path.display());
                continue;
            }
        };
        let events = adapter::parse_transcript(&jsonl)?;
        let anns = reader::emojikey_read(&events);
        if anns.is_empty() {
            continue;
        }
        let transcript_id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("transcript");
        let n = graph::ingest_annotations(&store, &anns, transcript_id, partition)?;
        sessions += 1;
        total_keys += anns.len();
        println!(
            "  ingested {:>3} annotation-triples from {} ({} key(s))",
            n,
            transcript_id,
            anns.len()
        );
    }
    println!(
        "\n[ingest] {sessions} session(s) with emojikeys → persistent store at {store_dir}  ({total_keys} keys total)"
    );

    // --- the payoff: query the graph of sessions ---
    let filter = if authored_only { Some("authored") } else { None };
    let hits = graph::query_emojikeys(&store, filter)?;
    println!(
        "\n=== emojikeys across sessions{} ===",
        if authored_only { " (authored only)" } else { "" }
    );
    let mut last = String::new();
    for h in &hits {
        if h.transcript != last {
            println!("\n▸ session {}", short(&h.transcript));
            last = h.transcript.clone();
        }
        println!(
            "    [{}] [ME|{}]~[CONTENT|{}]~[YOU|{}]  @ {}",
            h.source_kind, h.me, h.content, h.you, h.ts
        );
    }
    println!("\n[SEE GRAPH] {} emojikey annotation(s) queried across {} session graph(s).",
        hits.len(),
        hits.iter().map(|h| &h.transcript).collect::<std::collections::BTreeSet<_>>().len(),
    );
    Ok(())
}

/// Shorten a uuid-ish transcript id for display.
fn short(id: &str) -> String {
    if id.len() > 12 { format!("{}…", &id[..12]) } else { id.to_string() }
}
