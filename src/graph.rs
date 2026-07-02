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
//!    OWN named graph `weave:graph/<transcript_id>` (a per-session provenance
//!    unit — droppable, re-ingestable atomically). A naked default-graph query
//!    will NOT see them; every query here wraps in `GRAPH ?g { … }`. (Pool learned
//!    this the hard way — chevron.rs:707.)
//!
//! 3. **Same registry / partition.** The partition string (`urn:soul:<sha>:…`)
//!    is the adapter's, threaded through unchanged, so a Weave⋈Pool join lines up
//!    on the same soul URN. The engine never invents it.

use crate::annotate::annotation_nt;
use crate::reader::Annotation;
use crate::WEAVE_NS;
use anyhow::{Context, Result};
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{GraphNameRef, NamedNode};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use std::path::Path;

/// The named graph for one transcript's annotations.
pub fn transcript_graph_iri(transcript_id: &str) -> String {
    format!("{WEAVE_NS}graph/{}", safe(transcript_id))
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

/// Ingest one transcript's annotations into its named graph, idempotently.
///
/// Clears the transcript's named graph first (so a dropped key doesn't ghost),
/// then loads the freshly-projected N-Triples INTO that named graph via
/// `with_default_graph`. Returns the triple count now in that graph.
pub fn ingest_annotations(
    store: &Store,
    anns: &[Annotation],
    transcript_id: &str,
    partition: &str,
) -> Result<usize> {
    let graph_iri = transcript_graph_iri(transcript_id);
    let graph = NamedNode::new(&graph_iri).context("transcript graph IRI")?;

    // (1)+(2): wipe this transcript's graph so re-ingest is a clean replace.
    store.clear_graph(GraphNameRef::NamedNode(graph.as_ref()))?;

    // project to N-Triples, retarget into the named graph on load.
    let nt = annotation_nt(anns, transcript_id, partition)?;
    let parser = RdfParser::from_format(RdfFormat::NTriples).with_default_graph(graph.clone());
    store.load_from_reader(parser, nt.as_bytes())?;
    store.flush()?;

    // count what's in this graph now
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
        PREFIX weave: <{WEAVE_NS}>
        PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
        PREFIX oa: <http://www.w3.org/ns/oa#>
        PREFIX prov: <http://www.w3.org/ns/prov#>
        SELECT ?g ?sk ?me ?content ?you ?ts WHERE {{
            GRAPH ?g {{
                ?claim rdf:reifies <<( ?event weave:exhibits "emojikey/harvest" )>> ;
                       prov:wasDerivedFrom ?ann .
                ?ann oa:hasBody ?body .
                OPTIONAL {{ ?ann weave:sourceKind ?sk }}
                OPTIONAL {{ ?ann prov:generatedAtTime ?ts }}
                ?body weave:sig_me ?me ; weave:sig_content ?content ; weave:sig_you ?you .
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
    let prefix = format!("{WEAVE_NS}graph/");
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
        let part = "urn:soul:test-sha:Weave/Turn/";
        let anns = emojikey_read(&[ev("e1", "[ME|🧠]~[CONTENT|💻]~[YOU|🎓]", SourceKind::Authored)]);
        assert_eq!(anns.len(), 1);

        // ingest twice — idempotent: triple count stable, no duplication.
        let n1 = ingest_annotations(&store, &anns, "sess-A", part).unwrap();
        let n2 = ingest_annotations(&store, &anns, "sess-A", part).unwrap();
        assert!(n1 > 0);
        assert_eq!(n1, n2, "re-ingest must not change the graph (idempotent)");

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
        let part = "urn:soul:test-sha:Weave/Turn/";
        let a = emojikey_read(&[ev("e1", "[ME|a]~[CONTENT|b]~[YOU|c]", SourceKind::Authored)]);
        let b = emojikey_read(&[ev("e2", "[ME|x]~[CONTENT|y]~[YOU|z]", SourceKind::ToolResult)]);
        ingest_annotations(&store, &a, "sess-A", part).unwrap();
        ingest_annotations(&store, &b, "sess-B", part).unwrap();

        let all = query_emojikeys(&store, None).unwrap();
        assert_eq!(all.len(), 2, "two sessions accumulate in one store");
        // separable by named graph → by transcript
        let transcripts: Vec<_> = all.iter().map(|h| h.transcript.as_str()).collect();
        assert!(transcripts.contains(&"sess-A") && transcripts.contains(&"sess-B"));
        // and by provenance across sessions
        assert_eq!(query_emojikeys(&store, Some("authored")).unwrap().len(), 1);
    }
}
