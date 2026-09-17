//! Ravel — ingest many sessions into ONE persistent graph, then see it.
//!
//! Walks a directory of Claude Code `.jsonl` session logs, runs the emojikey
//! reader over each, and ingests every session's annotations into its own named
//! graph inside a persistent on-disk oxigraph store. Then runs the cross-session
//! query and prints what it found — the "see graphs of sessions" payoff.
//!
//! Run:
//!   cargo run --bin ravel-ingest -- <store-dir> <sessions-dir> [--authored-only]
//!
//! e.g.
//!   cargo run --bin ravel-ingest -- /tmp/ravel-store \
//!       ~/.claude/projects/-Users-dev-repos-SQUAD-spaceGOAT
//!
//! Turn subjects carry NO soul identity (`https://repolex.ai/ravel/Turn/<id>`);
//! the soul scope IS the store you point this at — one soul repo, one store.
//! See `soul.rs` for the ruling.
//!
//! Idempotent: re-running over the same sessions replaces each session's graph
//! in place (deterministic IRIs + per-transcript named-graph clear-on-reload).

use anyhow::{Context, Result};
use ravel::{adapter, graph, reader, soul};

fn main() -> Result<()> {
    let mut positional = Vec::new();
    let mut authored_only = false;
    for a in std::env::args().skip(1) {
        if a == "--authored-only" {
            authored_only = true;
        } else {
            positional.push(a);
        }
    }
    let mut positional = positional.into_iter();
    let store_dir = positional
        .next()
        .context("usage: ravel-ingest <store-dir> <sessions-dir> [--authored-only]")?;
    let sessions_dir = positional.next().context("missing <sessions-dir>")?;

    let store = graph::open(&store_dir)?;
    // The soul-adapter's constant partition: subjects carry no soul identity;
    // the store you opened IS the soul scope (see soul.rs).
    let partition = soul::TURN_PARTITION;
    println!("[soul] turn partition: {partition}");

    // --- walk the session logs ---
    let mut sessions = 0usize;
    let mut total_keys = 0usize;
    let mut total_turns = 0usize;
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
        // FULL SPINE: run the reader for annotations, but ingest EVERY session's
        // turns regardless — the spine is the deliverable, not the annotations. A
        // session with zero emojikeys is still a conversation worth storing.
        let anns = reader::emojikey_read(&events);
        if events.is_empty() {
            continue;
        }
        let transcript_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("transcript");
        let n = graph::ingest_transcript(&store, &events, &anns, transcript_id, partition)?;
        sessions += 1;
        total_turns += events.len();
        total_keys += anns.len();
        println!(
            "  {:>6} turns  →  {:>6} triples   {}  ({} key(s))",
            events.len(),
            n,
            transcript_id,
            anns.len()
        );
    }
    println!(
        "\n[ingest] {sessions} session(s), {total_turns} turns → persistent store at {store_dir}  ({total_keys} emojikeys)"
    );

    // --- the payoff: query the graph of sessions ---
    let filter = if authored_only {
        Some("authored")
    } else {
        None
    };
    let hits = graph::query_emojikeys(&store, filter)?;
    println!(
        "\n=== emojikeys across sessions{} ===",
        if authored_only {
            " (authored only)"
        } else {
            ""
        }
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
    println!(
        "\n[SEE GRAPH] {} emojikey annotation(s) queried across {} session graph(s).",
        hits.len(),
        hits.iter()
            .map(|h| &h.transcript)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
    );
    Ok(())
}

/// Shorten a uuid-ish transcript id for display.
fn short(id: &str) -> String {
    if id.len() > 12 {
        format!("{}…", &id[..12])
    } else {
        id.to_string()
    }
}
