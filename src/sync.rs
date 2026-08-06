//! Soul-sync — the ONE blessed entrypoint that brings a soul's ravel up to
//! date, called by the `SessionEnd-ravel-sync` hook (and usable by hand).
//!
//! Two stages, both idempotent:
//!
//! 1. **Mirror** — copy new/changed session `.jsonl` files from the harness's
//!    session dir into the soul repo's transcript mirror. The mirror is
//!    dialect-scoped so other harnesses can land as siblings later:
//!
//!      `<repo>/.ravel/_ignore/transcripts/claude-code/<session>.jsonl`
//!
//!    Bytes live WITH the soul (Rob's one-folder-per-agent rule: copying the
//!    soul folder copies the conversations), and the mirror — not the live
//!    harness dir — is what gets ingested, so the graph and the bytes it was
//!    derived from can never drift apart.
//!
//! 2. **Ingest** — project the mirror into `<repo>/.ravel/_ignore/oxigraph`.
//!    Turn-keyed IRIs + per-transcript named-graph replace make re-runs cheap
//!    and safe (only new turns add triples).
//!
//! Layout follows the stack-wide `_ignore/` pocket law (Rob, 2026-08-05:
//! subtexture docs/stack/2026_08_05_DOTDIR_IGNORE_POCKET.md): inside `.ravel/`,
//! `_ignore/` holds all machine-local rebuildable-or-relocatable state and is
//! gitignored; everything else in `.ravel/` is committable. Pre-pocket installs
//! are migrated automatically by [`migrate_legacy_layout`] on every sync.
//!
//! This module owns the `.ravel/` layout; nothing else may hardcode it.

use crate::{adapter, graph, reader, soul};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The dialect-scoped transcript mirror, relative to the soul repo root.
/// Other harnesses land as siblings of `claude-code/`.
pub const TRANSCRIPTS_SUBDIR: &str = ".ravel/_ignore/transcripts/claude-code";

/// The graph store, relative to the soul repo root.
pub const STORE_SUBDIR: &str = ".ravel/_ignore/oxigraph";

/// Pre-pocket (≤2026-08-05) locations of the two machine-local trees. This is
/// also git-lex's per-engine legacy list: its managed-gitignore emitter keeps
/// the whole-dir `.ravel/` entry until neither of these exists, then narrows
/// to `.ravel/_ignore/`. Transitional — dies with the fleet conversion.
const LEGACY_DIRS: [(&str, &str); 2] = [
    (".ravel/oxigraph", ".ravel/_ignore/oxigraph"),
    (".ravel/transcripts", ".ravel/_ignore/transcripts"),
];

/// Move pre-pocket trees into `.ravel/_ignore/`. Returns how many moved.
/// Fails loud when BOTH layouts hold the same tree — that's an ambiguity a
/// rename must not silently resolve (which copy is authoritative?).
pub fn migrate_legacy_layout(repo: &Path) -> Result<usize> {
    let mut moved = 0usize;
    for (old_rel, new_rel) in LEGACY_DIRS {
        let (old, new) = (repo.join(old_rel), repo.join(new_rel));
        if !old.exists() {
            continue;
        }
        if new.exists() {
            anyhow::bail!(
                "both {} and {} exist — refusing to migrate over live data; \
                 merge or remove one by hand, then re-run",
                old.display(),
                new.display()
            );
        }
        if let Some(parent) = new.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create pocket dir {}", parent.display()))?;
        }
        std::fs::rename(&old, &new)
            .with_context(|| format!("move {} → {}", old.display(), new.display()))?;
        eprintln!("[ravel-sync] migrated {} → {}", old.display(), new.display());
        moved += 1;
    }
    Ok(moved)
}

#[derive(Debug, Default)]
pub struct SyncStats {
    pub mirrored: usize,
    pub unchanged: usize,
    pub sessions: usize,
    pub turns: usize,
    pub keys: usize,
}

/// Where Claude Code keeps this soul repo's session logs:
/// `~/.claude/projects/<slug>`, slug = the absolute repo path with `/`→`-`.
/// (Claude-code dialect knowledge — that's why it lives here, not in a hook.)
pub fn claude_sessions_dir_for(repo: &Path) -> Result<PathBuf> {
    let abs = repo
        .canonicalize()
        .with_context(|| format!("canonicalize soul repo {}", repo.display()))?;
    let slug = abs.to_string_lossy().replace('/', "-");
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".claude").join("projects").join(slug))
}

/// Copy every top-level `*.jsonl` in `src` into `dst` when missing or changed
/// (size differs — a session JSONL only ever grows). Returns (copied, unchanged).
fn mirror_jsonl(src: &Path, dst: &Path) -> Result<(usize, usize)> {
    std::fs::create_dir_all(dst)
        .with_context(|| format!("create mirror dir {}", dst.display()))?;
    let (mut copied, mut unchanged) = (0usize, 0usize);
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(name) = path.file_name() else { continue };
        let to = dst.join(name);
        let src_len = std::fs::metadata(&path)?.len();
        let same = std::fs::metadata(&to).map(|m| m.len() == src_len).unwrap_or(false);
        if same {
            unchanged += 1;
        } else {
            std::fs::copy(&path, &to)
                .with_context(|| format!("copy {} → {}", path.display(), to.display()))?;
            copied += 1;
        }
    }
    Ok((copied, unchanged))
}

