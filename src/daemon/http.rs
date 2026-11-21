//! raveld's HTTP surface, on 127.0.0.1 only. `ravel` is its client.
//!
//!   GET  /ravel/health                      the daemon: version, uptime, souls, config problems
//!   GET  /ravel/stores                       every soul: id, path, last pass, last numbers
//!   POST /ravel/sync                        a pass over every soul, now
//!   POST /ravel/{org}/{store}/sync             a pass over one soul, now
//!   GET  /ravel/{org}/{store}/health           the read-only diagnostic (findings + attention flag)
//!   GET  /ravel/{org}/{store}/stats            counts: quads, graphs, turns, dated turns, claims, top predicates
//!   POST /ravel/{org}/{store}/query  {query}   SPARQL, W3C results JSON
//!   POST /ravel/{org}/{store}/sparql          the same, by the W3C SPARQL protocol (raw query body; n-triples on CONSTRUCT)
//!   POST /ravel/{org}/{store}/import  {path}   copy a claude.ai export into the soul, then a pass
//!   GET  /ravel/{org}/{store}/memory[?lines=N] the memory wake view: OptMem's cover (default 96 lines)
//!   GET  /ravel/{org}/{store}/memory/{node}    one node opened: a block's two halves (`16-31`), a note's turns
//!   GET  /ravel/{org}/{store}/memory-progress  turns, turns read, notes, blocks, blocks pending, done
//!   GET  /ravel/{org}/{store}/memory-check     the memory checked end to end; `failures` empty = good
//!   POST /ravel/shutdown                    stop, after answering
//!
//! Reads open the store read-only; only the sync pass writes.

use super::Daemon;
use crate::{graph, sync};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use oxigraph::model::Term;
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

type Shared = Arc<Daemon>;

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn bad(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, format!("{e:#}"))
}

fn not_found(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("{e:#}"))
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
}

/// Every route, behind the gate. Separate from `serve` so a test can stand
/// it up on a port of its own.
pub fn router(d: Shared) -> Router {
    Router::new()
        // The stack's one URL shape (goodlux, 2026-11-20):
        // /{daemon}/{org}/{store}/..., org = squad, store = username. The
        // path reads the way the store's addresses do. Calls that name no
        // store sit directly under /ravel.
        .route("/ravel/health", get(health))
        .route("/ravel/stores", get(souls))
        .route("/ravel/sync", post(sync_all))
        .route("/ravel/shutdown", post(shutdown))
        .route("/ravel/{org}/{store}/sparql", post(store_sparql))
        .route("/ravel/{org}/{store}/query", post(soul_query))
        .route("/ravel/{org}/{store}/sync", post(sync_one))
        .route("/ravel/{org}/{store}/import", post(import_one))
        .route("/ravel/{org}/{store}/memory", get(memory_view))
        .route("/ravel/{org}/{store}/memory-progress", get(memory_progress))
        .route("/ravel/{org}/{store}/memory-check", get(memory_check))
        .route("/ravel/{org}/{store}/memory/{node}", get(memory_open))
        .route("/ravel/{org}/{store}/health", get(soul_health))
        .route("/ravel/{org}/{store}/stats", get(soul_stats))
        .layer(axum::middleware::from_fn_with_state(d.clone(), super::gate::check))
        .with_state(d)
}

pub async fn serve(d: Shared) -> anyhow::Result<()> {
    let app = router(d.clone());
    let addr = format!("127.0.0.1:{}", d.cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    super::log(format!("listening on http://{addr}"));
    tokio::spawn(name_every_other_signal());
    let stop = d.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => super::log("interrupted; stopping"),
                // `raveld stop` from a soul's session ends here: /shutdown is
                // the machine owner's, the process signal is the OS user's.
                _ = terminated() => super::log("SIGTERM; stopping"),
                _ = stop.shutdown.notified() => super::log("asked to stop; stopping"),
            }
            stop.stopping
                .store(true, std::sync::atomic::Ordering::Relaxed);
        })
        .await?;
    Ok(())
}

/// The key list as tessera sees it, and whether reads need a token yet.
fn identity_status(d: &Daemon) -> Value {
    let s = d.keys.status();
    json!({
        "keys_file": s.keys_file.display().to_string(),
        "squaddies_with_a_key": s.squaddies_with_a_key,
        "error": s.error,
        "machine_owner_recorded": matches!(d.keys.owner(), Ok(Some(_))),
        "reads_need_a_token": super::gate::READS_NEED_A_TOKEN,
    })
}

