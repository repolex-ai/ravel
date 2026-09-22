//! Soul-sync — the ONE blessed entrypoint that brings a soul's ravel up to
//! date. raveld runs it on its schedule; `ravel sync` runs it now.
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

use crate::{adapter, agy, graph, reader, soul};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The dialect-scoped transcript mirror, relative to the soul repo root.
/// Other harnesses land as siblings of `claude-code/`.
pub const TRANSCRIPTS_SUBDIR: &str = ".ravel/_ignore/transcripts/claude-code";

/// The Gemini/Antigravity transcript mirror — sibling of `claude-code/`, named
/// for the substrate's kind as herdr reports it (Rob, 2026-08-26). The
/// per-dialect shelf has existed since before a second dialect did; this is its
/// first use.
pub const AGY_TRANSCRIPTS_SUBDIR: &str = ".ravel/_ignore/transcripts/agy";

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
        eprintln!("[raveld] migrated {} → {}", old.display(), new.display());
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
    /// A live session whose mirror has been behind for longer than
    /// [`GROWING_DIVERGENCE_LIMIT_SECS`]. Still "growing" in mechanism, but no
    /// longer healthy: see the doc comment on that constant for the incident
    /// that put this variant here.
    GrowingLong,
    Stale,
    Missing,
}

/// A source file that still changed within this window is treated as a live
/// session (`Growing`); older divergence is `Stale`.
pub const LIVE_SESSION_WINDOW_SECS: u64 = 30 * 60;

/// How long a live session's mirror may lag before `Growing` stops counting as
/// healthy.
///
/// **Why this exists.** `Growing` used to have no duration. It asked "is the
/// source still being written?" and never "for how long has the mirror been
/// behind?", so a 20-minute-old live session and a session left open for three
/// weeks produced identical green output. On 2026-08-26 that printed OK across
/// four souls while 101.4 MB of conversation existed in exactly one place on
/// disk. Every component did what it was built to do; the check forgot to ask
/// one question.
///
/// Three days is chosen to sit well clear of a long working session (souls here
/// run day-long ones routinely) while catching the real failure — a session
/// nobody has ended, whose SessionEnd hook therefore has not fired since it
/// opened. The backup is not broken; it is deferred indefinitely, which for a
/// custody engine is the same thing said politely.
pub const GROWING_DIVERGENCE_LIMIT_SECS: u64 = 3 * 24 * 60 * 60;

/// Classify one source file against its mirror.
///
/// `divergence_age_secs` answers "how long has the mirror been out of date?" —
/// the question the original classifier never asked. It is the mirror file's
/// own age when a mirror exists (it was last correct when it was written), and
/// the source's age since creation when no mirror exists (it has never been
/// correct). `None` when the filesystem could not supply it, which downgrades
/// to the old duration-blind behaviour rather than inventing a number.
pub fn classify_mirror_file(
    src_len: u64,
    mirror_len: Option<u64>,
    src_age_secs: u64,
    divergence_age_secs: Option<u64>,
) -> MirrorState {
    let live = src_age_secs <= LIVE_SESSION_WINDOW_SECS;
    let lagging_long = divergence_age_secs
        .map(|d| d > GROWING_DIVERGENCE_LIMIT_SECS)
        .unwrap_or(false);
    match mirror_len {
        Some(m) if m == src_len => MirrorState::Current,
        None if live && lagging_long => MirrorState::GrowingLong,
        None if live => MirrorState::Growing,
        None => MirrorState::Missing,
        Some(_) if live && lagging_long => MirrorState::GrowingLong,
        Some(_) if live => MirrorState::Growing,
        Some(_) => MirrorState::Stale,
    }
}

/// Seconds since a `SystemTime`, saturating at zero for clock skew.
fn secs_since(t: std::time::SystemTime, now: std::time::SystemTime) -> u64 {
    now.duration_since(t).map(|d| d.as_secs()).unwrap_or(0)
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
    /// The Gemini/Antigravity half, counted separately so a green claude-side
    /// number can never stand in for an agy-side one.
    pub agy: AgyStats,
}

/// What the agy pass did. `unattributed` is the load-bearing field: a
/// conversation on disk that no map ties to a repo is NOT backed up, and a
/// summary that omits it would be another instrument printing OK over missing
/// data.
#[derive(Debug, Default)]
pub struct AgyStats {
    pub mirrored: usize,
    pub unchanged: usize,
    pub sessions: usize,
    pub turns: usize,
    pub skipped: usize,
    /// Conversations whose transcripts exist but whose owning workspace could
    /// not be determined; reported by id, never silently skipped.
    pub unattributed: Vec<String>,
    /// Conversations ingested from a `transcript.jsonl` that declares
    /// truncation with no `transcript_full.jsonl` beside it — the only copy is
    /// lossy and ravel says so.
    pub lossy: Vec<String>,
    /// Total `step_index` holes recorded across this soul's conversations.
    /// Expected to be roughly one per conversation; a spike means something else.
    pub gaps: usize,
    /// Conversations this pass could not mirror or ingest, as `id: reason`.
    /// One bad conversation is reported and skipped; the rest of the soul is
    /// still synced. Refusing is right; refusing everything is not.
    pub failed: Vec<String>,
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
    Ok(PathBuf::from(home)
        .join(".claude")
        .join("projects")
        .join(slug))
}

