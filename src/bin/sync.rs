//! ravel-sync — bring a soul's ravel up to date (mirror + ingest, idempotent).
//!
//! The blessed entrypoint the `SessionEnd-ravel-sync` hook calls. All logic
//! lives in `ravel::sync` (the app package, per the hook-authoring guide);
//! this bin only parses args and prints the one-line summary.
//!
//! Usage: ravel-sync <soul-repo> [sessions-dir]
//!   sessions-dir defaults to this repo's Claude Code dir
//!   (~/.claude/projects/<slug>).

use anyhow::{Context, Result};
use ravel::sync;
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let repo: PathBuf = args
        .next()
        .context("usage: ravel-sync <soul-repo> [sessions-dir]")?
        .into();
    let src: Option<PathBuf> = args.next().map(Into::into);

    let stats = sync::sync_soul(&repo, src.as_deref())?;
    println!(
        "[ravel-sync] mirrored {} (unchanged {}) → {} session(s), {} turns, {} emojikeys in {}",
        stats.mirrored,
        stats.unchanged,
        stats.sessions,
        stats.turns,
        stats.keys,
        repo.join(sync::STORE_SUBDIR).display(),
    );
    Ok(())
}