async fn health(State(d): State<Shared>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "uptime_secs": d.uptime().as_secs(),
        "port": d.cfg.port,
        "interval_secs": d.cfg.interval_secs,
        "config": d.cfg.path.display().to_string(),
        "config_present": d.cfg.present,
        "config_problems": d.problems,
        "stores": d.souls.iter().filter_map(|s| s.address()).collect::<Vec<_>>(),
        // Every repo raveld backs up, named or not.
        "served": d.souls.len(),
        "passes": d.passes.load(std::sync::atomic::Ordering::Relaxed),
        "memory": d.memory_status(),
        "memory_state": d.memory_state(),
        "identity": identity_status(&d),
    }))
}

async fn souls(State(d): State<Shared>) -> Json<Value> {
    let rows: Vec<Value> = d
        .souls
        .iter()
        .map(|s| {
            let st = d.state_of(&s.id);
            // The same three fields gitlexd reports for this soul. A repo
            // whose .lex/repo.yml cannot be read as a root says why.
            let names = crate::names::Names::for_soul(&s.path);
            let (root, squad, username, names_error) = match &names {
                Ok(n) => (
                    Some(n.root().to_string()),
                    n.squad().map(str::to_string),
                    n.username().map(str::to_string),
                    None,
                ),
                Err(e) => (None, None, None, Some(format!("{e:#}"))),
            };
            json!({
                // The name a caller uses in a URL. Null for a repo with no
                // username: it is backed up, and cannot be asked for yet.
                // org and store are what a URL takes: /ravel/{org}/{store}/...
                // Null for a repo with no squad and username: it is backed
                // up, and cannot be asked for yet.
                "org": squad,
                "store": username,
                "genesis": s.genesis,
                "root": root,
                "squad": squad,
                "username": username,
                "names_error": names_error,
                "path": s.path.display().to_string(),
                "last_sync": st.last_sync,
                "last_error": st.last_error,
                "passes": st.passes,
                "last": st.last,
                "memory": d.memory_of(&s.id),
            })
        })
        .collect();
    Json(json!(rows))
}

async fn sync_all(State(d): State<Shared>) -> Json<Value> {
    let results = d.sync_all().await;
    let rows: Vec<Value> = results
        .into_iter()
        .map(|(id, r)| match r {
            Ok(s) => json!({ "id": id, "ok": true, "summary": s }),
            Err(e) => json!({ "id": id, "ok": false, "error": format!("{e:#}") }),
        })
        .collect();
    Json(json!(rows))
}

#[derive(Deserialize, Default)]
struct SyncBody {
    /// Read session files from here instead of the soul's own directory —
    /// how a sibling slug's history is brought home.
    sessions_dir: Option<String>,
}

async fn sync_one(
    State(d): State<Shared>,
    Path((org, store)): Path<(String, String)>,
    body: Option<Json<SyncBody>>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul_named(&org, &store).map_err(not_found)?.clone();
    let src = body
        .and_then(|Json(b)| b.sessions_dir)
        .map(std::path::PathBuf::from);
    if let Some(p) = &src {
        if !p.is_dir() {
            return Err(bad(format!(
                "sessions_dir {} is not a directory",
                p.display()
            )));
        }
    }
    let s = d.sync_one(&soul.id, src).await.map_err(internal)?;
    Ok(Json(
        json!({ "store": soul.name(), "path": soul.path.display().to_string(), "summary": s }),
    ))
}

#[derive(serde::Deserialize)]
struct ImportBody {
    path: String,
}

async fn import_one(
    State(d): State<Shared>,
    Path((org, store)): Path<(String, String)>,
    Json(b): Json<ImportBody>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul_named(&org, &store).map_err(not_found)?.clone();
    let src = std::path::PathBuf::from(&b.path);
    if !src.is_dir() {
        return Err(bad(format!("{} is not a directory", src.display())));
    }
    let (outcome, s) = d.import(&soul.id, src).await.map_err(bad)?;
    let (status, name, files, bytes) = match outcome {
        crate::sync::ImportOutcome::Copied { name, files, bytes } => ("copied", name, files, bytes),
        crate::sync::ImportOutcome::AlreadyImported { name } => ("already imported", name, 0, 0),
    };
    Ok(Json(json!({
        "store": soul.name(),
        "status": status,
        "export": name,
        "files": files,
        "bytes": bytes,
        "summary": s,
    })))
}