/// Copy every top-level `*.jsonl` in `src` into `dst` when missing or grown.
/// A session JSONL only ever grows, so a SMALLER source is a stale snapshot
/// (e.g. a frozen Raw/ archive of a session the mirror holds in fuller form)
/// — never let it regress the mirror; warn and keep the fuller copy.
/// Returns (copied, unchanged).
fn mirror_jsonl(src: &Path, dst: &Path) -> Result<(usize, usize)> {
    std::fs::create_dir_all(dst).with_context(|| format!("create mirror dir {}", dst.display()))?;
    let (mut copied, mut unchanged) = (0usize, 0usize);
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        let to = dst.join(name);
        let src_len = std::fs::metadata(&path)?.len();
        match std::fs::metadata(&to).map(|m| m.len()) {
            Ok(dst_len) if dst_len == src_len => unchanged += 1,
            Ok(dst_len) if dst_len > src_len => {
                eprintln!(
                    "[raveld] REFUSING to shrink mirror of {} ({dst_len} → {src_len} bytes): source looks like a stale snapshot; mirror keeps the fuller copy",
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

// ─────────────────────────── Gemini / Antigravity ───────────────────────────
//
// The agy half of sync. Structurally the same two stages as the claude-code
// half — mirror the bytes into the soul, then project the mirror — but three
// things differ, each for a measured reason:
//
//   1. **Attribution is a lookup, not a path slug.** Claude Code files sessions
//      in a directory named after the repo path. Antigravity files them by
//      conversation UUID under one flat store, so which soul a conversation
//      belongs to must be read from `history.jsonl`.
//   2. **Change detection is by CONTENT HASH, not size.** The client rewrites a
//      step's line in place (`RUNNING` → `DONE`) rather than appending a
//      correction, so bytes change without the file growing. Size comparison —
//      correct for an append-only claude log — is unsound here.
//   3. **No anti-shrink guard.** That guard exists because a frozen archive can
//      be smaller than a live mirror. An in-place rewrite may legitimately
//      shrink the file, and the agy source is always the live store rather than
//      an archive, so the guard would block correct updates and catch nothing.

/// The Antigravity CLI store root.
pub fn agy_store_root() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".gemini").join("antigravity-cli"))
}

/// One conversation's transcript, resolved to the file ravel will actually read.
#[derive(Debug, Clone)]
pub struct AgyConversation {
    pub id: String,
    /// `transcript_full.jsonl` when it exists, else `transcript.jsonl`.
    pub source: PathBuf,
    /// True when the chosen source is a `transcript.jsonl` with no full
    /// counterpart. Whether that is actually lossy is only known after parsing
    /// (it depends on `truncated_fields` markers), so this only flags the
    /// *possibility* — [`sync_agy`] resolves it against the parse.
    pub short_only: bool,
}

/// Resolve the transcript file for one conversation, preferring the untruncated
/// copy. Returns `None` when the conversation folder holds neither.
pub fn agy_transcript_for(brain: &Path, conversation_id: &str) -> Option<AgyConversation> {
    let logs = brain
        .join(conversation_id)
        .join(".system_generated")
        .join("logs");
    let full = logs.join("transcript_full.jsonl");
    if full.is_file() {
        return Some(AgyConversation {
            id: conversation_id.to_string(),
            source: full,
            short_only: false,
        });
    }
    let short = logs.join("transcript.jsonl");
    short.is_file().then(|| AgyConversation {
        id: conversation_id.to_string(),
        source: short,
        short_only: true,
    })
}

/// Which conversations belong to which workspace, read from the CLI's
/// `history.jsonl` — one JSON object per user prompt carrying `workspace` and
/// `conversationId`. This is the agy analogue of Claude Code's path slug, and
/// unlike `conversation_summaries.db` (which stopped being written around
/// 2026-06-18) it is current.
///
/// Returns `workspace path → conversation ids`, in first-seen order.
pub fn agy_workspace_map(store_root: &Path) -> Result<Vec<(String, Vec<String>)>> {
    let path = store_root.join("history.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::HashMap<String, Vec<String>> = Default::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(o) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let (Some(ws), Some(cid)) = (
            o.get("workspace").and_then(|v| v.as_str()),
            o.get("conversationId").and_then(|v| v.as_str()),
        ) else {
            continue; // a prompt logged before a conversation existed
        };
        let entry = map.entry(ws.to_string()).or_insert_with(|| {
            order.push(ws.to_string());
            Vec::new()
        });
        if !entry.iter().any(|c| c == cid) {
            entry.push(cid.to_string());
        }
    }
    Ok(order
        .into_iter()
        .map(|ws| {
            let ids = map.remove(&ws).unwrap_or_default();
            (ws, ids)
        })
        .collect())
}

