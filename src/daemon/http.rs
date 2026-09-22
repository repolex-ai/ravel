//! raveld's HTTP surface, on 127.0.0.1 only. `ravel` is its client.
//!
//!   GET  /health                      the daemon: version, uptime, souls, config problems
//!   GET  /souls                       every soul: id, path, last pass, last numbers
//!   POST /sync                        a pass over every soul, now
//!   POST /souls/{id}/sync             a pass over one soul, now
//!   GET  /souls/{id}/health           the read-only diagnostic (findings + attention flag)
//!   GET  /souls/{id}/stats            counts: quads, graphs, turns, dated turns, claims, top predicates
//!   POST /souls/{id}/query  {query}   SPARQL, W3C results JSON
//!   GET  /souls/{id}/emojikeys[?origin=authored]
//!   POST /shutdown                    stop, after answering
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
use std::collections::HashMap;
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

pub async fn serve(d: Shared) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/health", get(health))
        .route("/souls", get(souls))
        .route("/sync", post(sync_all))
        .route("/souls/{id}/sync", post(sync_one))
        .route("/souls/{id}/health", get(soul_health))
        .route("/souls/{id}/stats", get(soul_stats))
        .route("/souls/{id}/query", post(soul_query))
        .route("/souls/{id}/emojikeys", get(soul_emojikeys))
        .route("/shutdown", post(shutdown))
        .with_state(d.clone());
    let addr = format!("127.0.0.1:{}", d.cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    super::log(format!("listening on http://{addr}"));
    let stop = d.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => super::log("interrupted; stopping"),
                _ = stop.shutdown.notified() => super::log("asked to stop; stopping"),
            }
        })
        .await?;
    Ok(())
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
        "souls": d.souls.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        "passes": d.passes.load(std::sync::atomic::Ordering::Relaxed),
    }))
}

async fn souls(State(d): State<Shared>) -> Json<Value> {
    let rows: Vec<Value> = d
        .souls
        .iter()
        .map(|s| {
            let st = d.state_of(&s.id);
            json!({
                "id": s.id,
                "path": s.path.display().to_string(),
                "last_sync": st.last_sync,
                "last_error": st.last_error,
                "passes": st.passes,
                "last": st.last,
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
    Path(id): Path<String>,
    body: Option<Json<SyncBody>>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul(&id).map_err(not_found)?.clone();
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
        json!({ "id": soul.id, "path": soul.path.display().to_string(), "summary": s }),
    ))
}

async fn soul_health(
    State(d): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul(&id).map_err(not_found)?.clone();
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
        "id": soul.id,
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
            soul.id,
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
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul(&id).map_err(not_found)?.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let store = open_store(&soul)?;
        let g = "GRAPH ?g { ?s ?p ?o }";
        let quads = scalar(&store, &format!("SELECT (COUNT(*) AS ?n) WHERE {{ {g} }}"))?;
        let graphs = scalar(&store, &format!("SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE {{ {g} }}"))?;
        let subjects = scalar(&store, &format!("SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ {g} }}"))?;
        let turns = scalar(&store, "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } FILTER(STRSTARTS(STR(?s), \"https://repolex.ai/ravel/Turn/\")) }")?;
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
            "id": soul.id,
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

async fn soul_query(
    State(d): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<QueryBody>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul(&id).map_err(not_found)?.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let store = open_store(&soul)?;
        let results = SparqlEvaluator::new()
            .parse_query(&body.query)?
            .on_store(&store)
            .execute()?;
        Ok(match results {
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
                json!({ "head": { "vars": vars }, "results": { "bindings": rows } })
            }
            QueryResults::Boolean(b) => json!({ "head": {}, "boolean": b }),
            QueryResults::Graph(triples) => {
                let mut out = Vec::new();
                for t in triples {
                    out.push(t?.to_string());
                }
                json!({ "triples": out })
            }
        })
    })
    .await
    .map_err(internal)?
    .map_err(bad)?;
    Ok(Json(v))
}

async fn soul_emojikeys(
    State(d): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let soul = d.soul(&id).map_err(not_found)?.clone();
    let origin = q.get("origin").cloned();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let store = open_store(&soul)?;
        let hits = graph::query_emojikeys(&store, origin.as_deref())?;
        Ok(json!(hits
            .iter()
            .map(|h| json!({
                "transcript": h.transcript,
                "origin": h.source_kind,
                "me": h.me,
                "content": h.content,
                "you": h.you,
                "ts": h.ts,
            }))
            .collect::<Vec<_>>()))
    })
    .await
    .map_err(internal)?
    .map_err(bad)?;
    Ok(Json(v))
}

async fn shutdown(State(d): State<Shared>) -> Json<Value> {
    d.shutdown.notify_one();
    Json(json!({ "ok": true, "stopping": true }))
}