#[derive(Deserialize)]
struct MemoryViewQuery {
    lines: Option<usize>,
}

async fn memory_view(
    State(d): State<Shared>,
    Path((org, store)): Path<(String, String)>,
    Query(q): Query<MemoryViewQuery>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul_named(&org, &store).map_err(not_found)?.clone();
    let lines = q.lines.unwrap_or(crate::memory::WAKE_LINES).max(1);
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let log = crate::memory_log::MemoryLog::for_soul(&soul.path);
        let ms = log.memories()?;
        let sums = log.summaries()?;
        let view = crate::memory_log::wake_view(&ms, &sums, ms.len(), lines);
        Ok(json!({ "store": soul.name(), "memories": ms.len(), "summaries": sums.len(), "lines": view }))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(Json(v))
}

async fn memory_open(
    State(d): State<Shared>,
    Path((org, store, node)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul_named(&org, &store).map_err(not_found)?.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        use crate::memory_log::{zoom, MemoryLog};
        let log = MemoryLog::for_soul(&soul.path);
        let ms = log.memories()?;
        let sums = log.summaries()?;
        if let Some(b) = crate::memory::parse_block(&node) {
            if b.0 >= ms.len() {
                anyhow::bail!("#{node} is beyond the memory: it holds {} notes", ms.len());
            }
            let kids = zoom(&ms, &sums, b);
            return Ok(json!({ "node": node, "kind": "block", "children": kids }));
        }
        let (pos, m) = ms
            .iter()
            .enumerate()
            .find(|(i, m)| m.id == node || node.trim_start_matches('#') == i.to_string())
            .ok_or_else(|| anyhow::anyhow!("no note or block {node:?} in {}", soul.path.display()))?;
        let store = open_store(&soul)?;
        let names = crate::names::Names::for_soul(&soul.path)?;
        let mut turns = Vec::new();
        for t in &m.turns {
            let turn = names.turn(t);
            let q = format!(
                "SELECT ?ts ?role ?text WHERE {{ GRAPH ?g {{ <{turn}> <https://repolex.ai/ontology/ravel/timestamp> ?ts ; <https://repolex.ai/ontology/ravel/role> ?role . OPTIONAL {{ <{turn}> <https://repolex.ai/ontology/ravel/text> ?text }} }} }} LIMIT 1"
            );
            let row = graph::query_rows(&store, &q)?.into_iter().next().unwrap_or_default();
            turns.push(json!({
                "turn": t,
                "ts": row.get("ts"),
                "role": row.get("role"),
                "text": row.get("text"),
            }));
        }
        Ok(json!({ "node": node, "kind": "memory", "position": pos, "memory": m, "turns": turns }))
    })
    .await
    .map_err(internal)?
    .map_err(not_found)?;
    Ok(Json(v))
}

