//! raveld keeps every soul's memory up to date by itself (goodlux,
//! 2026-11-05: auto-update inside ravel, no crontab, subscription first).
//!
//! Every [`MEMORY_INTERVAL`], for each soul whose transcripts gained turns:
//!
//! 1. **Read.** The soul's conversations, as the memory reader sees them —
//!    under the daemon's gate, since this is the one step that reads the
//!    mirrors a sync pass writes.
//! 2. **Wait for a whole stretch.** Turns younger than [`SETTLE`] are left
//!    for later, so a conversation still going on is not noted mid-thought.
//!    The rest is written once it fills a stretch, or once its oldest turn is
//!    [`FLUSH`] old, so a quiet soul is never left behind.
//! 3. **Write.** The same relive the backfill used — OptMem's note rule, then
//!    every merge that came due — by the writer the config names, on the
//!    subscription. At most [`PASS_CALLS`] calls per soul per pass, so one
//!    long catch-up cannot hold every other soul back.
//! 4. **Repair.** Any merge the checks refuse is rebuilt, with every merge
//!    above it.
//! 5. **Show.** The memory graph is projected, as before.
//!
//! When the subscription is out of allowance, every soul waits
//! [`LIMIT_PAUSE`] and the pass says so on /health. A soul whose pass fails
//! for any other reason waits [`RETRY_AFTER`], with the reason on /souls.
//!
//! With no `memory:` in the config, steps 1–4 never run and raveld never
//! calls a model.

use super::{log, Daemon};
use crate::daemon::config::MemoryWriter;
use crate::memory_extract::Speakers;
use crate::memory_log::MemoryLog;
use crate::memory_relive::{self, STRETCH_CHARS};
use crate::peers::PeerIndex;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration as Span, SecondsFormat, Utc};
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

/// How often the memory loop looks at each soul.
pub const MEMORY_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Turns this young are not read yet: their conversation may be going on.
pub const SETTLE: Span = Span::minutes(30);
/// Turns this old are read even when they do not fill a stretch.
pub const FLUSH: Span = Span::hours(6);
/// Calls one soul may make in one pass before the next soul's turn.
pub const PASS_CALLS: usize = 60;
/// A session reader starts over with a fresh wake past this context size:
/// under Haiku 5.5's 100k-token price tier (the backfill's setting).
pub const SESSION_TOKENS: u64 = 90_000;
/// How long every soul waits when the subscription is out of allowance.
pub const LIMIT_PAUSE: Span = Span::minutes(30);
/// How long one soul waits after a pass failed for any other reason.
pub const RETRY_AFTER: Span = Span::hours(1);

/// What the memory writer last did for one soul, shown on /souls.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SoulMemory {
    /// RFC 3339, local time.
    pub last_pass: Option<String>,
    /// The last pass, in words: what it wrote, or why it waited.
    pub last: Option<String>,
    pub error: Option<String>,
    /// RFC 3339 UTC: no pass before this, after a failure.
    pub retry_after: Option<String>,
}

/// The memory writer's state across souls.
#[derive(Debug, Default)]
pub struct MemoryState {
    /// The subscription said it is out of allowance: no calls before this.
    pub paused_until: Option<DateTime<Utc>>,
    pub paused_why: Option<String>,
    /// A pass is calling the writer for this soul right now.
    pub writing: Option<String>,
    pub souls: std::collections::HashMap<String, SoulMemory>,
}

impl Daemon {
    /// What the memory is doing, said by name on /health.
    pub fn memory_status(&self) -> String {
        let Some(w) = &self.cfg.memory else {
            return "off — the config names no memory writer, so raveld calls no model; memories are shown, not written".into();
        };
        let st = self.memory.lock().unwrap();
        let base = format!(
            "written by {} ({}) on the subscription, every {} min",
            w.harness.name(),
            w.model,
            MEMORY_INTERVAL.as_secs() / 60
        );
        match (&st.paused_until, &st.writing) {
            (Some(t), _) if *t > Utc::now() => format!(
                "{base}; PAUSED until {} — {}",
                t.with_timezone(&chrono::Local).format("%H:%M"),
                st.paused_why.as_deref().unwrap_or("")
            ),
            (_, Some(id)) => format!("{base}; writing {id} now"),
            _ => base,
        }
    }

