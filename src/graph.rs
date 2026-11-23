//! The persistent graph: annotations from many sessions accumulate on disk into
//! one queryable oxigraph store — the "see graphs of sessions" arm.
//!
//! Three w4r3z-flagged gotchas are handled here, on purpose:
//!
//! 1. **Idempotency on re-ingest.** Annotation IRIs are deterministic (sha256
//!    over identity, `annotate` mod), so re-ingesting the same transcript
//!    overwrites the same triples rather than duplicating. We ALSO clear the
//!    transcript's named graph before reload, so a *removed* key (edited source)
//!    doesn't leave a ghost. Verified by the idempotent-reingest test.
//!
//! 2. **Named-graph read-wrapping.** Each transcript's annotations live in their
//!    OWN named graph `ravel:graph/<transcript_id>` (a per-session provenance
//!    unit — droppable, re-ingestable atomically). A naked default-graph query
//!    will NOT see them; every query here wraps in `GRAPH ?g { … }`. (Pool learned
//!    this the hard way — chevron.rs:707.)
//!
//! 3. **Same registry / partition.** The partition string
//!    (`https://repolex.ai/ravel/Turn/` from the soul adapter) is threaded
//!    through unchanged; the engine never invents it. Soul scoping is BY STORE,
//!    not by subject — see `soul.rs`.

use crate::annotate::annotation_nt;
use crate::project::project_nt;
use crate::reader::Annotation;
use crate::names::Names;
use crate::{Event, RAVEL_NS};
use anyhow::{Context, Result};
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{
    GraphName, GraphNameRef, Literal, NamedNode, NamedOrBlankNode, NamedOrBlankNodeRef, Quad,
};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use std::path::Path;

/// The named graph for one transcript's annotations. Lives under the
/// instance base (`…/ravel/NamedGraph/<id>`) — NOT the
/// ontology namespace, which is vocabulary-only.
pub fn transcript_graph_iri(names: &Names, transcript_id: &str) -> String {
    names.graph(&safe(transcript_id))
}

/// What a named graph is, said in the store's unnamed graph (named-graph
/// spec section 12.2). ravel's two classes both sit under
/// `subtexture:DefaultGraph`, which is why a question with no GRAPH sees
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphKind {
    Transcript,
    Memory,
    /// The list of the newest turns. A plain `ravel:NamedGraph`: it is no
    /// transcript, and holds no statement a transcript graph also holds.
    Recent,
}

impl GraphKind {
    pub fn class(self) -> String {
        match self {
            GraphKind::Transcript => format!("{RAVEL_NS}TranscriptGraph"),
            GraphKind::Memory => format!("{RAVEL_NS}MemoryGraph"),
            GraphKind::Recent => format!("{RAVEL_NS}NamedGraph"),
        }
    }
}

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// The statements that describe one graph: its class, its owner when the
/// store has one, and `private`. The subject is the graph's own address.
/// Every graph says its owner, so a reader that checks access never has to
/// guess whose a transcript is.
pub fn description(names: &Names, graph: &NamedNode, kind: GraphKind) -> Vec<Quad> {
    let s = NamedOrBlankNode::NamedNode(graph.clone());
    let p = |name: &str| NamedNode::new_unchecked(format!("{RAVEL_NS}{name}"));
    let mut out = vec![
        Quad::new(
            s.clone(),
            NamedNode::new_unchecked(RDF_TYPE),
            NamedNode::new_unchecked(kind.class()),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            s.clone(),
            p("visibility"),
            Literal::new_simple_literal("private"),
            GraphName::DefaultGraph,
        ),
    ];
    if let Some(owner) = names.owner() {
        out.push(Quad::new(
            s,
            p("owner"),
            NamedNode::new_unchecked(owner),
            GraphName::DefaultGraph,
        ));
    }
    out
}

/// Write a graph's description. Safe to repeat.
pub fn describe_graph(
    store: &Store,
    names: &Names,
    graph: &NamedNode,
    kind: GraphKind,
) -> Result<()> {
    for q in description(names, graph, kind) {
        store.insert(&q)?;
    }
    Ok(())
}

/// Empty a graph and take its description away with it: a graph that holds
/// nothing must not still be announced.
pub fn drop_graph(store: &Store, graph: &NamedNode) -> Result<()> {
    store.clear_graph(GraphNameRef::NamedNode(graph.as_ref()))?;
    let said: Vec<Quad> = store
        .quads_for_pattern(
            Some(NamedOrBlankNodeRef::NamedNode(graph.as_ref())),
            None,
            None,
            Some(GraphNameRef::DefaultGraph),
        )
        .collect::<Result<_, _>>()?;
    for q in &said {
        store.remove(q)?;
    }
    Ok(())
}