/// How far a soul's memory has got, for a UI's soul list: whether it is
/// done, not just how big it is (w3bl0rd, 2026-10-05). `turns` counts
/// distinct Turn nodes in the store; `turns_read` is turns the reader has
/// read; `pending_blocks` are OptMem merges due and not written.
async fn memory_progress(
    State(d): State<Shared>,
    Path((org, store)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul_named(&org, &store).map_err(not_found)?.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        use crate::memory_log::MemoryLog;
        let log = MemoryLog::for_soul(&soul.path);
        let ms = log.memories()?;
        let sums = log.summaries()?;
        let read = log.extracted()?.len();
        let pending = crate::memory_log::pending(&sums, ms.len()).len();
        // The reader's own stream size, from its last run: the store's Turn
        // count includes tool output and subagents, which are never read.
        let (turns, counted) = log.stream_size().unwrap_or((0, String::new()));
        Ok(json!({
            "store": soul.name(),
            "turns": turns,
            "turns_counted": counted,
            "turns_read": read,
            "memories": ms.len(),
            "blocks": sums.len(),
            "pending_blocks": pending,
            "done": turns > 0 && read as u64 >= turns && pending == 0,
        }))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(Json(v))
}

async fn soul_health(
    State(d): State<Shared>,
    Path((org, store)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul_named(&org, &store).map_err(not_found)?.clone();
    let path = soul.path.clone();
    let findings = tokio::task::spawn_blocking(move || sync::diagnose(&path, None))
        .await
        .map_err(internal)?
        .map_err(internal)?;
    let attention = findings.iter().any(|f| f.attention);
    let rows: Vec<Value> = findings
        .iter()
        .map(|f| json!({ "attention": f.attention, "line": f.line }))
        .collect();
    Ok(Json(json!({
        "store": soul.name(),
        "path": soul.path.display().to_string(),
        "attention": attention,
        "findings": rows,
    })))
}

fn open_store(soul: &super::registry::Soul) -> anyhow::Result<Store> {
    let dir = soul.path.join(sync::STORE_SUBDIR);
    // Never create a store from a read: a mis-aimed path must refuse, not
    // litter an empty store and report a trustworthy-looking zero.
    if !dir.join("CURRENT").exists() {
        anyhow::bail!(
            "{} has no store yet at {} — nothing has been synced",
            soul.path.display(),
            dir.display()
        );
    }
    graph::open_read_only(&dir)
}

fn literal_value(t: &Term) -> String {
    match t {
        Term::Literal(l) => l.value().to_string(),
        other => other.to_string(),
    }
}

fn scalar(store: &Store, q: &str) -> anyhow::Result<String> {
    if let QueryResults::Solutions(mut sols) = SparqlEvaluator::new()
        .parse_query(q)?
        .on_store(store)
        .execute()?
    {
        if let Some(s) = sols.next() {
            let s = s?;
            // Bound to a local first: as a block's tail expression the
            // iterator's borrow of `s` would outlive `s`.
            let first = s.iter().next().map(|(_, t)| literal_value(t));
            if let Some(v) = first {
                return Ok(v);
            }
        }
    }
    Ok(String::new())
}

async fn soul_stats(
    State(d): State<Shared>,
    Path((org, store)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul_named(&org, &store).map_err(not_found)?.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let store = open_store(&soul)?;
        let g = "GRAPH ?g { ?s ?p ?o }";
        let quads = scalar(&store, &format!("SELECT (COUNT(*) AS ?n) WHERE {{ {g} }}"))?;
        let graphs = scalar(&store, &format!("SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE {{ {g} }}"))?;
        let subjects = scalar(&store, &format!("SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ {g} }}"))?;
        let turn_prefix = crate::names::Names::for_soul(&soul.path)?.turn_partition();
        let turns = scalar(&store, &format!("SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ GRAPH ?g {{ ?s ?p ?o }} FILTER(STRSTARTS(STR(?s), \"{turn_prefix}\")) }}"))?;
        let dt = "FILTER(DATATYPE(?t) = <http://www.w3.org/2001/XMLSchema#dateTime>)";
        let dated = scalar(&store, &format!("SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ GRAPH ?g {{ ?s ?ts ?t }} {dt} }}"))?;
        let earliest = scalar(&store, &format!("SELECT (MIN(?t) AS ?m) WHERE {{ GRAPH ?g {{ ?s ?p ?t }} {dt} }}"))?;
        let latest = scalar(&store, &format!("SELECT (MAX(?t) AS ?m) WHERE {{ GRAPH ?g {{ ?s ?p ?t }} {dt} }}"))?;
        let claims = scalar(&store, "SELECT (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> ?tt } }")?;
        let mut top = Vec::new();
        if let QueryResults::Solutions(sols) = SparqlEvaluator::new()
            .parse_query("SELECT ?p (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } } GROUP BY ?p ORDER BY DESC(?n) LIMIT 20")?
            .on_store(&store)
            .execute()?
        {
            for s in sols {
                let s = s?;
                let p = s.get("p").map(|t| t.to_string()).unwrap_or_default();
                let n = s.get("n").map(literal_value).unwrap_or_default();
                top.push(json!({ "predicate": p, "n": n.parse::<u64>().unwrap_or(0) }));
            }
        }
        let num = |s: &str| s.parse::<u64>().unwrap_or(0);
        Ok(json!({
            "store": soul.name(),
            "path": soul.path.display().to_string(),
            "quads": num(&quads),
            "graphs": num(&graphs),
            "subjects": num(&subjects),
            "turns": num(&turns),
            "dated_turns": num(&dated),
            "earliest": earliest,
            "latest": latest,
            "claims": num(&claims),
            "top_predicates": top,
        }))
    })
    .await
    .map_err(internal)?
    .map_err(bad)?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct QueryBody {
    query: String,
}

fn term_json(t: &Term) -> Value {
    match t {
        Term::NamedNode(n) => json!({ "type": "uri", "value": n.as_str() }),
        Term::BlankNode(b) => json!({ "type": "bnode", "value": b.as_str() }),
        Term::Literal(l) => {
            let mut o = json!({ "type": "literal", "value": l.value() });
            if let Some(lang) = l.language() {
                o["xml:lang"] = Value::from(lang);
            } else {
                o["datatype"] = Value::from(l.datatype().as_str());
            }
            o
        }
        other => json!({ "type": "triple", "value": other.to_string() }),
    }
}

async fn terminated() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}

/// Signals that would end the process without a word, caught and written to
/// the log instead; the daemon stays up. With these named, a daemon that is
/// gone with no last line was killed outright (SIGKILL) or crashed below Rust:
/// on 2026-11-21 it vanished after 08:00:13 with nothing written, and nobody
/// could say which.
async fn name_every_other_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let kinds = [
        ("SIGHUP", SignalKind::hangup()),
        ("SIGQUIT", SignalKind::quit()),
        ("SIGUSR1", SignalKind::user_defined1()),
        ("SIGUSR2", SignalKind::user_defined2()),
        ("SIGALRM", SignalKind::alarm()),
    ];
    for (name, kind) in kinds {
        match signal(kind) {
            Ok(mut s) => {
                tokio::spawn(async move {
                    while s.recv().await.is_some() {
                        super::log(format!(
                            "{name} received; staying up (`raveld stop` or SIGTERM stops it)"
                        ));
                    }
                });
            }
            Err(e) => super::log(format!("cannot watch for {name}: {e}")),
        }
    }
}

