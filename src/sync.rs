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

/// How one mirrored session file relates to its source. `Growing` is the
/// by-design case (a live session's source outruns the mirror until
/// SessionEnd fires); `Stale` is the silent failure the diagnostic exists to
/// catch (source changed long ago and no hook caught up).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MirrorState {
    Current,
    Growing,
    Stale,
    Missing,
}

/// A source file that still changed within this window is treated as a live
/// session (`Growing`); older divergence is `Stale`.
pub const LIVE_SESSION_WINDOW_SECS: u64 = 30 * 60;

pub fn classify_mirror_file(
    src_len: u64,
    mirror_len: Option<u64>,
    src_age_secs: u64,
) -> MirrorState {
    match mirror_len {
        Some(m) if m == src_len => MirrorState::Current,
        None if src_age_secs <= LIVE_SESSION_WINDOW_SECS => MirrorState::Growing,
        None => MirrorState::Missing,
        Some(_) if src_age_secs <= LIVE_SESSION_WINDOW_SECS => MirrorState::Growing,
        Some(_) => MirrorState::Stale,
    }
}

#[derive(Debug, Default)]
pub struct SyncStats {
    pub mirrored: usize,
    pub unchanged: usize,
    pub sessions: usize,
    pub turns: usize,
    pub keys: usize,
    /// Mirror files skipped at ingest because the manifest shows them
    /// unchanged since their last successful ingest.
    pub skipped: usize,
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

/// Copy every top-level `*.jsonl` in `src` into `dst` when missing or grown.
/// A session JSONL only ever grows, so a SMALLER source is a stale snapshot
/// (e.g. a frozen Raw/ archive of a session the mirror holds in fuller form)
/// — never let it regress the mirror; warn and keep the fuller copy.
/// Returns (copied, unchanged).
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
        match std::fs::metadata(&to).map(|m| m.len()) {
            Ok(dst_len) if dst_len == src_len => unchanged += 1,
            Ok(dst_len) if dst_len > src_len => {
                eprintln!(
                    "[ravel-sync] REFUSING to shrink mirror of {} ({dst_len} → {src_len} bytes): source looks like a stale snapshot; mirror keeps the fuller copy",
                    name.to_string_lossy()
                );
                unchanged += 1;
            }
            _ => {
                std::fs::copy(&path, &to)
                    .with_context(|| format!("copy {} → {}", path.display(), to.display()))?;
                copied += 1;
            }
        }
    }
    Ok((copied, unchanged))
}

/// Per-transcript ingest bookkeeping: `<filename>\t<bytes>` per line, living
/// in the pocket beside the store. Sync state, not graph data — the store
/// stays a pure projection of the bytes, and no ontology term is spent on
/// operational bookkeeping.
const INGEST_MANIFEST: &str = ".ravel/_ignore/ingest-manifest.tsv";