/// Open (or create) the persistent store at `path`. Read-WRITE — takes the
/// exclusive RocksDB lock; a concurrent reader must use `open_read_only`.
pub fn open(path: impl AsRef<Path>) -> Result<Store> {
    Store::open(path.as_ref())
        .with_context(|| format!("open oxigraph store at {}", path.as_ref().display()))
}

// ───────────────── readers and the writer never overlap ─────────────────
//
// A read-only handle is a second RocksDB instance over the directory the
// writer is changing, with no snapshot. When the writer flushes or compacts,
// a file the reader is iterating can vanish; the iterator then reports an
// error on EVERY later call and never ends
// (oxigraph 0.5.9 storage/rocksdb.rs: `if let Err(e) = self.iter.status()
// { return Some(Err(e)) }`), and spareval's joins push each error onto a
// Vec and keep reading (spareval 0.2.6 eval.rs, `errors.push`). One query
// caught that way grows by gigabytes until the process is killed: raveld,
// 2026-11-21, 84 GB and then 12.4 GB. Reproduced by
// `outside::tests::real_reader_beside_writer`; mechanism found with
// tr1pl3x.
//
// Until raveld keeps one shared Store per soul, every reader in this process
// holds [`reading`] for as long as its handle is open, and every writer
// holds [`writing`]: many readers together, or one writer alone.

#[derive(Default)]
struct Turnstile {
    state: std::sync::Mutex<TurnstileState>,
    changed: std::sync::Condvar,
}

#[derive(Default)]
struct TurnstileState {
    readers: usize,
    writing: bool,
    writers_waiting: usize,
}

fn turnstile(dir: &Path) -> std::sync::Arc<Turnstile> {
    static ALL: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, std::sync::Arc<Turnstile>>>,
    > = std::sync::OnceLock::new();
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    ALL.get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(key)
        .or_default()
        .clone()
}

/// Held by a reader of the store at `dir`; released when dropped.
pub struct Reading(std::sync::Arc<Turnstile>);
/// Held by the writer of the store at `dir`; released when dropped.
pub struct Writing(std::sync::Arc<Turnstile>);

/// Wait until no writer has the store at `dir` (or is waiting for it), then
/// hold it for reading alongside any other readers.
pub fn reading(dir: &Path) -> Reading {
    let t = turnstile(dir);
    {
        let mut s = t.state.lock().unwrap();
        while s.writing || s.writers_waiting > 0 {
            s = t.changed.wait(s).unwrap();
        }
        s.readers += 1;
    }
    Reading(t)
}

/// Wait until nobody has the store at `dir`, then hold it alone. A waiting
/// writer goes ahead of readers that arrive after it.
pub fn writing(dir: &Path) -> Writing {
    let t = turnstile(dir);
    {
        let mut s = t.state.lock().unwrap();
        s.writers_waiting += 1;
        while s.writing || s.readers > 0 {
            s = t.changed.wait(s).unwrap();
        }
        s.writers_waiting -= 1;
        s.writing = true;
    }
    Writing(t)
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().readers -= 1;
        self.0.changed.notify_all();
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().writing = false;
        self.0.changed.notify_all();
    }
}

/// Open the store read-only (shared lock — many readers, or a reader beside a
/// writer). The path must already exist.
pub fn open_read_only(path: impl AsRef<Path>) -> Result<Store> {
    // A read-only open lists the store's log files while the writer may be
    // rotating them; one vanishing mid-open fails it with "No such file …
    // .log" (selkie, 2026-11-01). The next open sees the new set, so it is
    // tried five times over about two seconds, then fails loud. One shared
    // handle per soul in raveld would end the race; this bounds it.
    let mut last = None;
    for k in 0..5 {
        match Store::open_read_only(path.as_ref()) {
            Ok(s) => return Ok(s),
            Err(e) => {
                let gone = e.to_string().contains("No such file");
                last = Some(e);
                if !gone {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100 << k.min(3)));
            }
        }
    }
    Err(last.expect("tried at least once")).with_context(|| {
        format!(
            "open read-only oxigraph store at {}",
            path.as_ref().display()
        )
    })
}