/// What a query produced, before it is given a shape for the wire.
enum Answer {
    /// W3C SPARQL results JSON, for SELECT and ASK.
    Results(Value),
    /// The statements of a CONSTRUCT or DESCRIBE, one N-Triples line each.
    Triples(Vec<String>),
}

/// Run an outside query: prepared by `crate::outside` (base, calamus,
/// every graph in view), then evaluated on the soul's store, read-only.
fn answer(
    soul: &super::registry::Soul,
    text: &str,
    stop: oxigraph::sparql::CancellationToken,
) -> anyhow::Result<(Answer, Vec<String>)> {
    let store = open_store(soul)?;
    let names = crate::names::Names::for_soul(&soul.path)?;
    let (results, report) = crate::outside::run_until(&store, text, names.root(), Some(stop))?;
    let notes = crate::outside::notes(&report);
    let a = match results {
        QueryResults::Solutions(sols) => {
            let vars: Vec<String> = sols
                .variables()
                .iter()
                .map(|v| v.as_str().to_string())
                .collect();
            let mut rows = Vec::new();
            for s in sols {
                let s = s?;
                let mut row = serde_json::Map::new();
                for (v, t) in s.iter() {
                    row.insert(v.as_str().to_string(), term_json(t));
                }
                rows.push(Value::Object(row));
            }
            Answer::Results(json!({ "head": { "vars": vars }, "results": { "bindings": rows } }))
        }
        QueryResults::Boolean(b) => Answer::Results(json!({ "head": {}, "boolean": b })),
        QueryResults::Graph(triples) => {
            let mut out = Vec::new();
            for t in triples {
                out.push(t?.to_string());
            }
            Answer::Triples(out)
        }
    };
    Ok((a, notes))
}

/// How many outside queries one store runs at once. One more is refused
/// with 503 rather than queued: a reader on a timer whose question takes
/// longer than its interval otherwise stacks work without end (2026-11-21,
/// an 8 s question asked every 4 s).
const QUERIES_AT_ONCE: usize = 4;
/// A query still running after this long is stopped and answered 503.
const QUERY_TIME_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);
/// A query that took at least this long is written to the log.
const QUERY_IS_SLOW: std::time::Duration = std::time::Duration::from_secs(3);

fn running() -> &'static std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, usize>> {
    static R: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, usize>>,
    > = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

/// One of a store's running queries. Held by the work itself, so a caller
/// that walks away does not free the place while the work is still going.
struct Seat(std::path::PathBuf);

