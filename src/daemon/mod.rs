//! raveld — the one daemon per machine that owns every soul's transcript store.
//!
//! What raveld is (goodlux, 2026-09-22, following pand): the only process
//! that writes a soul's `.ravel/_ignore/` — the transcript mirror and the
//! graph. It watches the harness's transcript folders itself and brings every
//! registered soul up to date on a schedule. There are no hooks. `ravel` (the
//! command line) is a client of raveld and starts it when it is not running.
//!
//! Each pass is `sync::sync_soul`: mirror what the harness wrote, ingest what
//! the mirror gained, both idempotent and manifest-keyed, so an unchanged
//! soul costs about a second. One pass at a time per soul; passes for
//! different souls do not overlap either — this is a backup, not a race.

pub mod config;
pub mod http;
pub mod registry;

use crate::sync::{self, SyncStats};
use anyhow::{anyhow, Result};
use chrono::Local;
use config::DaemonConfig;
use registry::Soul;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// What one sync pass did, in the numbers `ravel sync` prints.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SyncSummary {
    pub mirrored: usize,
    pub unchanged: usize,
    pub sessions: usize,
    pub skipped: usize,
    pub turns: usize,
    pub keys: usize,
    pub agy_mirrored: usize,
    pub agy_unchanged: usize,
    pub agy_sessions: usize,
    pub agy_skipped: usize,
    pub agy_turns: usize,
    pub agy_gaps: usize,
    pub agy_unattributed: Vec<String>,
    pub agy_lossy: Vec<String>,
    pub agy_failed: Vec<String>,
}

impl From<&SyncStats> for SyncSummary {
    fn from(s: &SyncStats) -> Self {
        Self {
            mirrored: s.mirrored,
            unchanged: s.unchanged,
            sessions: s.sessions,
            skipped: s.skipped,
            turns: s.turns,
            keys: s.keys,
            agy_mirrored: s.agy.mirrored,
            agy_unchanged: s.agy.unchanged,
            agy_sessions: s.agy.sessions,
            agy_skipped: s.agy.skipped,
            agy_turns: s.agy.turns,
            agy_gaps: s.agy.gaps,
            agy_unattributed: s.agy.unattributed.clone(),
            agy_lossy: s.agy.lossy.clone(),
            agy_failed: s.agy.failed.clone(),
        }
    }
}

/// What the daemon remembers about one soul between passes. In memory only;
/// the truth is on disk and a restart re-derives it.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SoulState {
    /// RFC 3339, local time. A string so it serializes without ceremony.
    pub last_sync: Option<String>,
    pub last_error: Option<String>,
    pub passes: u64,
    pub last: Option<SyncSummary>,
}

pub struct Daemon {
    pub cfg: DaemonConfig,
    pub souls: Vec<Soul>,
    /// Configured paths that did not resolve. Reported on every /health.
    pub problems: Vec<String>,
    pub started: Instant,
    pub passes: AtomicU64,
    state: Mutex<HashMap<String, SoulState>>,
    /// One pass at a time across all souls.
    gate: tokio::sync::Mutex<()>,
    pub shutdown: Notify,
}

pub fn log(msg: impl AsRef<str>) {
    eprintln!(
        "{} raveld {}",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        msg.as_ref()
    );
}

impl Daemon {
    pub fn open(cfg: DaemonConfig) -> Result<Self> {
        let (souls, problems) = registry::build(&cfg.souls);
        let state = souls
            .iter()
            .map(|s| (s.id.clone(), SoulState::default()))
            .collect();
        Ok(Self {
            cfg,
            souls,
            problems,
            started: Instant::now(),
            passes: AtomicU64::new(0),
            state: Mutex::new(state),
            gate: tokio::sync::Mutex::new(()),
            shutdown: Notify::new(),
        })
    }

    /// A soul by its six-character id, or by a path inside it.
    pub fn soul(&self, id_or_path: &str) -> Result<&Soul> {
        if let Some(s) = self.souls.iter().find(|s| s.id == id_or_path) {
            return Ok(s);
        }
        if let Ok(p) = std::path::Path::new(id_or_path).canonicalize() {
            if let Some(s) = self.souls.iter().find(|s| p.starts_with(&s.path)) {
                return Ok(s);
            }
        }
        Err(anyhow!(
            "no registered soul {id_or_path:?} — ids: {}; add the repo to souls: in {} and run `raveld restart`",
            self.souls
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            self.cfg.path.display()
        ))
    }

    pub fn state_of(&self, id: &str) -> SoulState {
        self.state
            .lock()
            .map(|m| m.get(id).cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Run one pass over one soul. Serialized: a second caller waits.
    /// `sessions_src` overrides where the harness's session files are read
    /// from — how a sibling slug's history is brought home; `None` is the
    /// soul's own directory.
    pub async fn sync_one(
        self: &Arc<Self>,
        id: &str,
        sessions_src: Option<std::path::PathBuf>,
    ) -> Result<SyncSummary> {
        let soul = self.soul(id)?.clone();
        let _g = self.gate.lock().await;
        let path = soul.path.clone();
        let res =
            tokio::task::spawn_blocking(move || sync::sync_soul(&path, sessions_src.as_deref()))
                .await
                .map_err(|e| anyhow!("sync task panicked: {e}"))?;
        self.passes.fetch_add(1, Ordering::Relaxed);
        let mut m = self.state.lock().map_err(|_| anyhow!("state poisoned"))?;
        let st = m.entry(soul.id.clone()).or_default();
        st.passes += 1;
        st.last_sync = Some(Local::now().to_rfc3339());
        match &res {
            Ok(stats) => {
                st.last_error = None;
                st.last = Some(SyncSummary::from(stats));
            }
            Err(e) => {
                st.last_error = Some(format!("{e:#}"));
                log(format!(
                    "{} ({}): sync FAILED — {e:#}",
                    soul.id,
                    soul.path.display()
                ));
            }
        }
        res.map(|s| SyncSummary::from(&s))
    }

    /// Every soul, in config order. A failure on one is recorded and the
    /// pass continues; one broken soul must not cost the others their backup.
    pub async fn sync_all(self: &Arc<Self>) -> Vec<(String, Result<SyncSummary>)> {
        let ids: Vec<String> = self.souls.iter().map(|s| s.id.clone()).collect();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let r = self.sync_one(&id, None).await;
            out.push((id, r));
        }
        out
    }

    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }
}

/// The schedule: a pass over every soul, then wait `interval_secs`, forever.
/// The first pass starts immediately so a fresh daemon catches up at once.
pub async fn run_sync_loop(d: Arc<Daemon>) {
    let interval = Duration::from_secs(d.cfg.interval_secs.max(1));
    loop {
        let results = d.sync_all().await;
        for (id, r) in &results {
            if let Ok(s) = r {
                if s.mirrored + s.sessions + s.agy_mirrored + s.agy_sessions > 0 {
                    log(format!(
                        "{id}: mirrored {} → ingested {} session(s), {} turns; agy mirrored {} → {} conversation(s)",
                        s.mirrored, s.sessions, s.turns, s.agy_mirrored, s.agy_sessions
                    ));
                }
            }
        }
        tokio::time::sleep(interval).await;
    }
}
