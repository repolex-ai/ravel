//! Project `Annotation`s into oxigraph as RDF 1.2 (oa: + prov: + triple terms).
//!
//! This is the Rust port of the proven Python `graph/project.py` annotation
//! layer. The shape (verified green in the lab):
//!
//!   ann   a oa:Annotation ;
//!         prov:wasAttributedTo <detector> ; prov:generatedAtTime ts ;
//!         oa:hasTarget [ oa:hasSource <transcript> ;
//!                        oa:hasSelector [ a oa:TextPositionSelector ;
//!                                         oa:start ; oa:end ] ;
//!                        weave:eventId … ] ;
//!         oa:hasBody [ weave:sig_* … ] ;
//!         prov:used <evidence> .
//!   claim rdf:reifies <<( ann weave:exhibits "event_type" )>> ;   # UNASSERTED
//!         weave:detector ; weave:detectionType .                  #   belief form
//!
//! The base proposition is NOT separately asserted: a detection is an unasserted
//! CLAIM carrying detector metadata, which is exactly the belief semantics we
//! want (the Day-37 thesis). RDF 1.2 triple term = `<<( s p o )>>` (PARENS,
//! object-position) + `rdf:reifies` — squad standard, NOT RDF-star `<< >>`.
//!
//! Identity is deterministic (sha256 over the annotation's defining fields), so
//! re-projecting the same findings overwrites rather than duplicating — the
//! w4r3z idempotency discipline, carried into the annotation IRIs.

use crate::reader::Annotation;
use crate::WEAVE_NS;
use anyhow::Result;
use oxigraph::store::Store;
use sha2::{Digest, Sha256};