fn read_manifest(repo: &Path) -> std::collections::HashMap<String, u64> {
    std::fs::read_to_string(repo.join(INGEST_MANIFEST))
        .map(|s| {
            s.lines()
                .filter_map(|l| {
                    let (name, len) = l.split_once('\t')?;
                    Some((name.to_string(), len.parse().ok()?))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn write_manifest(repo: &Path, m: &std::collections::HashMap<String, u64>) -> Result<()> {
    let mut lines: Vec<String> = m.iter().map(|(k, v)| format!("{k}\t{v}")).collect();
    lines.sort();
    let tmp = repo.join(INGEST_MANIFEST).with_extension("tsv.tmp");
    std::fs::write(&tmp, lines.join("\n") + "\n")?;
    std::fs::rename(&tmp, repo.join(INGEST_MANIFEST))?;
    Ok(())
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
    let mut manifest = read_manifest(repo);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&mirror)
        .with_context(|| format!("read mirror {}", mirror.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect();
    paths.sort();
    for path in &paths {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        // Unchanged since its last successful ingest → the graph already
        // holds this projection; skip the parse. Makes a large backfilled
        // mirror cost nothing at session end.
        if manifest.get(&name) == Some(&len) {
            stats.skipped += 1;
            continue;
        }
        let jsonl = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("skip {}: {e}", path.display());
                continue;
            }
        };
        let events = adapter::parse_transcript(&jsonl)?;
        if events.is_empty() {
            manifest.insert(name, len);
            continue;
        }
        let anns = reader::emojikey_read(&events);
        let transcript_id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("transcript");
        graph::ingest_transcript(&store, &events, &anns, transcript_id, soul::TURN_PARTITION)?;
        manifest.insert(name, len);
        stats.sessions += 1;
        stats.turns += events.len();
        stats.keys += anns.len();
    }
    write_manifest(repo, &manifest)?;
    Ok(stats)
}

/// One diagnostic finding: severity tag + human line. "attention" findings
/// make the diagnostic exit non-zero; "ok"/"expected" never do.
#[derive(Debug)]
pub struct DiagFinding {
    pub attention: bool,
    pub line: String,
}

/// `ravel-sync --diagnostic` — the ONE diagnostic switch (Rob's rule: every
/// tool gets exactly one, spelled `--diagnostic`). STRICTLY READ-ONLY: it
/// reports a legacy layout, it never migrates one; it opens the store
/// read-only; it creates nothing. Healthy output is short and ends in a
/// one-line verdict; every problem is named with its fix.
pub fn diagnose(repo: &Path, sessions_src: Option<&Path>) -> Result<Vec<DiagFinding>> {
    let mut out: Vec<DiagFinding> = Vec::new();
    let mut push = |attention: bool, line: String| out.push(DiagFinding { attention, line });

    // Layout — pocket law (2026-08-05).
    let legacy: Vec<&str> = LEGACY_DIRS
        .iter()
        .filter(|(old, _)| repo.join(old).exists())
        .map(|(old, _)| *old)
        .collect();
    if legacy.is_empty() {
        push(false, "layout: pocket (.ravel/_ignore/) — OK".into());
    } else {
        push(
            true,
            format!(
                "layout: PRE-POCKET paths present ({}) — run `ravel-sync {}` to migrate",
                legacy.join(", "),
                repo.display()
            ),
        );
    }

    // Kit + hook — is the backup wired to fire at all?
    let kit_reg = std::fs::read_to_string(repo.join(".lex/repo.yml"))
        .map(|s| s.contains("git-lex-kit-ravel"))
        .unwrap_or(false);
    let hook = repo.join(".claude/hooks/SessionEnd-ravel-ravelsync.sh").is_file();
    match (kit_reg, hook) {
        (true, true) => push(false, "kit: registered; hook: installed — OK".into()),
        (_, false) => push(
            true,
            "hook: SessionEnd-ravel-ravelsync.sh MISSING — backups will not run on session end \
             (install: `git lex kit-add repolex-ai/git-lex-kit-ravel`, then kit-update)"
                .into(),
        ),
        (false, true) => push(
            false,
            "kit: not in .lex/repo.yml optional_kits (hook present — manual install?)".into(),
        ),
    }

    // Mirror vs source.
    let src = match sessions_src {
        Some(p) => p.to_path_buf(),
        None => claude_sessions_dir_for(repo)?,
    };
    let mirror = repo.join(TRANSCRIPTS_SUBDIR);
    let now = std::time::SystemTime::now();
    let (mut current, mut growing, mut stale, mut missing, mut src_n) = (0u32, 0u32, 0u32, 0u32, 0u32);
    if src.is_dir() {
        for entry in std::fs::read_dir(&src)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            src_n += 1;
            let meta = std::fs::metadata(&path)?;
            let age = now
                .duration_since(meta.modified()?)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let mirror_len = path
                .file_name()
                .map(|n| mirror.join(n))
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| m.len());
            match classify_mirror_file(meta.len(), mirror_len, age) {
                MirrorState::Current => current += 1,
                MirrorState::Growing => growing += 1,
                MirrorState::Stale => stale += 1,
                MirrorState::Missing => missing += 1,
            }
        }
        let summary = format!(
            "mirror: {src_n} source session(s) → {current} current, {growing} growing (live, catches up at session end), {stale} stale, {missing} missing"
        );
        if stale > 0 || missing > 0 {
            push(true, format!("{summary} — stale/missing with no live session means the hook did not fire; run `ravel-sync {}` and check the hook", repo.display()));
        } else {
            push(false, summary);
        }
    } else if mirror.is_dir() {
        push(
            false,
            format!("mirror: source dir {} absent; mirror exists (archived soul?) — verify the slug if this soul is active", src.display()),
        );
    } else {
        push(
            true,
            format!("mirror: NO source dir ({}) and NO mirror — nothing has ever been backed up here", src.display()),
        );
    }

    // Store — read-only look, never create.
    let store_dir = repo.join(STORE_SUBDIR);
    if !store_dir.join("CURRENT").exists() {
        push(
            true,
            format!(
                "store: {} is not an oxigraph store (no CURRENT) — mirror exists but nothing is ingested; run `ravel-sync {}`",
                store_dir.display(),
                repo.display()
            ),
        );
    } else {
        let store = graph::open_read_only(&store_dir)?;
        let count = |q: &str| -> Result<String> {
            use oxigraph::model::Term;
            use oxigraph::sparql::QueryResults;
            if let QueryResults::Solutions(mut sols) = store.query(q)? {
                if let Some(s) = sols.next() {
                    if let Some((_, t)) = s?.iter().next() {
                        // lexical form only — "11994", not "11994"^^xsd:integer
                        return Ok(match t {
                            Term::Literal(l) => l.value().to_string(),
                            other => other.to_string(),
                        });
                    }
                }
            }
            Ok("0".into())
        };
        let turns = count(
            "SELECT (COUNT(?s) AS ?n) WHERE { GRAPH ?g { ?s <https://repolex.ai/ontology/ravel/turnId> ?id } }",
        )?;
        let graphs = count("SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }")?;
        let latest = count(
            "SELECT ?t WHERE { GRAPH ?g { ?s <https://repolex.ai/ontology/ravel/timestamp> ?t } } ORDER BY DESC(?t) LIMIT 1",
        )?;
        push(
            false,
            format!("store: {graphs} transcript graph(s), {turns} turns, latest turn {latest}"),
        );
    }

    Ok(out)
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

    /// A smaller source (stale snapshot) must never regress a fuller mirror
    /// copy; a larger one still re-mirrors.
    #[test]
    fn mirror_never_shrinks() {
        let base = std::env::temp_dir().join("ravel-sync-test-noshrink");
        let (src, dst) = (base.join("src"), base.join("dst"));
        fs::remove_dir_all(&base).ok();
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::write(src.join("a.jsonl"), "old-snapshot\n").unwrap();
        fs::write(dst.join("a.jsonl"), "the fuller, newer mirror copy\n").unwrap();

        let (copied, unchanged) = mirror_jsonl(&src, &dst).unwrap();
        assert_eq!((copied, unchanged), (0, 1), "stale snapshot must not copy");
        assert_eq!(
            fs::read_to_string(dst.join("a.jsonl")).unwrap(),
            "the fuller, newer mirror copy\n"
        );
        fs::remove_dir_all(&base).ok();
    }

    /// Manifest round-trip: what write_manifest stores, read_manifest returns.
    #[test]
    fn ingest_manifest_round_trips() {
        let repo = std::env::temp_dir().join("ravel-sync-test-manifest");
        fs::remove_dir_all(&repo).ok();
        fs::create_dir_all(repo.join(".ravel/_ignore")).unwrap();
        let mut m = std::collections::HashMap::new();
        m.insert("a.jsonl".to_string(), 42u64);
        m.insert("b.jsonl".to_string(), 991_000_000u64);
        write_manifest(&repo, &m).unwrap();
        assert_eq!(read_manifest(&repo), m);
        assert_eq!(read_manifest(&std::env::temp_dir().join("no-such-repo")).len(), 0);
        fs::remove_dir_all(&repo).ok();
    }

    /// The mirror-state classifier: same-size is current regardless of age;
    /// divergence within the live window is a growing session (healthy);
    /// old divergence or absence is the hook having failed to fire.
    #[test]
    fn mirror_state_classification() {
        let w = LIVE_SESSION_WINDOW_SECS;
        assert_eq!(classify_mirror_file(100, Some(100), w * 10), MirrorState::Current);
        assert_eq!(classify_mirror_file(100, Some(100), 0), MirrorState::Current);
        assert_eq!(classify_mirror_file(200, Some(100), w / 2), MirrorState::Growing);
        assert_eq!(classify_mirror_file(200, None, w / 2), MirrorState::Growing);
        assert_eq!(classify_mirror_file(200, Some(100), w + 1), MirrorState::Stale);
        assert_eq!(classify_mirror_file(200, None, w + 1), MirrorState::Missing);
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