/// Ingest one transcript's Turn nodes AND its annotations into its named graph,
/// idempotently.
///
/// Both halves go into the same graph on purpose: the annotations carry
/// soul-prefixed `ravel:atTurn` / `prov:used` references to Turn IRIs, and the
/// Turn NODES carry the federation TIME anchor (`ravel:timestamp` xsd:dateTime).
/// Ingesting annotations alone would leave those references dangling and the
/// soul+time cross-store join would have soul but not time — verified the hard
/// way (a fed-probe found 32 soul-prefixed anchors but 0 actual Turn nodes).
///
/// Clears the transcript's named graph first (so a dropped key doesn't ghost),
/// then loads the freshly-projected N-Triples INTO that named graph via
/// `with_default_graph`. Returns the triple count now in that graph.
pub fn ingest_annotations(
    store: &Store,
    events: &[Event],
    anns: &[Annotation],
    transcript_id: &str,
    names: &Names,
) -> Result<usize> {
    let graph_iri = transcript_graph_iri(names, transcript_id);
    let graph = NamedNode::new(&graph_iri).context("transcript graph IRI")?;
    let partition = names.turn_partition();

    // (1)+(2): wipe this transcript's graph so re-ingest is a clean replace.
    store.clear_graph(GraphNameRef::NamedNode(graph.as_ref()))?;

    // Turn nodes (the time anchor) + annotations (the signal), same graph. Only
    // project events that an annotation actually references, so the store stays
    // annotation-scoped — but every referenced Turn is present with its timestamp.
    let referenced: std::collections::HashSet<&str> =
        anns.iter().map(|a| a.event_id.as_str()).collect();
    let anchor_events: Vec<Event> = events
        .iter()
        .filter(|e| referenced.contains(e.event_id.as_str()))
        .cloned()
        .collect();

    let mut nt = project_nt(&anchor_events, &partition, &crate::project::Who::of(names))?;
    nt.push_str(&annotation_nt(anns, transcript_id, names)?);
    let parser = RdfParser::from_format(RdfFormat::NTriples).with_default_graph(graph.clone());
    store.load_from_reader(parser, nt.as_bytes())?;
    describe_graph(store, names, &graph, GraphKind::Transcript)?;
    store.flush()?;

    // count what's in this graph now
    Ok(store
        .quads_for_pattern(
            None,
            None,
            None,
            Some(GraphNameRef::NamedNode(graph.as_ref())),
        )
        .count())
}

/// Ingest a transcript's FULL SPINE (every Turn node) plus its annotations into
/// its named graph, idempotently. Same shape as [`ingest_annotations`], but it
/// does NOT filter events to just the reader-anchored ones — the whole
/// conversation lands in the graph, not only the turns a detector happened to
/// touch.
///
/// This is the difference between "the meaning layer" (annotations + the handful
/// of turns they point at) and "the queryable conversation" (every turn, chained
/// by reply-to, each with its timestamp and text). Use this when you want to ask
/// spine questions — "walk the reply-to chain", "how many turns", "the text of
/// turn N" — not just "what did a reader find". The federation TIME anchor is now
/// present on *every* turn, not only annotated ones.
///
/// Returns the triple count now in that graph. Idempotent by the same mechanism
/// (deterministic IRIs + clear-graph-before-reload).
pub fn ingest_transcript(
    store: &Store,
    events: &[Event],
    anns: &[Annotation],
    transcript_id: &str,
    names: &Names,
) -> Result<usize> {
    let graph_iri = transcript_graph_iri(names, transcript_id);
    let graph = NamedNode::new(&graph_iri).context("transcript graph IRI")?;
    let partition = names.turn_partition();

    // clean replace (gotchas #1 + #2, as in ingest_annotations)
    store.clear_graph(GraphNameRef::NamedNode(graph.as_ref()))?;

    // FULL spine: project EVERY event, not just the annotation-referenced ones.
    let mut nt = project_nt(events, &partition, &crate::project::Who::of(names))?;
    nt.push_str(&annotation_nt(anns, transcript_id, names)?);
    let parser = RdfParser::from_format(RdfFormat::NTriples).with_default_graph(graph.clone());
    store.load_from_reader(parser, nt.as_bytes())?;
    describe_graph(store, names, &graph, GraphKind::Transcript)?;
    keep_recent(store, names, events)?;
    store.flush()?;

    Ok(store
        .quads_for_pattern(
            None,
            None,
            None,
            Some(GraphNameRef::NamedNode(graph.as_ref())),
        )
        .count())
}

/// Add turns to a transcript graph that already holds the earlier ones.
///
/// What a pass does when a transcript has only grown at its end: the new
/// lines' turns are loaded and nothing is cleared. A turn's address is made
/// from its id, so loading one that is already there changes nothing.
/// [`ingest_transcript`] is still what loads a transcript the first time,
/// and again whenever its earlier bytes are not what was loaded before.
pub fn append_transcript(
    store: &Store,
    events: &[Event],
    anns: &[Annotation],
    transcript_id: &str,
    names: &Names,
) -> Result<()> {
    let graph_iri = transcript_graph_iri(names, transcript_id);
    let graph = NamedNode::new(&graph_iri).context("transcript graph IRI")?;
    let mut nt = project_nt(events, &names.turn_partition(), &crate::project::Who::of(names))?;
    nt.push_str(&annotation_nt(anns, transcript_id, names)?);
    let parser = RdfParser::from_format(RdfFormat::NTriples).with_default_graph(graph.clone());
    store.load_from_reader(parser, nt.as_bytes())?;
    describe_graph(store, names, &graph, GraphKind::Transcript)?;
    keep_recent(store, names, events)?;
    store.flush()?;
    Ok(())
}