const OA: &str = "http://www.w3.org/ns/oa#";
const PROV: &str = "http://www.w3.org/ns/prov#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// Stable short id over an annotation's defining fields. Re-importing the same
/// finding yields the same IRIs → idempotent bulk reload, no blank-node bloat.
fn ann_hash(transcript_id: &str, a: &Annotation) -> String {
    let mut h = Sha256::new();
    h.update(transcript_id.as_bytes());
    h.update([0]);
    h.update(a.reader.as_bytes());
    h.update([0]);
    h.update(a.event_type.as_bytes());
    h.update([0]);
    h.update(a.event_id.as_bytes());
    h.update([0]);
    h.update(a.start.to_le_bytes());
    h.update(a.end.to_le_bytes());
    h.update([0]);
    h.update(a.source_kind.map(|k| k.tag()).unwrap_or("").as_bytes());
    // signal is a BTreeMap → stable iteration order → stable hash
    for (k, v) in &a.signal {
        h.update([0]);
        h.update(k.as_bytes());
        h.update([1]);
        h.update(v.as_bytes());
    }
    let digest = h.finalize();
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Project annotations for one transcript into a fresh in-memory store.
///
/// `partition` is the engine-opaque prefix the adapter chose (for the soul
/// adapter, `urn:soul:<sha>:Weave/Turn/`). Event-anchor IRIs are
/// `<partition><event_id>` so the annotation's target resolves back to the same
/// turn node the projector emits.
pub fn project_annotations(
    anns: &[Annotation],
    transcript_id: &str,
    partition: &str,
) -> Result<Store> {
    let store = Store::new()?;
    let nt = annotation_nt(anns, transcript_id, partition);
    store.load_from_reader(oxigraph::io::RdfFormat::NTriples, nt.as_bytes())?;
    Ok(store)
}

/// Build the N-Triples for a set of annotations (no store). Split out so the
/// persistent-graph arm can retarget these into a per-transcript NAMED graph via
/// `RdfParser::with_default_graph` — same triples, different graph home.
pub fn annotation_nt(anns: &[Annotation], transcript_id: &str, partition: &str) -> String {
    let mut nt = String::new();

    let a_type = format!("{RDF}type");
    for ann in anns {
        let h = ann_hash(transcript_id, ann);
        let ann_iri = format!("{WEAVE_NS}ann/{h}");
        let claim_iri = format!("{WEAVE_NS}claim/{h}");
        let detector_iri = format!("{WEAVE_NS}detector/{}", safe(&ann.reader));
        let transcript_iri = format!("{WEAVE_NS}transcript/{}", safe(transcript_id));
        let target_iri = format!("{ann_iri}/target");
        let selector_iri = format!("{ann_iri}/selector");
        let body_iri = format!("{ann_iri}/body");
        // the event this anchors to = the turn node the projector emitted
        let event_iri = crate::project::event_iri(partition, &ann.event_id);

        // --- detector as a prov:SoftwareAgent ---
        triple(&mut nt, iri(&detector_iri), iri(&a_type), iri(&format!("{PROV}SoftwareAgent")));
        triple(&mut nt, iri(&detector_iri), iri(&format!("{WEAVE_NS}readerName")), str_lit(&ann.reader));

        // --- the annotation (oa:) ---
        triple(&mut nt, iri(&ann_iri), iri(&a_type), iri(&format!("{OA}Annotation")));
        triple(&mut nt, iri(&ann_iri), iri(&format!("{PROV}wasAttributedTo")), iri(&detector_iri));
        triple(&mut nt, iri(&ann_iri), iri(&format!("{WEAVE_NS}eventType")), str_lit(&ann.event_type));
        if let Some(ts) = &ann.ts {
            triple(&mut nt, iri(&ann_iri), iri(&format!("{PROV}generatedAtTime")), typed_lit(ts, &format!("{XSD}dateTime")));
        }
        // provenance: was the signal AUTHORED (live emission) or found inside a
        // TOOL_RESULT (quoted/pasted)? Carried on the annotation so a query can
        // filter to live keys — the lossless-emit decision (don't drop at read).
        if let Some(sk) = ann.source_kind {
            triple(&mut nt, iri(&ann_iri), iri(&format!("{WEAVE_NS}sourceKind")), str_lit(sk.tag()));
        }

        // --- target: a span of the source. Matches the proven Python shape:
        // oa:hasSource is the whole transcript; the selector carries the
        // character span; and the durable per-turn join key (the event node) is
        // carried explicitly so the annotation resolves to BOTH the document and
        // the exact turn (address-by-identity, not just offsets). ---
        triple(&mut nt, iri(&ann_iri), iri(&format!("{OA}hasTarget")), iri(&target_iri));
        triple(&mut nt, iri(&target_iri), iri(&format!("{OA}hasSource")), iri(&transcript_iri));
        triple(&mut nt, iri(&target_iri), iri(&format!("{OA}hasSelector")), iri(&selector_iri));
        triple(&mut nt, iri(&selector_iri), iri(&a_type), iri(&format!("{OA}TextPositionSelector")));
        triple(&mut nt, iri(&selector_iri), iri(&format!("{OA}start")), typed_lit(&ann.start.to_string(), &format!("{XSD}integer")));
        triple(&mut nt, iri(&selector_iri), iri(&format!("{OA}end")), typed_lit(&ann.end.to_string(), &format!("{XSD}integer")));
        // durable per-turn join key — the event node the projector emits, so the
        // annotation joins straight back to its exact turn (not just the doc).
        triple(&mut nt, iri(&target_iri), iri(&format!("{WEAVE_NS}eventId")), str_lit(&ann.event_id));
        triple(&mut nt, iri(&target_iri), iri(&format!("{WEAVE_NS}atEvent")), iri(&event_iri));

        // --- evidence (prov:used): what the detector looked at = the event ---
        triple(&mut nt, iri(&ann_iri), iri(&format!("{PROV}used")), iri(&event_iri));

        // --- body: the signal payload, flattened onto a body node ---
        triple(&mut nt, iri(&ann_iri), iri(&format!("{OA}hasBody")), iri(&body_iri));
        for (k, v) in &ann.signal {
            triple(&mut nt, iri(&body_iri), iri(&format!("{WEAVE_NS}sig_{}", safe(k))), str_lit(v));
        }

        // --- the CLAIM: an RDF 1.2 triple term, UNASSERTED, carrying meta ---
        // <<( ann weave:exhibits "event_type" )>> as the reified proposition.
        let proposition = format!(
            "<<( {} {} {} )>>",
            iri(&ann_iri),
            iri(&format!("{WEAVE_NS}exhibits")),
            str_lit(&ann.event_type),
        );
        triple(&mut nt, iri(&claim_iri), iri(&format!("{RDF}reifies")), proposition);
        triple(&mut nt, iri(&claim_iri), iri(&format!("{WEAVE_NS}detector")), str_lit(&ann.reader));
        triple(&mut nt, iri(&claim_iri), iri(&format!("{WEAVE_NS}detectionType")), str_lit(&ann.event_type));
    }

    nt
}

// --- N-Triples term builders -------------------------------------------------

fn iri(s: &str) -> String {
    format!("<{s}>")
}

fn str_lit(s: &str) -> String {
    format!("\"{}\"", escape(s))
}

fn typed_lit(s: &str, datatype: &str) -> String {
    format!("\"{}\"^^<{datatype}>", escape(s))
}

fn triple(buf: &mut String, s: String, p: String, o: String) {
    buf.push_str(&s);
    buf.push(' ');
    buf.push_str(&p);
    buf.push(' ');
    buf.push_str(&o);
    buf.push_str(" .\n");
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
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
    use crate::Event;
    use oxigraph::model::GraphName;
    use oxigraph::sparql::{QueryResults, SparqlEvaluator};

    fn ev(id: &str, text: &str) -> Event {
        let span = crate::TextSpan { start: 0, end: text.len(), kind: crate::SourceKind::Authored };
        Event {
            event_id: id.into(),
            parent_id: None,
            role: "assistant".into(),
            timestamp: Some("2026-06-30T12:00:00Z".into()),
            text: Some(text.into()),
            text_provenance: vec![span],
        }
    }

    #[test]
    fn idempotent_hash() {
        let a = &emojikey_read(&[ev("e1", "[ME|🐐]~[CONTENT|⚙️]~[YOU|🤝]")])[0];
        assert_eq!(ann_hash("t", a), ann_hash("t", a));
    }

    #[test]
    fn projects_and_reads_back_through_triple_term() {
        let part = "urn:soul:test-sha:Weave/Turn/";
        let anns = emojikey_read(&[ev("e1", "key [ME|🧠]~[CONTENT|💻]~[YOU|🎓] here")]);
        assert_eq!(anns.len(), 1);
        let store = project_annotations(&anns, "trans-1", part).unwrap();
        assert!(store.len().unwrap() > 0);

        // round-trip through a fresh store, reading back THROUGH the triple term
        let store2 = Store::new().unwrap();
        let dump = store
            .dump_to_writer(oxigraph::io::RdfSerializer::from_format(oxigraph::io::RdfFormat::NQuads), Vec::new())
            .unwrap();
        store2
            .load_from_reader(oxigraph::io::RdfFormat::NQuads, dump.as_slice())
            .unwrap();

        let q = format!(
            r#"
            PREFIX weave: <{WEAVE_NS}>
            PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
            PREFIX oa: <{OA}>
            SELECT ?ann ?det ?me WHERE {{
                ?claim rdf:reifies <<( ?ann weave:exhibits "emojikey/harvest" )>> ;
                       weave:detector ?det .
                ?ann oa:hasBody ?body .
                ?body weave:sig_me ?me .
            }}
            "#
        );
        let res = SparqlEvaluator::new().parse_query(&q).unwrap().on_store(&store2).execute().unwrap();
        let QueryResults::Solutions(sols) = res else { panic!("expected solutions") };
        let mut n = 0;
        for s in sols {
            let s = s.unwrap();
            assert_eq!(s.get("det").unwrap().to_string(), "\"emojikey\"");
            assert!(s.get("me").is_some());
            n += 1;
        }
        assert_eq!(n, 1, "exactly one emojikey detection should round-trip");
        let _ = GraphName::DefaultGraph; // keep import honest if unused above
    }
}
