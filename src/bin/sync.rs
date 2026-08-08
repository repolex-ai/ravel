//! ravel-sync — bring a soul's ravel up to date (mirror + ingest, idempotent).
//!
//! The blessed entrypoint the `SessionEnd-ravel-sync` hook calls. All logic
//! lives in `ravel::sync` (the app package, per the hook-authoring guide);
//! this bin only parses args and prints the one-line summary.
//!
//! Usage: ravel-sync [--diagnostic] <soul-repo> [sessions-dir]
//!   sessions-dir defaults to this repo's Claude Code dir
//!   (~/.claude/projects/<slug>).
//!
//! `--diagnostic` is the tool's ONE diagnostic switch (squad rule): a strictly
//! read-only health report — layout, kit/hook wiring, mirror freshness with
//! live-session awareness, store counts. Exit 0 = healthy (growing live
//! sessions are healthy); exit 1 = something needs attention, named loudly.

use anyhow::{Context, Result};
use ravel::sync;
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1).peekable();
    let diagnostic = args.peek().map(|a| a == "--diagnostic").unwrap_or(false);
    if diagnostic {
        args.next();
    }
    let repo: PathBuf = args
        .next()
        .context("usage: ravel-sync [--diagnostic] <soul-repo> [sessions-dir]")?
        .into();
    let src: Option<PathBuf> = args.next().map(Into::into);

    if diagnostic {
        let findings = sync::diagnose(&repo, src.as_deref())?;
        println!("[ravel-diagnostic] {}", repo.display());
        let mut attention = false;
        for f in &findings {
            println!("  {}{}", if f.attention { "ATTENTION — " } else { "" }, f.line);
            attention |= f.attention;
        }
        println!(
            "  verdict: {}",
            if attention { "ATTENTION (see above)" } else { "OK" }
        );
        if attention {
            std::process::exit(1);
        }
        return Ok(());
    }

    let stats = sync::sync_soul(&repo, src.as_deref())?;
    println!(
        "[ravel-sync] mirrored {} (unchanged {}) → ingested {} session(s) ({} skipped unchanged), {} turns, {} emojikeys in {}",
        stats.mirrored,
        stats.unchanged,
        stats.sessions,
        stats.skipped,
        stats.turns,
        stats.keys,
        repo.join(sync::STORE_SUBDIR).display(),
    );
    Ok(())
}