impl Seat {
    fn take(path: &std::path::Path) -> Option<Self> {
        let mut r = running().lock().unwrap();
        let n = r.entry(path.to_path_buf()).or_insert(0);
        if *n >= QUERIES_AT_ONCE {
            return None;
        }
        *n += 1;
        Some(Self(path.to_path_buf()))
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        let mut r = running().lock().unwrap();
        if let Some(n) = r.get_mut(&self.0) {
            *n = n.saturating_sub(1);
        }
    }
}

/// Stops the work when the handler is dropped, which is what happens when
/// the caller hangs up before the answer.
struct StopWhenDropped(oxigraph::sparql::CancellationToken);

impl Drop for StopWhenDropped {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cut = flat.chars().take(200).collect::<String>();
    if flat.chars().count() > 200 {
        cut.push('…');
    }
    cut
}

/// An outside query, with a place at the store, a time limit, and a line in
/// the log when it was slow, stopped or abandoned.
async fn ask(
    soul: super::registry::Soul,
    text: String,
) -> Result<(Answer, Vec<String>), ApiError> {
    let name = soul
        .address()
        .unwrap_or_else(|| soul.path.display().to_string());
    let Some(seat) = Seat::take(&soul.path) else {
        super::log(format!(
            "query refused on {name}: {QUERIES_AT_ONCE} already running: {}",
            one_line(&text)
        ));
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "{name} already has {QUERIES_AT_ONCE} queries running; ask again when one has finished. A query asked on a timer must take less time than its interval"
            ),
        ));
    };
    let stop = oxigraph::sparql::CancellationToken::new();
    let hung_up = StopWhenDropped(stop.clone());
    let (for_work, shown, began) = (stop.clone(), one_line(&text), std::time::Instant::now());
    let who = name.clone();
    let mut work = tokio::task::spawn_blocking(move || {
        let _seat = seat;
        let out = answer(&soul, &text, for_work.clone());
        let took = began.elapsed();
        if for_work.is_cancelled() {
            super::log(format!(
                "query stopped on {who} after {:.1}s (time limit {}s, or the caller hung up): {shown}",
                took.as_secs_f64(),
                QUERY_TIME_LIMIT.as_secs()
            ));
        } else if took >= QUERY_IS_SLOW {
            super::log(format!(
                "slow query on {who}: {:.1}s: {shown}",
                took.as_secs_f64()
            ));
        }
        out
    });
    let out = tokio::select! {
        r = &mut work => r,
        _ = tokio::time::sleep(QUERY_TIME_LIMIT) => {
            stop.cancel();
            let _ = work.await;
            return Err(ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "the query on {name} ran for {} seconds and was stopped; narrow it (sort fewer rows, or filter before ORDER BY)",
                    QUERY_TIME_LIMIT.as_secs()
                ),
            ));
        }
    };
    // Answered: nothing left to stop.
    std::mem::forget(hung_up);
    out.map_err(internal)?.map_err(bad)
}

/// ravel's own route, JSON in and JSON out. `notes` is present only when
/// the query holds a name that was left as written and will likely match
/// nothing; the answer itself cannot say so.
async fn soul_query(
    State(d): State<Shared>,
    Path((org, store)): Path<(String, String)>,
    Json(body): Json<QueryBody>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul_named(&org, &store).map_err(not_found)?.clone();
    let (a, notes) = ask(soul, body.query).await?;
    let mut v = match a {
        Answer::Results(v) => v,
        Answer::Triples(t) => json!({ "triples": t }),
    };
    if !notes.is_empty() {
        v["notes"] = json!(notes);
    }
    Ok(Json(v))
}