/// The conversations belonging to `repo`, plus the ones on disk that no map
/// could place.
///
/// The unattributed list is deliberately part of the return value rather than a
/// silent filter. `history.jsonl` only records conversations that logged a user
/// prompt through it; at least one conversation on this machine (June-era)
/// predates that and appears nowhere in it. Such a conversation is NOT backed
/// up, and the whole point of today's work is that an instrument must not print
/// a clean number over data it never counted.
///
/// KNOWN GAP, stated rather than papered over: `conversation_summaries.db` does
/// carry `workspace_uris` for exactly the conversations `history.jsonl` misses.
/// Reading it needs a SQLite dependency in a binary that is installed once and
/// shared by every soul on the machine, so that is a deliberate follow-up
/// decision, not an oversight. Until then the ids are reported and the fix is
/// one command, named in the message.
pub fn agy_conversations_for(repo: &Path) -> Result<(Vec<AgyConversation>, Vec<String>)> {
    let root = agy_store_root()?;
    let brain = root.join("brain");
    if !brain.is_dir() {
        return Ok((Vec::new(), Vec::new()));
    }
    let abs = repo
        .canonicalize()
        .with_context(|| format!("canonicalize soul repo {}", repo.display()))?;

    let map = agy_workspace_map(&root)?;
    let mut mine: Vec<AgyConversation> = Vec::new();
    let mut placed: std::collections::HashSet<String> = Default::default();
    for (ws, ids) in &map {
        let same = Path::new(ws)
            .canonicalize()
            .map(|p| p == abs)
            .unwrap_or(false);
        for id in ids {
            placed.insert(id.clone());
            if same {
                if let Some(c) = agy_transcript_for(&brain, id) {
                    mine.push(c);
                }
            }
        }
    }

    let mut unattributed: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&brain).with_context(|| format!("read {}", brain.display()))? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !placed.contains(id) && agy_transcript_for(&brain, id).is_some() {
            unattributed.push(id.to_string());
        }
    }
    mine.sort_by(|a, b| a.id.cmp(&b.id));
    unattributed.sort();
    Ok((mine, unattributed))
}