/// How many turns the `recent` graph lists.
pub const RECENT_TURNS: usize = 500;

const RDFS_MEMBER: &str = "http://www.w3.org/2000/01/rdf-schema#member";

/// A turn's time as a number, for telling newer from older whatever zone or
/// precision it was written in. A time that cannot be read sorts oldest.
fn instant(ts: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| t.timestamp_millis())
        .unwrap_or(i64::MIN)
}

/// Keep `{root}ravel/NamedGraph/recent` listing the store's newest
/// [`RECENT_TURNS`] turns, given the turns just loaded.
///
/// Why it exists: the store has no index by time, so "the newest 40 turns"
/// asked of the whole store reads every turn there is and sorts them (0.4 s
/// to 4 s a question, asked every few seconds by a panel that shows them).
/// Asked of this graph it sorts 500:
///
/// ```sparql
/// SELECT ?t ?ts ?role ?text WHERE {
///   GRAPH <ravel/NamedGraph/recent> { ?list rdfs:member ?t }
///   ?t ravel:timestamp ?ts ; ravel:role ?role . OPTIONAL { ?t ravel:text ?text }
/// } ORDER BY DESC(?ts) LIMIT 40
/// ```
///
/// What it holds is one statement a turn, `<the graph> rdfs:member <turn>`,
/// and nothing else. It does not hold a copy of the turns: a question with
/// no GRAPH is asked of every graph together, and a statement stored in two
/// graphs comes back twice, so copies would show the newest turns double in
/// every ordinary question (tested, 2026-11-22). A turn's own statements
/// stay only in its transcript's graph.
///
/// The graph is described as a `ravel:NamedGraph` with the store's owner and
/// `private` (direction by goodlux, 2026-11-22: a sliding window in a named
/// graph, as git-lex keeps `git-lex/NamedGraph/now`, and no per-day property).
///
/// Listed once from the whole store when the graph is not there yet; after
/// that only the turns handed in are weighed against the ones listed.
pub fn keep_recent(store: &Store, names: &Names, loaded: &[Event]) -> Result<()> {
    use oxigraph::model::{NamedNodeRef, TermRef};
    let graph = NamedNode::new(names.recent_graph()).context("recent graph IRI")?;
    let g = GraphNameRef::NamedNode(graph.as_ref());
    let member = NamedNodeRef::new_unchecked(RDFS_MEMBER);
    let ts_p = NamedNode::new_unchecked(format!("{RAVEL_NS}timestamp"));
    let described = store
        .quads_for_pattern(
            Some(NamedOrBlankNodeRef::NamedNode(graph.as_ref())),
            Some(NamedNodeRef::new_unchecked(RDF_TYPE)),
            None,
            Some(GraphNameRef::DefaultGraph),
        )
        .next()
        .is_some();
    if described && loaded.is_empty() {
        return Ok(());
    }
    // (turn, time) for every candidate: the turns listed now, or on the
    // first time the store's newest; and the turns just loaded.
    let mut cand: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut listed: std::collections::HashSet<String> = std::collections::HashSet::new();
    if described {
        for q in store.quads_for_pattern(None, Some(member), None, Some(g)) {
            let oxigraph::model::Term::NamedNode(t) = q?.object else {
                continue;
            };
            let when = store
                .quads_for_pattern(
                    Some(NamedOrBlankNodeRef::NamedNode(t.as_ref())),
                    Some(ts_p.as_ref()),
                    None,
                    None,
                )
                .filter_map(|q| q.ok())
                .map(|q| instant(&term_value(&q.object)))
                .max();
            listed.insert(t.as_str().to_string());
            // A turn whose transcript has left the store drops off the list.
            if let Some(when) = when {
                cand.insert(t.into_string(), when);
            }
        }
    } else {
        let q = format!(
            "SELECT ?t ?ts WHERE {{ GRAPH ?g {{ ?t a <{RAVEL_NS}Turn> ; <{RAVEL_NS}timestamp> ?ts }} }} \
             ORDER BY DESC(?ts) LIMIT {}",
            RECENT_TURNS * 2
        );
        if let QueryResults::Solutions(rows) =
            SparqlEvaluator::new().parse_query(&q)?.on_store(store).execute()?
        {
            for row in rows {
                let row = row?;
                if let (Some(oxigraph::model::Term::NamedNode(t)), Some(ts)) =
                    (row.get("t"), row.get("ts"))
                {
                    cand.insert(t.as_str().to_string(), instant(&term_value(ts)));
                }
            }
        }
    }
    for e in loaded {
        if let Some(ts) = &e.timestamp {
            cand.insert(names.turn(&e.event_id), instant(ts));
        }
    }
    let mut newest: Vec<(i64, String)> = cand.into_iter().map(|(t, w)| (w, t)).collect();
    newest.sort_by(|a, b| b.cmp(a));
    newest.truncate(RECENT_TURNS);
    let keep: std::collections::HashSet<&str> = newest.iter().map(|(_, t)| t.as_str()).collect();
    let quad = |turn: &str| {
        Quad::new(
            NamedOrBlankNode::NamedNode(graph.clone()),
            member.into_owned(),
            NamedNode::new_unchecked(turn),
            GraphName::NamedNode(graph.clone()),
        )
    };
    for gone in listed.iter().filter(|t| !keep.contains(t.as_str())) {
        store.remove(&quad(gone))?;
    }
    for new in keep.iter().filter(|t| !listed.contains(**t)) {
        store.insert(&quad(new))?;
    }
    describe_graph(store, names, &graph, GraphKind::Recent)?;
    let _ = TermRef::from(member);
    Ok(())
}

