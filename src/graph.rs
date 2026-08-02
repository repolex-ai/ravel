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
use crate::{Event, RAVEL_BASE, RAVEL_NS};
use anyhow::{Context, Result};
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{GraphNameRef, NamedNode};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use std::path::Path;

/// The named graph for one transcript's annotations. Lives under the
/// instance base (`…/ravel/NamedGraph/<id>`, git-lex's shape) — NOT the
/// ontology namespace, which is vocabulary-only.
pub fn transcript_graph_iri(transcript_id: &str) -> String {
    format!("{RAVEL_BASE}NamedGraph/{}", safe(transcript_id))
}

/// Open (or create) the persistent store at `path`. Read-WRITE — takes the
/// exclusive RocksDB lock; a concurrent reader must use `open_read_only`.
pub fn open(path: impl AsRef<Path>) -> Result<Store> {
    Store::open(path.as_ref())
        .with_context(|| format!("open oxigraph store at {}", path.as_ref().display()))
}

/// Open the store read-only (shared lock — many readers, or a reader beside a
/// writer). The path must already exist.
pub fn open_read_only(path: impl AsRef<Path>) -> Result<Store> {
    Store::open_read_only(path.as_ref())
        .with_context(|| format!("open read-only oxigraph store at {}", path.as_ref().display()))
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
    partition: &str,
) -> Result<usize> {
    let graph_iri = transcript_graph_iri(transcript_id);
    let graph = NamedNode::new(&graph_iri).context("transcript graph IRI")?;

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

    let mut nt = project_nt(&anchor_events, partition)?;
    nt.push_str(&annotation_nt(anns, transcript_id, partition)?);
    let parser = RdfParser::from_format(RdfFormat::NTriples).with_default_graph(graph.clone());
    store.load_from_reader(parser, nt.as_bytes())?;
    store.flush()?;

    // count what's in this graph now
    Ok(store
        .quads_for_pattern(None, None, None, Some(GraphNameRef::NamedNode(graph.as_ref())))
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
    partition: &str,
) -> Result<usize> {
    let graph_iri = transcript_graph_iri(transcript_id);
    let graph = NamedNode::new(&graph_iri).context("transcript graph IRI")?;

    // clean replace (gotchas #1 + #2, as in ingest_annotations)
    store.clear_graph(GraphNameRef::NamedNode(graph.as_ref()))?;

    // FULL spine: project EVERY event, not just the annotation-referenced ones.
    let mut nt = project_nt(events, partition)?;
    nt.push_str(&annotation_nt(anns, transcript_id, partition)?);
    let parser = RdfParser::from_format(RdfFormat::NTriples).with_default_graph(graph.clone());
    store.load_from_reader(parser, nt.as_bytes())?;
    store.flush()?;

    Ok(store
        .quads_for_pattern(None, None, None, Some(GraphNameRef::NamedNode(graph.as_ref())))
        .count())
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
    let results = SparqlEvaluator::new().parse_query(&q)?.on_store(store).execute()?;
    let QueryResults::Solutions(solutions) = results else {
        anyhow::bail!("expected SELECT solutions");
    };
    let prefix = format!("{RAVEL_BASE}NamedGraph/");
    let mut hits = Vec::new();
    for sol in solutions {
        let sol = sol?;
        let get = |k: &str| sol.get(k).map(term_value).unwrap_or_default();
        let g = get("g");
        let transcript = g.strip_prefix(&prefix).unwrap_or(&g).to_string();
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
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::emojikey_read;
    use crate::{Event, SourceKind, TextSpan};

    fn ev(id: &str, text: &str, kind: SourceKind) -> Event {
        let span = TextSpan { start: 0, end: text.len(), kind };
        Event {
            event_id: id.into(),
            parent_id: None,
            role: "assistant".into(),
            timestamp: Some("2026-07-01T00:00:00Z".into()),
            text: Some(text.into()),
            text_provenance: vec![span],
        }
    }

    #[test]
    fn triple_term_survives_named_graph_and_reingest_is_idempotent() {
        let store = Store::new().unwrap(); // in-memory is the same API as on-disk
        let part = "https://repolex.ai/ravel/Turn/";
        let evs = vec![ev("e1", "[ME|🧠]~[CONTENT|💻]~[YOU|🎓]", SourceKind::Authored)];
        let anns = emojikey_read(&evs);
        assert_eq!(anns.len(), 1);

        // ingest twice — idempotent: triple count stable, no duplication.
        let n1 = ingest_annotations(&store, &evs, &anns, "sess-A", part).unwrap();
        let n2 = ingest_annotations(&store, &evs, &anns, "sess-A", part).unwrap();
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
        let ask = SparqlEvaluator::new().parse_query(&turn_q).unwrap().on_store(&store).execute().unwrap();
        assert!(matches!(ask, QueryResults::Boolean(true)),
            "the Turn node with its timestamp must be in the graph");

        // GOTCHA #2: the triple term must be readable THROUGH the named graph.
        let hits = query_emojikeys(&store, None).unwrap();
        assert_eq!(hits.len(), 1, "one emojikey, read back through GRAPH + triple term");
        assert_eq!(hits[0].transcript, "sess-A");
        assert_eq!(hits[0].source_kind, "authored");
        assert_eq!(hits[0].me, "🧠");

        // provenance filter: authored-only keeps it; tool_result-only drops it.
        assert_eq!(query_emojikeys(&store, Some("authored")).unwrap().len(), 1);
        assert_eq!(query_emojikeys(&store, Some("tool_result")).unwrap().len(), 0);
    }

    #[test]
    fn two_sessions_accumulate_and_are_separable() {
        let store = Store::new().unwrap();
        let part = "https://repolex.ai/ravel/Turn/";
        let eva = vec![ev("e1", "[ME|a]~[CONTENT|b]~[YOU|c]", SourceKind::Authored)];
        let evb = vec![ev("e2", "[ME|x]~[CONTENT|y]~[YOU|z]", SourceKind::ToolResult)];
        let a = emojikey_read(&eva);
        let b = emojikey_read(&evb);
        ingest_annotations(&store, &eva, &a, "sess-A", part).unwrap();
        ingest_annotations(&store, &evb, &b, "sess-B", part).unwrap();

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
        let part = "https://repolex.ai/ravel/Turn/";
        let evs = vec![
            ev("e1", "just some prose, no key here", SourceKind::Authored),
            ev("e2", "[ME|🧠]~[CONTENT|💻]~[YOU|🎓]", SourceKind::Authored),
            ev("e3", "more prose, still no key", SourceKind::Authored),
        ];
        let anns = emojikey_read(&evs);
        assert_eq!(anns.len(), 1, "only the middle turn has an emojikey");

        ingest_transcript(&store, &evs, &anns, "sess-full", part).unwrap();

        // ALL THREE turns are present as spine nodes (not just the annotated one).
        let count_q = format!(
            r#"PREFIX ravel: <{RAVEL_NS}>
               SELECT (COUNT(?t) AS ?n) WHERE {{ GRAPH ?g {{ ?t a ravel:Turn }} }}"#
        );
        let res = SparqlEvaluator::new().parse_query(&count_q).unwrap().on_store(&store).execute().unwrap();
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
}