    /// The memory's state as one word for a UI: off, paused, writing, on.
    pub fn memory_state(&self) -> &'static str {
        if self.cfg.memory.is_none() {
            return "off";
        }
        let st = self.memory.lock().unwrap();
        match (&st.paused_until, &st.writing) {
            (Some(t), _) if *t > Utc::now() => "paused",
            (_, Some(_)) => "writing",
            _ => "on",
        }
    }

    /// Mark a soul as having turns its memory has not read.
    pub fn mark(&self, id: &str) {
        self.memory_dirty.lock().unwrap().insert(id.to_string());
    }

    pub fn memory_of(&self, id: &str) -> SoulMemory {
        self.memory
            .lock()
            .map(|m| m.souls.get(id).cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    fn note(&self, id: &str, f: impl FnOnce(&mut SoulMemory)) {
        if let Ok(mut m) = self.memory.lock() {
            let s = m.souls.entry(id.to_string()).or_default();
            s.last_pass = Some(chrono::Local::now().to_rfc3339());
            f(s);
        }
    }

    /// Project one soul's memory logs into its `memory-v1` graph, under the
    /// gate.
    pub async fn memory_one(self: &Arc<Self>, id: &str) -> Result<()> {
        let soul = self.soul(id)?.clone();
        let _g = self.gate.lock().await;
        let path = soul.path.clone();
        tokio::task::spawn_blocking(move || crate::memory_log::project_into_store(&path))
            .await
            .map_err(|e| anyhow!("memory projection panicked: {e}"))??;
        Ok(())
    }

    /// Who can speak in this soul's conversations: the soul, by its repo's
    /// name; its peers, every other soul raveld serves; and the human the
    /// config names. Peers come from the config, never from folders that
    /// happen to sit beside the soul.
    fn speakers(&self, id: &str) -> Result<Speakers> {
        let soul = self.soul(id)?;
        Ok(Speakers {
            agent: soul_name(&soul.path),
            peers: self
                .souls
                .iter()
                .filter(|s| s.id != id)
                .map(|s| soul_name(&s.path))
                .collect(),
            human: self.cfg.human.clone().unwrap_or_else(|| "human".into()),
        })
    }

    /// Every served soul's sends, for telling a peer's message from the
    /// human's. Reads every soul's mirrors, so under the gate.
    async fn peer_index(self: &Arc<Self>) -> Result<Arc<PeerIndex>> {
        let souls: Vec<(std::path::PathBuf, String)> = self
            .souls
            .iter()
            .map(|s| (s.path.clone(), soul_name(&s.path)))
            .collect();
        let _g = self.gate.lock().await;
        let idx = tokio::task::spawn_blocking(move || PeerIndex::build(&souls))
            .await
            .map_err(|e| anyhow!("peer index panicked: {e}"))?;
        Ok(Arc::new(idx))
    }

    /// One soul's memory pass: read, decide, write, repair.
    async fn write_one(
        self: &Arc<Self>,
        id: &str,
        w: &MemoryWriter,
        peers: Arc<PeerIndex>,
    ) -> Result<()> {
        let soul = self.soul(id)?.clone();
        let sp = Arc::new(self.speakers(id)?);
        // The mark is taken now and put back wherever work is left. A sync
        // that lands new turns while this pass writes puts its own back, so
        // they are never lost behind a pass that started before them.
        self.memory_dirty.lock().unwrap().remove(id);
        let convs = {
            let _g = self.gate.lock().await;
            let (path, sp, peers) = (soul.path.clone(), sp.clone(), peers.clone());
            tokio::task::spawn_blocking(move || memory_relive::load(&path, &sp, &peers))
                .await
                .map_err(|e| anyhow!("memory read panicked: {e}"))??
        };
        let now = Utc::now();
        let to = stamp(now - SETTLE);
        let log_ = MemoryLog::for_soul(&soul.path);
        let (ready, chars, oldest) = memory_relive::unread(&convs, &log_, &to)?;
        let (waiting, _, _) = memory_relive::unread(&convs, &log_, "9999")?;
        if ready == 0 {
            if waiting > 0 {
                self.mark(id);
            }
            self.note(id, |s| {
                s.error = None;
                s.last = Some(if waiting == 0 {
                    "up to date".into()
                } else {
                    format!(
                        "{waiting} turns younger than {} min, read once they settle",
                        SETTLE.num_minutes()
                    )
                });
            });
            return Ok(());
        }
        let flush_at = DateTime::parse_from_rfc3339(&oldest)
            .map(|t| t.with_timezone(&Utc) + FLUSH)
            .unwrap_or(now);
        if chars < STRETCH_CHARS && flush_at > now {
            self.mark(id);
            self.note(id, |s| {
                s.error = None;
                s.last =
                    Some(format!(
                    "{ready} turns ({chars} characters) waiting for a whole stretch, or until {}",
                    flush_at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")
                ));
            });
            return Ok(());
        }
        self.memory.lock().unwrap().writing = Some(id.to_string());
        let (path, sp2, w2, d) = (soul.path.clone(), sp.clone(), w.clone(), self.clone());
        let res = tokio::task::spawn_blocking(move || -> Result<memory_relive::ReliveReport> {
            let llm = w2.harness.open(&w2.model)?;
            let log_ = MemoryLog::for_soul(&path);
            let mut r = memory_relive::relive(
                &path,
                &sp2,
                llm.as_ref(),
                &log_,
                &convs,
                "",
                &to,
                PASS_CALLS,
                SESSION_TOKENS,
                &d.stopping,
            )?;
            if r.stopped.is_none() && !memory_relive::to_repair(&log_)?.is_empty() {
                let fixed = memory_relive::repair(llm.as_ref(), &path, &sp2.agent, &log_)?;
                r.merges += fixed.merges;
                r.stopped = fixed.stopped;
            }
            Ok(r)
        })
        .await
        .map_err(|e| anyhow!("memory pass panicked: {e}"));
        self.memory.lock().unwrap().writing = None;
        let r = match res.and_then(|r| r) {
            Ok(r) => r,
            Err(e) => {
                self.mark(id);
                return Err(e);
            }
        };
        let did = format!(
            "{} notes and {} merges from {} turns",
            r.notes, r.merges, r.turns
        );
        match &r.stopped {
            Some(why) if crate::writer::is_usage_limit_text(why) => {
                self.mark(id);
                let until = Utc::now() + LIMIT_PAUSE;
                {
                    let mut m = self.memory.lock().unwrap();
                    m.paused_until = Some(until);
                    m.paused_why = Some(why.clone());
                }
                self.note(id, |s| {
                    s.last = Some(format!("{did}, then the subscription's limit"))
                });
                log(format!(
                    "{id}: memory {did}; subscription limit, every soul waits {} min — {why}",
                    LIMIT_PAUSE.num_minutes()
                ));
            }
            Some(why) => {
                self.mark(id);
                let retry = stamp(Utc::now() + RETRY_AFTER);
                self.note(id, |s| {
                    s.last = Some(did.clone());
                    s.error = Some(why.clone());
                    s.retry_after = Some(retry.clone());
                });
                log(format!(
                    "{id}: memory pass FAILED after {did} — {why}; next try after {retry}"
                ));
            }
            None => {
                if r.yielded || waiting > ready {
                    self.mark(id);
                }
                self.note(id, |s| {
                    s.error = None;
                    s.retry_after = None;
                    s.last = Some(if r.yielded {
                        format!("{did}; more to read next pass")
                    } else {
                        did.clone()
                    });
                });
                log(format!(
                    "{id}: memory {did}{}",
                    if r.yielded { "; more next pass" } else { "" }
                ));
            }
        }
        Ok(())
    }
}

impl Daemon {
    /// One soul's memory pass now, exactly as the loop runs it: for a trial
    /// on one soul before the loop is turned on for all of them.
    pub async fn memory_pass(self: &Arc<Self>, id: &str) -> Result<SoulMemory> {
        let w = self
            .cfg
            .memory
            .clone()
            .ok_or_else(|| anyhow!("no memory writer configured"))?;
        let peers = self.peer_index().await?;
        self.write_one(id, &w, peers).await?;
        Ok(self.memory_of(id))
    }
}

/// A soul's name: its repo folder's. What its peers call it, and what a
/// memory calls it.
pub fn soul_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// An instant as the turns write theirs, so the two compare as strings.
fn stamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The memory schedule: write what is due, then project, every soul, every
/// [`MEMORY_INTERVAL`], after a first wait so the transcript passes catch up
/// first.
pub async fn run_memory_loop(d: Arc<Daemon>) {
    tokio::time::sleep(Duration::from_secs(60)).await;
    log(format!("memory {}", d.memory_status()));
    loop {
        if let Some(w) = d.cfg.memory.clone() {
            if let Err(e) = write_due(&d, &w).await {
                log(format!("memory pass FAILED — {e:#}"));
            }
        }
        let ids: Vec<String> = d.souls.iter().map(|s| s.id.clone()).collect();
        for id in ids {
            if let Err(e) = d.memory_one(&id).await {
                log(format!("{id}: memory projection FAILED — {e:#}"));
            }
        }
        tokio::time::sleep(MEMORY_INTERVAL).await;
    }
}

/// Every soul with new turns, in config order, until the subscription says
/// stop or raveld does.
async fn write_due(d: &Arc<Daemon>, w: &MemoryWriter) -> Result<()> {
    let mut peers: Option<Arc<PeerIndex>> = None;
    for s in d.souls.clone() {
        if d.stopping.load(Ordering::Relaxed) {
            break;
        }
        if d.memory
            .lock()
            .unwrap()
            .paused_until
            .is_some_and(|t| t > Utc::now())
        {
            break;
        }
        if !d.memory_dirty.lock().unwrap().contains(&s.id) {
            continue;
        }
        let waits = d
            .memory_of(&s.id)
            .retry_after
            .and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
            .is_some_and(|t| t > Utc::now());
        if waits {
            continue;
        }
        if peers.is_none() {
            peers = Some(d.peer_index().await?);
        }
        let idx = peers.clone().expect("built just above");
        if let Err(e) = d.write_one(&s.id, w, idx).await {
            d.mark(&s.id);
            let retry = stamp(Utc::now() + RETRY_AFTER);
            d.note(&s.id, |m| {
                m.error = Some(format!("{e:#}"));
                m.retry_after = Some(retry.clone());
            });
            log(format!(
                "{}: memory pass FAILED — {e:#}; next try after {retry}",
                s.id
            ));
        }
    }
    Ok(())
}