/// A cross-session query result row: one harvested emojikey with provenance.
#[derive(Debug, Clone)]
pub struct EmojikeyHit {
    pub transcript: String,
    pub source_kind: String,
    pub me: String,
    pub content: String,
    pub you: String,
    pub ts: String,
}

/// Cross-session query: every emojikey annotation in the store, read back
/// THROUGH the triple term, across ALL transcript named graphs. Optionally
/// filter to a single `source_kind` (e.g. "authored" for live emissions only).
///
/// This is the "see graphs of sessions" query — it reaches into every session's
/// named graph (`GRAPH ?g`) and pulls the emojikeys the readers found, with the
/// session they came from and whether they were authored or quoted.
/// Run a SELECT and return each row as variable → plain value (an IRI's
/// string, a literal's lexical form). For callers that want values, not
/// RDF terms.
pub fn query_rows(
    store: &Store,
    q: &str,
) -> Result<Vec<std::collections::HashMap<String, String>>> {
    let QueryResults::Solutions(solutions) = SparqlEvaluator::new()
        .parse_query(q)?
        .on_store(store)
        .execute()?
    else {
        anyhow::bail!("expected SELECT solutions");
    };
    let mut rows = Vec::new();
    for sol in solutions {
        let sol = sol?;
        rows.push(
            sol.iter()
                .map(|(v, t)| (v.as_str().to_string(), term_value(t)))
                .collect(),
        );
    }
    Ok(rows)
}