/// The W3C SPARQL protocol route, the same shape gitlexd and pand answer
/// on, so a standard client (and Syrinx) needs nothing ravel-specific.
/// The query is the raw body (`application/sparql-query`) or `{"query"}`
/// (`application/json`). SELECT and ASK answer
/// `application/sparql-results+json`; CONSTRUCT answers
/// `application/n-triples`.
async fn store_sparql(
    State(d): State<Shared>,
    Path((org, store)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    body: String,
) -> Result<Response, ApiError> {
    let soul = d.soul_named(&org, &store).map_err(not_found)?.clone();
    let kind = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let text = match kind.as_str() {
        "application/sparql-query" => body,
        "application/json" => serde_json::from_str::<QueryBody>(&body).map_err(bad)?.query,
        other => {
            return Err(ApiError(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!(
                    "Content-Type {other:?} is not read here; send the query as                      application/sparql-query, or {{\"query\": ...}} as application/json"
                ),
            ))
        }
    };
    let (a, notes) = ask(soul, text).await?;
    let (ctype, out) = match a {
        Answer::Results(v) => ("application/sparql-results+json", v.to_string()),
        Answer::Triples(t) => {
            let mut s = String::new();
            for line in t {
                s.push_str(&line);
                s.push_str(" .\n");
            }
            ("application/n-triples", s)
        }
    };
    let mut resp = (StatusCode::OK, out).into_response();
    let h = resp.headers_mut();
    h.insert(axum::http::header::CONTENT_TYPE, ctype.parse().unwrap());
    // A standard body has no room for a note, so it rides a header.
    if !notes.is_empty() {
        if let Ok(v) = notes.join(" | ").parse() {
            h.insert("x-ravel-notes", v);
        }
    }
    Ok(resp)
}

/// `ravel optmem check`: the soul's memory against OptMem's rules and its
/// own records — every line passes the checks, every block due is built,
/// every note cites real turns, the wake is OptMem's cover.
async fn memory_check(
    State(d): State<Shared>,
    Path((org, store)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul_named(&org, &store).map_err(not_found)?.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let log = crate::memory_log::MemoryLog::for_soul(&soul.path);
        let v = crate::memory_relive::verify(&soul.path, &log)?;
        Ok(json!({ "store": soul.name(), "check": v }))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(Json(v))
}

async fn shutdown(State(d): State<Shared>) -> Json<Value> {
    d.shutdown.notify_one();
    Json(json!({ "ok": true, "stopping": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::config::DaemonConfig;

    /// A raveld with no souls and an empty key list, on a port of its own.
    async fn stand_up() -> String {
        let dir = std::env::temp_dir().join(format!("ravel-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("jwks.json"), r#"{"keys":[]}"#).unwrap();
        let cfg = DaemonConfig {
            path: dir.join("config.yml"),
            present: false,
            port: 0,
            interval_secs: 3600,
            souls: vec![],
            human: None,
            memory: None,
            warnings: vec![],
        };
        let mut d = Daemon::open(cfg).unwrap();
        d.keys = tessera::HostKeys::new(dir.join("jwks.json"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(Arc::new(d))).await.unwrap() });
        format!("http://{addr}")
    }

    async fn status(req: reqwest::RequestBuilder) -> (u16, bool) {
        let r = req.send().await.unwrap();
        (r.status().as_u16(), r.headers().contains_key("www-authenticate"))
    }

    #[tokio::test]
    async fn the_gate_stands_in_front_of_every_route() {
        let base = stand_up().await;
        let c = reqwest::Client::new();
        // Open to anyone on the machine.
        assert_eq!(status(c.get(format!("{base}/ravel/health"))).await.0, 200);
        assert_eq!(status(c.get(format!("{base}/ravel/stores"))).await.0, 200);
        // No token: stopping raveld and writing are refused, and say how to ask.
        assert_eq!(status(c.post(format!("{base}/ravel/shutdown"))).await, (401, true));
        assert_eq!(status(c.post(format!("{base}/ravel/sync"))).await, (401, true));
        assert_eq!(status(c.post(format!("{base}/ravel/7r1pl3f0rc3/nobody/sync"))).await, (401, true));
        // No token on a read passes the gate for now; no such soul, so 404.
        assert_eq!(status(c.get(format!("{base}/ravel/7r1pl3f0rc3/nobody/stats"))).await.0, 404);
        // A token that is sent and is not good is refused, reads included.
        let bad = c.get(format!("{base}/ravel/7r1pl3f0rc3/nobody/stats")).bearer_auth("abc.def.ghi");
        assert_eq!(status(bad).await, (401, true));
        let basic = c
            .get(format!("{base}/ravel/7r1pl3f0rc3/nobody/stats"))
            .header("authorization", "Basic abc");
        assert_eq!(status(basic).await.0, 401);
        // /health still answers after all of that: nothing was stopped.
        let h: Value = c.get(format!("{base}/ravel/health")).send().await.unwrap().json().await.unwrap();
        assert_eq!(h["identity"]["reads_need_a_token"], false);
        assert_eq!(h["identity"]["squaddies_with_a_key"], 0);
    }
}