/// Hex sha256 of a file's bytes — the agy shelf's change detector.
fn file_sha256(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// Per-conversation ingest bookkeeping for the agy shelf: `<id>\t<sha256>`.
/// A SEPARATE file from the claude-code manifest on purpose — that one stores a
/// byte length, this one a content hash, and one file holding two silently
/// different meanings in the same column is exactly the kind of ambiguity that
/// reads fine until it doesn't.
const AGY_MANIFEST: &str = ".ravel/_ignore/ingest-manifest-agy.tsv";

fn read_agy_manifest(repo: &Path) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(repo.join(AGY_MANIFEST))
        .map(|s| {
            s.lines()
                .filter_map(|l| {
                    let (name, hash) = l.split_once('\t')?;
                    Some((name.to_string(), hash.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn write_agy_manifest(repo: &Path, m: &std::collections::HashMap<String, String>) -> Result<()> {
    let mut lines: Vec<String> = m.iter().map(|(k, v)| format!("{k}\t{v}")).collect();
    lines.sort();
    let tmp = repo.join(AGY_MANIFEST).with_extension("tsv.tmp");
    std::fs::write(&tmp, lines.join("\n") + "\n")?;
    std::fs::rename(&tmp, repo.join(AGY_MANIFEST))?;
    Ok(())
}

/// Mirror and ingest this soul's Gemini/Antigravity conversations.
///
/// A clean no-op when the CLI is not installed or no conversation belongs to
/// this repo — most souls on this machine are claude-only and must not pay for
/// this pass.
pub fn sync_agy(repo: &Path) -> Result<AgyStats> {
    let mut st = AgyStats::default();
    let (convs, unattributed) = agy_conversations_for(repo)?;
    st.unattributed = unattributed;
    if !st.unattributed.is_empty() {
        eprintln!(
            "[raveld] {} agy conversation(s) on disk are attributed to NO workspace and are therefore NOT backed up: {}\n\
             [raveld]   history.jsonl does not name them (it predates them, or they logged no prompt through it).\n\
             [raveld]   check the owner by hand: sqlite3 \"file:$HOME/.gemini/antigravity-cli/conversation_summaries.db?mode=ro\" \\\n\
             [raveld]     \"SELECT conversation_id, workspace_uris FROM conversation_summaries;\"",
            st.unattributed.len(),
            st.unattributed.join(", ")
        );
    }
    if convs.is_empty() {
        return Ok(st);
    }

    let mirror = repo.join(AGY_TRANSCRIPTS_SUBDIR);
    std::fs::create_dir_all(&mirror)
        .with_context(|| format!("create agy mirror dir {}", mirror.display()))?;

    let store = graph::open(repo.join(STORE_SUBDIR))?;
    let mut manifest = read_agy_manifest(repo);

    for c in &convs {
        if let Err(e) = sync_agy_one(c, &mirror, &store, &mut manifest, &mut st) {
            eprintln!(
                "[raveld] agy {}: SKIPPED this pass — {e:#}",
                &c.id[..8.min(c.id.len())]
            );
            st.failed.push(format!("{}: {e:#}", c.id));
        }
    }
    write_agy_manifest(repo, &manifest)?;
    Ok(st)
}

/// Mirror and ingest ONE conversation. An error here is that conversation's
/// alone: the caller records it and moves on to the next.
fn sync_agy_one(
    c: &AgyConversation,
    mirror: &Path,
    store: &oxigraph::store::Store,
    manifest: &mut std::collections::HashMap<String, String>,
    st: &mut AgyStats,
) -> Result<()> {
    // MIRROR — by content hash. The source is rewritten in place while a
    // session is live, so equal size proves nothing.
    let to = mirror.join(format!("{}.jsonl", c.id));
    let src_hash = file_sha256(&c.source)?;
    let dst_hash = to.is_file().then(|| file_sha256(&to)).transpose()?;
    if dst_hash.as_deref() == Some(src_hash.as_str()) {
        st.unchanged += 1;
    } else {
        std::fs::copy(&c.source, &to)
            .with_context(|| format!("copy {} → {}", c.source.display(), to.display()))?;
        st.mirrored += 1;
    }

    // INGEST — skip when this exact content is already projected.
    if manifest.get(&c.id) == Some(&src_hash) {
        st.skipped += 1;
        return Ok(());
    }
    let jsonl = std::fs::read_to_string(&to)
        .with_context(|| format!("read agy mirror {}", to.display()))?;
    let parsed = agy::parse_transcript(&jsonl, &c.id)?;
    st.gaps += parsed.gaps.len();
    if parsed.duplicate_rows > 0 {
        eprintln!(
            "[raveld] agy {}: {} byte-identical duplicate row(s) dropped — the client wrote the same step twice",
            &c.id[..8.min(c.id.len())],
            parsed.duplicate_rows
        );
    }
    if !parsed.gaps.is_empty() {
        eprintln!(
            "[raveld] agy {}: step_index gap(s) at {:?} — the spine is left DISCONNECTED there rather than bridged (expected: the client burns an index on an interrupted step)",
            &c.id[..8.min(c.id.len())],
            parsed.gaps
        );
    }
    if c.short_only && parsed.truncated_rows > 0 {
        st.lossy.push(c.id.clone());
        eprintln!(
            "[raveld] agy {}: LOSSY SOURCE — {} row(s) declare truncated_fields and there is no transcript_full.jsonl beside them; ingesting the short copy, which is NOT the whole conversation",
            &c.id[..8.min(c.id.len())],
            parsed.truncated_rows
        );
    }
    if parsed.malformed_lines > 0 {
        eprintln!(
            "[raveld] agy {}: {} unparseable line(s) skipped",
            &c.id[..8.min(c.id.len())],
            parsed.malformed_lines
        );
    }
    if parsed.events.is_empty() {
        manifest.insert(c.id.clone(), src_hash);
        return Ok(());
    }
    let anns = reader::emojikey_read(&parsed.events);
    // Dialect-prefixed graph name: the named graph is the provenance unit,
    // and which substrate a conversation came from is provenance.
    let transcript_id = format!("agy-{}", c.id);
    graph::ingest_transcript(
        store,
        &parsed.events,
        &anns,
        &transcript_id,
        soul::TURN_PARTITION,
    )?;
    manifest.insert(c.id.clone(), src_hash);
    st.sessions += 1;
    st.turns += parsed.events.len();
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
    }

    if mirror.is_dir() {
        let store = graph::open(repo.join(STORE_SUBDIR))?;
        let mut manifest = read_manifest(repo);
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&mirror)
            .with_context(|| format!("read mirror {}", mirror.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
            .collect();
        paths.sort();
        for path in &paths {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
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
            let transcript_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("transcript");
            graph::ingest_transcript(&store, &events, &anns, transcript_id, soul::TURN_PARTITION)?;
            manifest.insert(name, len);
            stats.sessions += 1;
            stats.turns += events.len();
            stats.keys += anns.len();
        }
        write_manifest(repo, &manifest)?;
    }

    // The agy half. Independent of the claude half on purpose: a soul with no
    // Antigravity conversations pays nothing, and a failure on one side must
    // not be able to silently swallow the other's numbers.
    stats.agy = sync_agy(repo)?;
    Ok(stats)
}

/// A slug reduced to its comparable core: lowercase, punctuation dropped.
///
/// Claude Code names a session directory after the repo's absolute path with
/// `/` → `-`, so ANY change to the path spells a different directory. In
/// practice the change that actually happens is cosmetic — `spaceG.O.A.T.`
/// became `spaceGOAT`, `M4RQ` became `m4rq` — and the old directory keeps its
/// sessions forever (Claude's `cleanupPeriodDays` here is 99999, so nothing
/// expires). Normalizing to letters and digits makes those renames comparable
/// without matching two genuinely different repos that merely share a word.
fn slug_core(slug: &str) -> String {
    slug.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// A session directory OTHER than this repo's current one that appears to hold
/// this repo's history.
#[derive(Debug)]
pub struct SiblingSlug {
    pub slug: String,
    pub sessions: usize,
    /// Sessions in that directory with no counterpart in this repo's mirror.
    pub unmirrored: usize,
    pub unmirrored_bytes: u64,
    /// True when at least one of its session files is ALREADY in this mirror —
    /// proof the directory served this repo, not a guess from its name.
    pub proven: bool,
}

/// Find session directories that belong to this repo under a name it no longer
/// uses.
///
/// **The blind spot this closes.** The mirror check counts source sessions in
/// ONE directory — the slug for the repo's current path. Rename the repo and
/// the old directory keeps every session it ever held, outside the counted set:
/// not stale, not missing, simply never looked at. The verdict stays a clean
/// `0 missing` while history sits unbacked-up. That cost spaceGOAT 42.8 MB of
/// April–May conversations, which a full backfill of the current slug swept
/// past without a word.
///
/// Two classes, kept apart on purpose:
///   - **proven** — the directory holds at least one session file this repo's
///     mirror already has. Session ids are unique, so that is evidence, not
///     resemblance.
///   - **suspected** — the directory's slug normalizes to the same core as this
///     repo's ([`slug_core`]). Reported so a human can look, never asserted.
pub fn sibling_slugs_for(repo: &Path) -> Result<Vec<SiblingSlug>> {
    let current = claude_sessions_dir_for(repo)?;
    let Some(projects) = current.parent().map(Path::to_path_buf) else {
        return Ok(Vec::new());
    };
    if !projects.is_dir() {
        return Ok(Vec::new());
    }
    let current_name = current
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let core = slug_core(&current_name);
    let mirror = repo.join(TRANSCRIPTS_SUBDIR);
    let mirrored: std::collections::HashSet<String> = std::fs::read_dir(&mirror)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    for entry in std::fs::read_dir(&projects)? {
        let dir = entry?.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == current_name {
            continue;
        }
        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
                    .collect()
            })
            .unwrap_or_default();
        if files.is_empty() {
            continue;
        }
        let names: Vec<String> = files
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
            .collect();
        // A mirrored file may carry a date prefix from a hand backfill, so
        // match on the session id appearing anywhere in a mirror filename.
        let is_home = |n: &String| mirrored.iter().any(|m| m == n || m.ends_with(n.as_str()));
        let proven = names.iter().any(is_home);
        if !proven && slug_core(name) != core {
            continue;
        }
        let unmirrored: Vec<&PathBuf> = files
            .iter()
            .zip(&names)
            .filter(|(_, n)| !is_home(n))
            .map(|(p, _)| p)
            .collect();
        out.push(SiblingSlug {
            slug: name.to_string(),
            sessions: files.len(),
            unmirrored: unmirrored.len(),
            unmirrored_bytes: unmirrored
                .iter()
                .filter_map(|p| std::fs::metadata(p).ok())
                .map(|m| m.len())
                .sum(),
            proven,
        });
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(out)
}

/// One diagnostic finding: severity tag + human line. "attention" findings
/// make the diagnostic exit non-zero; "ok"/"expected" never do.
#[derive(Debug)]
pub struct DiagFinding {
    pub attention: bool,
    pub line: String,
}

/// `ravel health` — the ONE diagnostic (squad rule: every tool gets exactly
/// one, and it is read-only). STRICTLY READ-ONLY: it
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
                "layout: PRE-POCKET paths present ({}) — run `ravel sync {}` to migrate",
                legacy.join(", "),
                repo.display()
            ),
        );
    }

    // Kit, and the hook it used to ship. raveld is the writer (2026-09-22):
    // the SessionEnd hook is a SECOND writer while a `ravel-sync` binary is
    // still installed beside it, and inert once that binary is gone.
    let kit_reg = std::fs::read_to_string(repo.join(".lex/repo.yml"))
        .map(|s| s.contains("git-lex-kit-ravel"))
        .unwrap_or(false);
    if kit_reg {
        push(false, "kit: git-lex-kit-ravel registered — OK".into());
    } else {
        push(
            true,
            "kit: git-lex-kit-ravel NOT in .lex/repo.yml — the pocket layout and its gitignore come from the kit \
             (`git lex kit-add repolex-ai/git-lex-kit-ravel`, then kit-update)"
                .into(),
        );
    }
    let hook = repo
        .join(".claude/hooks/SessionEnd-ravel-ravelsync.sh")
        .is_file();
    let stale_bin = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".cargo/bin/ravel-sync").is_file())
        .unwrap_or(false);
    match (hook, stale_bin) {
        (true, true) => push(
            true,
            "hook: legacy SessionEnd-ravel-ravelsync.sh present AND ~/.cargo/bin/ravel-sync still installed — \
             two writers to one store; delete ~/.cargo/bin/ravel-sync (raveld syncs on its own)"
                .into(),
        ),
        (true, false) => push(
            false,
            "hook: legacy SessionEnd-ravel-ravelsync.sh present, inert (no ravel-sync binary); the kit will stop shipping it"
                .into(),
        ),
        (false, _) => {}
    }

    // Mirror vs source.
    let src = match sessions_src {
        Some(p) => p.to_path_buf(),
        None => claude_sessions_dir_for(repo)?,
    };
    let mirror = repo.join(TRANSCRIPTS_SUBDIR);
    let now = std::time::SystemTime::now();
    let (mut current, mut growing, mut stale, mut missing, mut src_n) =
        (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut long_lag: Vec<(String, u64, u64)> = Vec::new(); // (name, days behind, unmirrored bytes)
    if src.is_dir() {
        for entry in std::fs::read_dir(&src)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            src_n += 1;
            let meta = std::fs::metadata(&path)?;
            let age = secs_since(meta.modified()?, now);
            let mirror_path = path.file_name().map(|n| mirror.join(n));
            let mirror_meta = mirror_path.as_ref().and_then(|p| std::fs::metadata(p).ok());
            let mirror_len = mirror_meta.as_ref().map(|m| m.len());
            // How long the mirror has been out of date: the mirror's own age
            // when one exists, else how long the source has existed unmirrored.
            let divergence = match &mirror_meta {
                Some(m) => m.modified().ok().map(|t| secs_since(t, now)),
                None => meta.created().ok().map(|t| secs_since(t, now)),
            };
            match classify_mirror_file(meta.len(), mirror_len, age, divergence) {
                MirrorState::Current => current += 1,
                MirrorState::Growing => growing += 1,
                MirrorState::GrowingLong => {
                    growing += 1;
                    long_lag.push((
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        divergence.unwrap_or(0) / 86_400,
                        meta.len().saturating_sub(mirror_len.unwrap_or(0)),
                    ));
                }
                MirrorState::Stale => stale += 1,
                MirrorState::Missing => missing += 1,
            }
        }
        let summary = format!(
            "mirror: {src_n} source session(s) → {current} current, {growing} growing (live, catches up at session end), {stale} stale, {missing} missing"
        );
        if stale > 0 || missing > 0 {
            push(true, format!("{summary} — stale/missing with no live session means raveld has not passed over this soul; is it running? (`ravel`) — or run `ravel sync {}` now", repo.display()));
        } else {
            push(false, summary);
        }
        // The age question `growing` used to skip. A live session is healthy;
        // a live session whose mirror has been behind for weeks is a backup
        // deferred indefinitely, and it used to print exactly as green as the
        // healthy one.
        if !long_lag.is_empty() {
            let unmirrored: u64 = long_lag.iter().map(|(_, _, b)| *b).sum();
            let worst = long_lag.iter().map(|(_, d, _)| *d).max().unwrap_or(0);
            push(
                true,
                format!(
                    "mirror age: {} live session(s) have been ahead of the mirror for over {} day(s) (worst: {worst}) — {:.1} MB currently exists in ONE place only. raveld re-mirrors a live session within a minute of it growing, so a lag this long means raveld was not running; start it (`ravel`) or run `ravel sync {}` now. Sessions: {}",
                    long_lag.len(),
                    GROWING_DIVERGENCE_LIMIT_SECS / 86_400,
                    unmirrored as f64 / 1_048_576.0,
                    repo.display(),
                    long_lag
                        .iter()
                        .map(|(n, d, _)| format!("{n} ({d}d)"))
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            );
        }
    } else if mirror.is_dir() {
        push(
            false,
            format!("mirror: source dir {} absent; mirror exists (archived soul?) — verify the slug if this soul is active", src.display()),
        );
    } else {
        push(
            true,
            format!(
                "mirror: NO source dir ({}) and NO mirror — nothing has ever been backed up here",
                src.display()
            ),
        );
    }

    // Other session directories that hold this repo's history under a name it
    // no longer uses. `0 missing` above only ever covered the current slug.
    match sibling_slugs_for(repo) {
        Ok(sibs) if sibs.is_empty() => push(
            false,
            "other slugs: none — no renamed session directory holds this repo's history".into(),
        ),
        Ok(sibs) => {
            for sib in sibs {
                let how = if sib.proven {
                    "PROVEN (shares session ids with this mirror)"
                } else {
                    "suspected (name matches after normalizing)"
                };
                if sib.unmirrored > 0 {
                    push(true, format!(
                        "other slugs: {} — {how}, {} of {} session(s) are NOT in this mirror ({:.1} MB unbacked-up). Bring them home: `ravel sync {} {}`",
                        sib.slug, sib.unmirrored, sib.sessions,
                        sib.unmirrored_bytes as f64 / 1_048_576.0,
                        repo.display(),
                        std::path::Path::new(&std::env::var("HOME").unwrap_or_default())
                            .join(".claude").join("projects").join(&sib.slug).display(),
                    ));
                } else {
                    push(false, format!(
                        "other slugs: {} — {how}, all {} session(s) already mirrored (history from a previous path is home)",
                        sib.slug, sib.sessions,
                    ));
                }
            }
        }
        Err(e) => push(
            true,
            format!("other slugs: could not scan for renamed session directories — {e}"),
        ),
    }

    // The agy shelf. Reported only when the substrate is present, so a
    // claude-only soul does not read a row of zeros as a finding — but an
    // unattributed conversation is always said out loud.
    match agy_conversations_for(repo) {
        Ok((convs, unattributed)) => {
            if !convs.is_empty() {
                let agy_mirror = repo.join(AGY_TRANSCRIPTS_SUBDIR);
                let home = convs
                    .iter()
                    .filter(|c| agy_mirror.join(format!("{}.jsonl", c.id)).is_file())
                    .count();
                let line = format!(
                    "agy: {} conversation(s) for this soul → {home} mirrored, {} never backed up",
                    convs.len(),
                    convs.len() - home
                );
                if home < convs.len() {
                    push(
                        true,
                        format!("{line} — run `ravel sync {}`", repo.display()),
                    );
                } else {
                    push(false, line);
                }
            }
            // Deliberately NOT an attention finding, and deliberately printed
            // even on souls with no agy conversations of their own. An
            // unattributed conversation is a fact about the MACHINE, not about
            // this soul — raising the verdict here would flip every soul on the
            // box to ATTENTION for one orphan belonging to none of them, which
            // is how a real warning gets trained into background noise. It
            // stays visible everywhere so it cannot be missed, and owning it
            // belongs to the fleet-wide health pass that walks every soul.
            if !unattributed.is_empty() {
                push(false, format!(
                    "agy (machine-wide, not this soul): {} conversation(s) on disk belong to no workspace and are backed up nowhere: {} — find the owner with `sqlite3 \"file:$HOME/.gemini/antigravity-cli/conversation_summaries.db?mode=ro\" \"SELECT conversation_id, workspace_uris FROM conversation_summaries;\"`",
                    unattributed.len(), unattributed.join(", "),
                ));
            }
        }
        Err(e) => push(
            true,
            format!("agy: could not resolve Antigravity conversations — {e}"),
        ),
    }

    // Store — read-only look, never create.
    let store_dir = repo.join(STORE_SUBDIR);
    if !store_dir.join("CURRENT").exists() {
        push(
            true,
            format!(
                "store: {} is not an oxigraph store (no CURRENT) — mirror exists but nothing is ingested; run `ravel sync {}`",
                store_dir.display(),
                repo.display()
            ),
        );
    } else {
        let store = graph::open_read_only(&store_dir)?;
        let count = |q: &str| -> Result<String> {
            use oxigraph::model::Term;
            use oxigraph::sparql::{QueryResults, SparqlEvaluator};
            if let QueryResults::Solutions(mut sols) = SparqlEvaluator::new()
                .parse_query(q)?
                .on_store(&store)
                .execute()?
            {
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
        assert_eq!(
            (copied, unchanged),
            (1, 0),
            "first pass copies the jsonl only"
        );
        assert!(dst.join("a.jsonl").exists());
        assert!(!dst.join("noise.txt").exists(), "non-jsonl never mirrors");

        let (copied, unchanged) = mirror_jsonl(&src, &dst).unwrap();
        assert_eq!((copied, unchanged), (0, 1), "second pass is a no-op");

        fs::write(src.join("a.jsonl"), "line1\nline2\n").unwrap();
        let (copied, _) = mirror_jsonl(&src, &dst).unwrap();
        assert_eq!(copied, 1, "a grown session re-mirrors");
        assert_eq!(
            fs::read_to_string(dst.join("a.jsonl")).unwrap(),
            "line1\nline2\n"
        );

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
        assert_eq!(
            read_manifest(&std::env::temp_dir().join("no-such-repo")).len(),
            0
        );
        fs::remove_dir_all(&repo).ok();
    }

    /// The mirror-state classifier: same-size is current regardless of age;
    /// divergence within the live window is a growing session (healthy);
    /// old divergence or absence is the hook having failed to fire.
    #[test]
    fn mirror_state_classification() {
        let w = LIVE_SESSION_WINDOW_SECS;
        let fresh = Some(60u64);
        assert_eq!(
            classify_mirror_file(100, Some(100), w * 10, fresh),
            MirrorState::Current
        );
        assert_eq!(
            classify_mirror_file(100, Some(100), 0, fresh),
            MirrorState::Current
        );
        assert_eq!(
            classify_mirror_file(200, Some(100), w / 2, fresh),
            MirrorState::Growing
        );
        assert_eq!(
            classify_mirror_file(200, None, w / 2, fresh),
            MirrorState::Growing
        );
        assert_eq!(
            classify_mirror_file(200, Some(100), w + 1, fresh),
            MirrorState::Stale
        );
        assert_eq!(
            classify_mirror_file(200, None, w + 1, fresh),
            MirrorState::Missing
        );
    }

    /// THE 2026-08-26 INCIDENT, pinned as a test.
    ///
    /// A live session whose mirror has been behind for weeks used to be
    /// indistinguishable from one that started twenty minutes ago: both
    /// `Growing`, both green. Four souls printed OK over 101.4 MB that existed
    /// in one place. The classifier now asks how long, and the two cases part.
    #[test]
    fn a_live_session_lagging_for_weeks_is_no_longer_green() {
        let w = LIVE_SESSION_WINDOW_SECS;
        let day = 86_400u64;
        let twenty_days = Some(20 * day);
        let twenty_minutes = Some(20 * 60);

        // Same source, same mirror, same live source age — only the DURATION
        // of the divergence differs, and that is now decisive.
        assert_eq!(
            classify_mirror_file(200, Some(100), w / 2, twenty_minutes),
            MirrorState::Growing,
            "a session that has been running twenty minutes is healthy"
        );
        assert_eq!(
            classify_mirror_file(200, Some(100), w / 2, twenty_days),
            MirrorState::GrowingLong,
            "the same shape after twenty days is a backup deferred indefinitely"
        );

        // A never-mirrored live session, judged by how long it has existed.
        assert_eq!(
            classify_mirror_file(200, None, w / 2, twenty_days),
            MirrorState::GrowingLong
        );

        // The threshold is a boundary, not a vibe.
        let lim = GROWING_DIVERGENCE_LIMIT_SECS;
        assert_eq!(
            classify_mirror_file(200, Some(100), 0, Some(lim)),
            MirrorState::Growing
        );
        assert_eq!(
            classify_mirror_file(200, Some(100), 0, Some(lim + 1)),
            MirrorState::GrowingLong
        );

        // Unknown duration must NOT be invented — it falls back to the old
        // duration-blind answer rather than guessing a bad one.
        assert_eq!(
            classify_mirror_file(200, Some(100), w / 2, None),
            MirrorState::Growing
        );
        // …and a stale or missing verdict never depended on duration anyway.
        assert_eq!(
            classify_mirror_file(200, Some(100), w + 1, twenty_days),
            MirrorState::Stale
        );
    }

    /// `spaceG.O.A.T.` → `spaceGOAT` and `M4RQ` → `m4rq` are the renames that
    /// actually happened on this machine; two different repos that merely share
    /// a word are not.
    #[test]
    fn slug_core_matches_renames_not_neighbours() {
        assert_eq!(
            slug_core("-Users-dev-repos-SQUAD-spaceG-O-A-T-"),
            slug_core("-Users-dev-repos-SQUAD-spaceGOAT"),
        );
        assert_eq!(
            slug_core("-Users-dev-repos-SQUAD--M4RQ"),
            slug_core("-Users-dev-repos-SQUAD-m4rq"),
        );
        assert_ne!(
            slug_core("-Users-dev-repos-SQUAD-lUX"),
            slug_core("-Users-dev-repos-SQUAD-lUX-making"),
            "a neighbouring repo is not a rename"
        );
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
        assert!(
            !repo.join(".ravel/oxigraph").exists(),
            "legacy path is gone"
        );
        assert!(
            !repo.join(".ravel/transcripts").exists(),
            "legacy path is gone"
        );

        assert_eq!(
            migrate_legacy_layout(&repo).unwrap(),
            0,
            "second run is a no-op"
        );
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

    /// Prefer the untruncated transcript; fall back to the short one and SAY
    /// that it is the short one.
    #[test]
    fn agy_prefers_full_transcript_and_flags_short_only() {
        let brain = std::env::temp_dir().join("ravel-agy-test-brain");
        fs::remove_dir_all(&brain).ok();
        let logs = |id: &str| brain.join(id).join(".system_generated").join("logs");
        fs::create_dir_all(logs("both")).unwrap();
        fs::create_dir_all(logs("short")).unwrap();
        fs::create_dir_all(logs("neither")).unwrap();
        fs::write(logs("both").join("transcript.jsonl"), "{}").unwrap();
        fs::write(logs("both").join("transcript_full.jsonl"), "{}").unwrap();
        fs::write(logs("short").join("transcript.jsonl"), "{}").unwrap();

        let both = agy_transcript_for(&brain, "both").unwrap();
        assert!(both.source.ends_with("transcript_full.jsonl"));
        assert!(!both.short_only);

        let short = agy_transcript_for(&brain, "short").unwrap();
        assert!(short.source.ends_with("transcript.jsonl"));
        assert!(
            short.short_only,
            "a short-only source must be flagged, not assumed whole"
        );

        assert!(agy_transcript_for(&brain, "neither").is_none());
        fs::remove_dir_all(&brain).ok();
    }

    /// `history.jsonl` is the agy analogue of the claude path slug: rows carry
    /// `workspace` + `conversationId`, a conversation repeats across many rows,
    /// and the earliest rows have no conversation yet.
    #[test]
    fn agy_workspace_map_groups_and_dedupes() {
        let root = std::env::temp_dir().join("ravel-agy-test-history");
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("history.jsonl"),
            [
                r#"{"display":"hi","timestamp":1,"workspace":"/w/one"}"#,
                r#"{"display":"a","timestamp":2,"workspace":"/w/one","conversationId":"c1"}"#,
                r#"{"display":"b","timestamp":3,"workspace":"/w/one","conversationId":"c1"}"#,
                r#"{"display":"c","timestamp":4,"workspace":"/w/one","conversationId":"c2"}"#,
                r#"{"display":"d","timestamp":5,"workspace":"/w/two","conversationId":"c3"}"#,
                r#"garbage"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let map = agy_workspace_map(&root).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map[0].0, "/w/one");
        assert_eq!(
            map[0].1,
            vec!["c1", "c2"],
            "repeated ids collapse, order preserved"
        );
        assert_eq!(map[1].1, vec!["c3"]);

        // A missing history file is empty, not an error — most machines have no
        // Antigravity CLI at all.
        assert!(agy_workspace_map(&std::env::temp_dir().join("nope"))
            .unwrap()
            .is_empty());
        fs::remove_dir_all(&root).ok();
    }

    /// The agy manifest stores a CONTENT HASH where the claude one stores a
    /// byte length — separate files, because the same column meaning two
    /// different things is how a stale entry passes for a fresh one.
    #[test]
    fn agy_manifest_round_trips_hashes() {
        let repo = std::env::temp_dir().join("ravel-agy-test-manifest");
        fs::remove_dir_all(&repo).ok();
        fs::create_dir_all(repo.join(".ravel/_ignore")).unwrap();
        let mut m = std::collections::HashMap::new();
        m.insert(
            "d6bf9dc3-c6e6-49ef-9e9b-fc70c7186501".to_string(),
            "a".repeat(64),
        );
        write_agy_manifest(&repo, &m).unwrap();
        assert_eq!(read_agy_manifest(&repo), m);
        assert!(
            !repo.join(INGEST_MANIFEST).exists(),
            "must not collide with the claude manifest"
        );
        fs::remove_dir_all(&repo).ok();
    }

    /// An in-place rewrite that keeps the file the SAME SIZE must still be seen.
    /// This is the whole reason the agy shelf hashes instead of measuring: the
    /// client rewrites a step's line from RUNNING to DONE without appending.
    #[test]
    fn agy_change_detection_catches_a_same_size_rewrite() {
        let dir = std::env::temp_dir().join("ravel-agy-test-hash");
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("t.jsonl");
        fs::write(&f, r#"{"status":"RUNNING"}"#).unwrap();
        let before = file_sha256(&f).unwrap();
        let before_len = fs::metadata(&f).unwrap().len();

        fs::write(&f, r#"{"status":"DONE___"}"#).unwrap();
        let after = file_sha256(&f).unwrap();
        assert_eq!(
            before_len,
            fs::metadata(&f).unwrap().len(),
            "same size by construction"
        );
        assert_ne!(
            before, after,
            "size says unchanged; the hash says otherwise"
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// The slug is the absolute repo path with slashes flattened to dashes.
    #[test]
    fn sessions_dir_slug_matches_claude_code_convention() {
        let repo = std::env::temp_dir().join("ravel-sync-test-slug");
        fs::create_dir_all(&repo).unwrap();
        let dir = claude_sessions_dir_for(&repo).unwrap();
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with('-'),
            "abs path starts with / → slug starts with -"
        );
        assert!(name.contains("ravel-sync-test-slug"));
        assert!(!name.contains('/'));
        fs::remove_dir_all(&repo).ok();
    }
}