pub fn query_emojikeys(store: &Store, only_source_kind: Option<&str>) -> Result<Vec<EmojikeyHit>> {
    let filter = match only_source_kind {
        Some(sk) => format!(r#"FILTER(?sk = "{sk}")"#),
        None => String::new(),
    };
    // ?g is the transcript graph; strip the graph-IRI prefix to a readable id.
    let q = format!(
        r#"
        PREFIX ravel: <{RAVEL_NS}>
        PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
        PREFIX oa: <http://www.w3.org/ns/oa#>
        PREFIX prov: <http://www.w3.org/ns/prov#>
        SELECT ?g ?sk ?me ?content ?you ?ts WHERE {{
            GRAPH ?g {{
                ?claim rdf:reifies <<( ?event ravel:exhibits "emojikey/harvest" )>> ;
                       prov:wasDerivedFrom ?ann .
                ?ann oa:hasBody ?body .
                OPTIONAL {{ ?ann ravel:textOrigin ?sk }}
                OPTIONAL {{ ?ann prov:generatedAtTime ?ts }}
                ?body ravel:sig_me ?me ; ravel:sig_content ?content ; ravel:sig_you ?you .
                {filter}
            }}
        }}
        ORDER BY ?g ?ts
        "#
    );
    let results = SparqlEvaluator::new()
        .parse_query(&q)?
        .on_store(store)
        .execute()?;
    let QueryResults::Solutions(solutions) = results else {
        anyhow::bail!("expected SELECT solutions");
    };
    let mut hits = Vec::new();
    for sol in solutions {
        let sol = sol?;
        let get = |k: &str| sol.get(k).map(term_value).unwrap_or_default();
        let g = get("g");
        // Whatever root the store is under, the transcript id is what follows.
        let transcript = g
            .rsplit_once("/NamedGraph/")
            .map_or(g.as_str(), |(_, id)| id)
            .to_string();
        hits.push(EmojikeyHit {
            transcript,
            source_kind: get("sk"),
            me: get("me"),
            content: get("content"),
            you: get("you"),
            ts: get("ts"),
        });
    }
    Ok(hits)
}

/// Unwrap an oxigraph term's lexical value (strip IRI `<>` / literal quotes).
fn term_value(t: &oxigraph::model::Term) -> String {
    use oxigraph::model::Term;
    match t {
        Term::NamedNode(n) => n.as_str().to_string(),
        Term::Literal(l) => l.value().to_string(),
        other => other.to_string(),
    }
}

fn safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {

    #[test]
    fn readers_share_a_store_and_a_writer_has_it_alone() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!("ravel-turnstile-{}", std::process::id()));
        let (inside, overlaps) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        // Two readers at once is allowed.
        let (a, b) = (super::reading(&dir), super::reading(&dir));
        drop((a, b));
        let mut threads = Vec::new();
        for k in 0..8 {
            let (dir, inside, overlaps) = (dir.clone(), inside.clone(), overlaps.clone());
            threads.push(std::thread::spawn(move || {
                for _ in 0..200 {
                    if k % 2 == 0 {
                        let _w = super::writing(&dir);
                        // A writer must find nobody inside, and nobody may enter.
                        if inside.fetch_add(1000, Ordering::SeqCst) != 0 {
                            overlaps.fetch_add(1, Ordering::SeqCst);
                        }
                        std::thread::yield_now();
                        inside.fetch_sub(1000, Ordering::SeqCst);
                    } else {
                        let _r = super::reading(&dir);
                        if inside.fetch_add(1, Ordering::SeqCst) >= 1000 {
                            overlaps.fetch_add(1, Ordering::SeqCst);
                        }
                        std::thread::yield_now();
                        inside.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(overlaps.load(Ordering::SeqCst), 0);
    }
    use super::*;
    use crate::reader::emojikey_read;
    use crate::{Event, SourceKind, TextSpan};

    /// The newest turns are listed, the list slides, and listing them does
    /// not make any turn come back twice from an ordinary question.
    #[test]
    fn the_recent_graph_lists_the_newest_turns_and_doubles_nothing() {
        let store = Store::new().unwrap();
        let names = Names::new("7r1pl3f0rc3", "spacegoat").unwrap();
        let line = |i: usize| {
            format!(
                r#"{{"type":"user","uuid":"u{i:04}","timestamp":"2026-11-10T{:02}:{:02}:{:02}Z","message":{{"role":"user","content":"turn {i}"}}}}"#,
                i / 3600,
                (i / 60) % 60,
                i % 60
            )
        };
        let events = |r: std::ops::Range<usize>| {
            crate::adapter::parse_transcript(&r.map(line).collect::<Vec<_>>().join("\n")).unwrap()
        };
        ingest_transcript(&store, &events(0..400), &[], "s1", &names).unwrap();
        let n = |q: &str| query_rows(&store, q).unwrap().len();
        let recent = names.recent_graph();
        let listed = format!("SELECT ?t WHERE {{ GRAPH <{recent}> {{ ?l <{RDFS_MEMBER}> ?t }} }}");
        assert_eq!(n(&listed), 400);
        // A second transcript, newer: the list slides to the newest 500.
        append_transcript(&store, &events(400..700), &[], "s2", &names).unwrap();
        assert_eq!(n(&listed), RECENT_TURNS);
        let ends = |order: &str| {
            let q = format!(
                "SELECT ?t WHERE {{ GRAPH <{recent}> {{ ?l <{RDFS_MEMBER}> ?t }} GRAPH ?g {{ ?t <{RAVEL_NS}timestamp> ?ts }} }} ORDER BY {order} LIMIT 1"
            );
            query_rows(&store, &q).unwrap()[0]["t"].clone()
        };
        assert!(ends("DESC(?ts)").ends_with("/ravel/Turn/u0699"));
        assert!(ends("?ts").ends_with("/ravel/Turn/u0200"));
        // Every turn is still stored once: asked of all graphs together.
        let mut q = SparqlEvaluator::new()
            .parse_query(&format!("SELECT ?t WHERE {{ ?t a <{RAVEL_NS}Turn> }}"))
            .unwrap();
        q.dataset_mut().set_default_graph_as_union();
        let QueryResults::Solutions(rows) = q.on_store(&store).execute().unwrap() else {
            panic!()
        };
        assert_eq!(rows.count(), 700);
        // It is described as a plain named graph, private, with its owner.
        let said: Vec<String> = query_rows(&store, &format!("SELECT ?o WHERE {{ <{recent}> ?p ?o }}"))
            .unwrap()
            .into_iter()
            .map(|r| r["o"].clone())
            .collect();
        assert!(said.contains(&format!("{RAVEL_NS}NamedGraph")), "{said:?}");
        assert!(said.contains(&"private".to_string()));
        assert!(said.iter().any(|o| o.ends_with("/soul/Soul/spacegoat")));
    }

    fn ev(id: &str, text: &str, kind: SourceKind) -> Event {
        let span = TextSpan {
            start: 0,
            end: text.len(),
            kind,
        };
        Event {
            event_id: id.into(),
            parent_id: None,
            role: "assistant".into(),
            timestamp: Some("2026-07-01T00:00:00Z".into()),
            text: Some(text.into()),
            text_provenance: vec![span],
            thinking: None,
            model: None,
            model_setting: None,
            wrote: crate::Wrote::Nobody,
            actions: Vec::new(),
            outcomes: Vec::new(),
        }
    }

    #[test]
    fn triple_term_survives_named_graph_and_reingest_is_idempotent() {
        let store = Store::new().unwrap(); // in-memory is the same API as on-disk
        let evs = vec![ev(
            "e1",
            "[ME|🧠]~[CONTENT|💻]~[YOU|🎓]",
            SourceKind::Authored,
        )];
        let anns = emojikey_read(&evs);
        assert_eq!(anns.len(), 1);

        // ingest twice — idempotent: triple count stable, no duplication.
        let n1 = ingest_annotations(&store, &evs, &anns, "sess-A", &crate::names::Names::bare()).unwrap();
        let n2 = ingest_annotations(&store, &evs, &anns, "sess-A", &crate::names::Names::bare()).unwrap();
        assert!(n1 > 0);
        assert_eq!(n1, n2, "re-ingest must not change the graph (idempotent)");

        // the referenced Turn NODE must be present with its time anchor — else
        // the federation join has the anchor edge but not time (the
        // dangling-Turn bug this ingest fixes).
        let turn_q = format!(
            r#"PREFIX ravel: <{RAVEL_NS}>
               PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
               ASK {{ GRAPH ?g {{ ?t rdf:type ravel:Turn ; ravel:timestamp ?ts .
                      FILTER(STRSTARTS(STR(?t), "https://repolex.ai/ravel/Turn/")) }} }}"#
        );
        let ask = SparqlEvaluator::new()
            .parse_query(&turn_q)
            .unwrap()
            .on_store(&store)
            .execute()
            .unwrap();
        assert!(
            matches!(ask, QueryResults::Boolean(true)),
            "the Turn node with its timestamp must be in the graph"
        );

        // GOTCHA #2: the triple term must be readable THROUGH the named graph.
        let hits = query_emojikeys(&store, None).unwrap();
        assert_eq!(
            hits.len(),
            1,
            "one emojikey, read back through GRAPH + triple term"
        );
        assert_eq!(hits[0].transcript, "sess-A");
        assert_eq!(hits[0].source_kind, "authored");
        assert_eq!(hits[0].me, "🧠");

        // provenance filter: authored-only keeps it; tool_result-only drops it.
        assert_eq!(query_emojikeys(&store, Some("authored")).unwrap().len(), 1);
        assert_eq!(
            query_emojikeys(&store, Some("tool_result")).unwrap().len(),
            0
        );
    }

    #[test]
    fn two_sessions_accumulate_and_are_separable() {
        let store = Store::new().unwrap();
        let eva = vec![ev("e1", "[ME|a]~[CONTENT|b]~[YOU|c]", SourceKind::Authored)];
        let evb = vec![ev(
            "e2",
            "[ME|x]~[CONTENT|y]~[YOU|z]",
            SourceKind::ToolResult,
        )];
        let a = emojikey_read(&eva);
        let b = emojikey_read(&evb);
        ingest_annotations(&store, &eva, &a, "sess-A", &crate::names::Names::bare()).unwrap();
        ingest_annotations(&store, &evb, &b, "sess-B", &crate::names::Names::bare()).unwrap();

        let all = query_emojikeys(&store, None).unwrap();
        assert_eq!(all.len(), 2, "two sessions accumulate in one store");
        // separable by named graph → by transcript
        let transcripts: Vec<_> = all.iter().map(|h| h.transcript.as_str()).collect();
        assert!(transcripts.contains(&"sess-A") && transcripts.contains(&"sess-B"));
        // and by provenance across sessions
        assert_eq!(query_emojikeys(&store, Some("authored")).unwrap().len(), 1);
    }

    #[test]
    fn full_spine_stores_every_turn_not_just_annotated_ones() {
        // The whole point of ingest_transcript vs ingest_annotations: a session
        // with turns that NO reader touched still lands in the graph. Here two of
        // three turns carry no emojikey — annotation-scoped ingest would drop
        // them; full-spine keeps all three.
        let store = Store::new().unwrap();
        let evs = vec![
            ev("e1", "just some prose, no key here", SourceKind::Authored),
            ev("e2", "[ME|🧠]~[CONTENT|💻]~[YOU|🎓]", SourceKind::Authored),
            ev("e3", "more prose, still no key", SourceKind::Authored),
        ];
        let anns = emojikey_read(&evs);
        assert_eq!(anns.len(), 1, "only the middle turn has an emojikey");

        ingest_transcript(&store, &evs, &anns, "sess-full", &crate::names::Names::bare()).unwrap();

        // ALL THREE turns are present as spine nodes (not just the annotated one).
        let count_q = format!(
            r#"PREFIX ravel: <{RAVEL_NS}>
               SELECT (COUNT(?t) AS ?n) WHERE {{ GRAPH ?g {{ ?t a ravel:Turn }} }}"#
        );
        let res = SparqlEvaluator::new()
            .parse_query(&count_q)
            .unwrap()
            .on_store(&store)
            .execute()
            .unwrap();
        if let QueryResults::Solutions(mut s) = res {
            let row = s.next().unwrap().unwrap();
            let n = row.get("n").unwrap().to_string();
            assert!(n.contains('3'), "all 3 spine turns must be stored, got {n}");
        } else {
            panic!("expected solutions");
        }

        // and the emojikey annotation still rides on top — spine + belief coexist.
        assert_eq!(query_emojikeys(&store, None).unwrap().len(), 1);
    }

    /// Two souls' stores once held exactly 2,500 transcript graphs each
    /// (goodlux asked, 2026-11-20, whether that was a ceiling). It is not:
    /// a store on disk takes 2,600 and reports every one.
    #[test]
    fn a_store_has_no_ceiling_at_2500_graphs() {
        let dir = std::env::temp_dir().join(format!("ravel-2600-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = open(&dir).unwrap();
        let names = Names::new("7r1pl3f0rc3", "spacegoat").unwrap();
        for i in 0..2600 {
            let evs = vec![ev(&format!("t{i}"), "hello", SourceKind::Authored)];
            ingest_transcript(&store, &evs, &[], &format!("sess-{i}"), &names).unwrap();
        }
        let n = |q: &str| query_rows(&store, q).unwrap()[0]["n"].clone();
        // 2,600 transcripts and the list of the newest turns.
        assert_eq!(n("SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }"), "2601");
        assert_eq!(n("SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE { ?g <https://repolex.ai/ontology/ravel/visibility> ?v }"), "2601");
        assert_eq!(store.named_graphs().count(), 2601);
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_graph_is_described_while_it_holds_something_and_not_after() {
        let store = Store::new().unwrap();
        let names = Names::new("7r1pl3f0rc3", "spacegoat").unwrap();
        let evs = vec![ev("only", "hello", SourceKind::Authored)];
        ingest_transcript(&store, &evs, &[], "sess-1", &names).unwrap();
        ingest_transcript(&store, &evs, &[], "sess-1", &names).unwrap();
        let said = |store: &Store| {
            // (The list of the newest turns is described too; not what
            // this test is about.)
            query_rows(store, "SELECT ?g ?p ?o WHERE { ?g ?p ?o FILTER(!STRENDS(STR(?g), '/NamedGraph/recent')) } ORDER BY ?p").unwrap()
        };
        // Three statements in the unnamed graph, once, however often it is ingested.
        let rows = said(&store);
        assert_eq!(rows.len(), 3, "{rows:?}");
        let g = "https://repolex.ai/7r1pl3f0rc3/spacegoat/ravel/NamedGraph/sess-1";
        assert!(rows.iter().all(|r| r["g"] == g));
        let objects: Vec<&str> = rows.iter().map(|r| r["o"].as_str()).collect();
        assert!(objects.contains(&"https://repolex.ai/ontology/ravel/TranscriptGraph"));
        assert!(objects.contains(&"private"));
        assert!(objects.contains(&"https://repolex.ai/7r1pl3f0rc3/spacegoat/soul/Soul/spacegoat"));
        // The turn is under the soul's own root.
        let t = query_rows(&store, "SELECT ?t WHERE { GRAPH ?g { ?t a <https://repolex.ai/ontology/ravel/Turn> } }").unwrap();
        assert_eq!(t[0]["t"], "https://repolex.ai/7r1pl3f0rc3/spacegoat/ravel/Turn/only");

        drop_graph(&store, &NamedNode::new(g).unwrap()).unwrap();
        assert!(said(&store).is_empty(), "a dropped graph is no longer announced");
        let left = query_rows(&store, &format!("SELECT ?s WHERE {{ GRAPH <{g}> {{ ?s ?p ?o }} }}")).unwrap();
        assert!(left.is_empty());
    }
}