/// Bring one soul's ravel fully up to date: mirror then ingest-the-mirror.
/// `sessions_src` defaults to the Claude Code dir for this repo.
pub fn sync_soul(repo: &Path, sessions_src: Option<&Path>) -> Result<SyncStats> {
    migrate_legacy_layout(repo)?;
    let src = match sessions_src {
        Some(p) => p.to_path_buf(),
        None => claude_sessions_dir_for(repo)?,
    };
    let mirror = repo.join(TRANSCRIPTS_SUBDIR);
    let mut stats = SyncStats::default();
    if src.is_dir() {
        let (copied, unchanged) = mirror_jsonl(&src, &mirror)?;
        stats.mirrored = copied;
        stats.unchanged = unchanged;
    } else if !mirror.is_dir() {
        anyhow::bail!(
            "no sessions to sync: {} missing and no existing mirror at {}",
            src.display(),
            mirror.display()
        );
    }

    let store = graph::open(repo.join(STORE_SUBDIR))?;
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&mirror)
        .with_context(|| format!("read mirror {}", mirror.display()))?
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
        if events.is_empty() {
            continue;
        }
        let anns = reader::emojikey_read(&events);
        let transcript_id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("transcript");
        graph::ingest_transcript(&store, &events, &anns, transcript_id, soul::TURN_PARTITION)?;
        stats.sessions += 1;
        stats.turns += events.len();
        stats.keys += anns.len();
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// New files copy; same-size files skip; grown files re-copy.
    #[test]
    fn mirror_copies_new_and_grown_skips_unchanged() {
        let base = std::env::temp_dir().join("ravel-sync-test-mirror");
        let (src, dst) = (base.join("src"), base.join("dst"));
        fs::remove_dir_all(&base).ok();
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.jsonl"), "line1\n").unwrap();
        fs::write(src.join("noise.txt"), "ignored").unwrap();

        let (copied, unchanged) = mirror_jsonl(&src, &dst).unwrap();
        assert_eq!((copied, unchanged), (1, 0), "first pass copies the jsonl only");
        assert!(dst.join("a.jsonl").exists());
        assert!(!dst.join("noise.txt").exists(), "non-jsonl never mirrors");

        let (copied, unchanged) = mirror_jsonl(&src, &dst).unwrap();
        assert_eq!((copied, unchanged), (0, 1), "second pass is a no-op");

        fs::write(src.join("a.jsonl"), "line1\nline2\n").unwrap();
        let (copied, _) = mirror_jsonl(&src, &dst).unwrap();
        assert_eq!(copied, 1, "a grown session re-mirrors");
        assert_eq!(fs::read_to_string(dst.join("a.jsonl")).unwrap(), "line1\nline2\n");

        fs::remove_dir_all(&base).ok();
    }

    /// Legacy trees move into the pocket once; a second run is a no-op; a
    /// pocket-born repo is untouched.
    #[test]
    fn legacy_layout_migrates_once_then_noop() {
        let repo = std::env::temp_dir().join("ravel-sync-test-migrate");
        fs::remove_dir_all(&repo).ok();
        fs::create_dir_all(repo.join(".ravel/oxigraph")).unwrap();
        fs::create_dir_all(repo.join(".ravel/transcripts/claude-code")).unwrap();
        fs::write(repo.join(".ravel/oxigraph/CURRENT"), "x").unwrap();
        fs::write(repo.join(".ravel/transcripts/claude-code/a.jsonl"), "l\n").unwrap();

        assert_eq!(migrate_legacy_layout(&repo).unwrap(), 2, "both trees move");
        assert!(repo.join(".ravel/_ignore/oxigraph/CURRENT").exists());
        assert!(repo.join(TRANSCRIPTS_SUBDIR).join("a.jsonl").exists());
        assert!(!repo.join(".ravel/oxigraph").exists(), "legacy path is gone");
        assert!(!repo.join(".ravel/transcripts").exists(), "legacy path is gone");

        assert_eq!(migrate_legacy_layout(&repo).unwrap(), 0, "second run is a no-op");
        fs::remove_dir_all(&repo).ok();
    }

    /// Both layouts holding the same tree is ambiguous — refuse, loudly.
    #[test]
    fn migration_refuses_ambiguous_dual_layout() {
        let repo = std::env::temp_dir().join("ravel-sync-test-migrate-dual");
        fs::remove_dir_all(&repo).ok();
        fs::create_dir_all(repo.join(".ravel/oxigraph")).unwrap();
        fs::create_dir_all(repo.join(".ravel/_ignore/oxigraph")).unwrap();

        let err = migrate_legacy_layout(&repo).unwrap_err().to_string();
        assert!(err.contains("refusing to migrate"), "got: {err}");
        assert!(repo.join(".ravel/oxigraph").exists(), "nothing was touched");
        fs::remove_dir_all(&repo).ok();
    }

    /// The slug is the absolute repo path with slashes flattened to dashes.
    #[test]
    fn sessions_dir_slug_matches_claude_code_convention() {
        let repo = std::env::temp_dir().join("ravel-sync-test-slug");
        fs::create_dir_all(&repo).unwrap();
        let dir = claude_sessions_dir_for(&repo).unwrap();
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with('-'), "abs path starts with / → slug starts with -");
        assert!(name.contains("ravel-sync-test-slug"));
        assert!(!name.contains('/'));
        fs::remove_dir_all(&repo).ok();
    }
}
