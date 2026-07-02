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
//!   claim rdf:reifies <<( event weave:exhibits "event_type" )>> ;  # UNASSERTED
//!         prov:wasDerivedFrom ann ;                                #   belief form
//!         weave:detector ; weave:detectionType .
//!
//! The reified proposition is the WORLD-claim — "this event exhibits X" — and it
//! is never asserted: a detection is an unasserted CLAIM carrying detector
//! metadata, which is exactly the belief semantics we want (the Day-37 thesis).
//! The subject must be the EVENT, not the annotation: reifying a statement about
//! the annotation would just restate metadata the annotation already asserts,
//! leaving the actual detection asserted-by-implication. (The original spike had
//! this right — `<<( turn weave:detected ... )>>` — the emojikey promotion
//! drifted.) claim → ann via prov:wasDerivedFrom joins belief to evidence.
//! RDF 1.2 triple term = `<<( s p o )>>` (PARENS, object-position) +
//! `rdf:reifies` — squad standard, NOT RDF-star `<< >>`.
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
    // signal is a BTreeMap → stable iteration order → stable hash. Fold in the
    // type tag so Text("8") and Int(8) hash to DISTINCT findings (they project
    // to different literals — a string vs a number).
    for (k, v) in &a.signal {
        h.update([0]);
        h.update(k.as_bytes());
        h.update([1]);
        h.update([v.type_tag()]);
        h.update(v.lexical().as_bytes());
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
    let nt = annotation_nt(anns, transcript_id, partition)?;
    store.load_from_reader(oxigraph::io::RdfFormat::NTriples, nt.as_bytes())?;
    Ok(store)
}

/// Build the N-Triples for a set of annotations (no store). Split out so the
/// persistent-graph arm can retarget these into a per-transcript NAMED graph via
/// `RdfParser::with_default_graph` — same triples, different graph home.
pub fn annotation_nt(anns: &[Annotation], transcript_id: &str, partition: &str) -> Result<String> {
    let mut nt = String::new();

    let a_type = format!("{RDF}type");
    for ann in anns {
        // Fail loud on an empty or malformed event_id: empty would silently
        // mint the bare partition prefix as the event IRI and reify a claim
        // about a non-thing; malformed would embed IRI-breaking bytes. Five
        // triples depend on this id (eventId, atEvent, prov:used, and the
        // reified claim's subject) — the run stops here, not in a downstream
        // query returning wrong-but-plausible joins. (w3bl0rd flinch-audit,
        // Day 41: empty caught by SG, malformed layer caught by the audit.)
        crate::project::validate_event_id(&ann.event_id).map_err(|e| {
            anyhow::anyhow!(
                "annotation from reader '{}' (type '{}', span {}..{}): {e}",
                ann.reader, ann.event_type, ann.start, ann.end
            )
        })?;
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

        // --- body: the signal payload, flattened onto a body node. A leg the
        // reader declared numeric projects as its xsd typed literal, so
        // FILTER-by-magnitude compares as a NUMBER; text legs stay plain. ---
        triple(&mut nt, iri(&ann_iri), iri(&format!("{OA}hasBody")), iri(&body_iri));
        for (k, v) in &ann.signal {
            let obj = match v.xsd_datatype() {
                Some(dt) => typed_lit(&v.lexical(), dt),
                None => str_lit(&v.lexical()),
            };
            triple(&mut nt, iri(&body_iri), iri(&format!("{WEAVE_NS}sig_{}", safe(k))), obj);
        }

        // --- the CLAIM: an RDF 1.2 triple term, UNASSERTED, carrying meta ---
        // <<( event weave:exhibits "event_type" )>> — the world-claim ("this
        // turn exhibits X"), never asserted. Subject is the EVENT: reifying a
        // statement about the annotation would restate already-asserted
        // metadata and the belief semantics would protect nothing.
        let proposition = format!(
            "<<( {} {} {} )>>",
            iri(&event_iri),
            iri(&format!("{WEAVE_NS}exhibits")),
            str_lit(&ann.event_type),
        );
        triple(&mut nt, iri(&claim_iri), iri(&format!("{RDF}reifies")), proposition);
        // belief → evidence: the claim is derived from the annotation wrapper
        triple(&mut nt, iri(&claim_iri), iri(&format!("{PROV}wasDerivedFrom")), iri(&ann_iri));
        triple(&mut nt, iri(&claim_iri), iri(&format!("{WEAVE_NS}detector")), str_lit(&ann.reader));
        triple(&mut nt, iri(&claim_iri), iri(&format!("{WEAVE_NS}detectionType")), str_lit(&ann.event_type));
    }

    Ok(nt)
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
    fn empty_event_id_fails_loud() {
        // the flinch test (w3bl0rd's audit, Day 41): an empty event_id must
        // REFUSE, not silently mint a claim anchored to the bare partition
        // prefix. Healthy annotations continue to project fine.
        let mut anns = emojikey_read(&[ev("e1", "[ME|🐐]~[CONTENT|⚙️]~[YOU|🤝]")]);
        anns[0].event_id = "  ".into();
        let Err(e) = project_annotations(&anns, "t", "urn:soul:x:Weave/Turn/") else {
            panic!("empty event_id must refuse to project");
        };
        assert!(e.to_string().contains("empty event_id"));
        // malformed layer (the audit's catch): non-empty but IRI-unsafe must
        // also refuse — VALIDATED, not mangled (mangling identity keys can
        // collide two distinct ids into one IRI)
        let mut bad = emojikey_read(&[ev("e1", "[ME|🐐]~[CONTENT|⚙️]~[YOU|🤝]")]);
        bad[0].event_id = "e 1<".into();
        let Err(e2) = project_annotations(&bad, "t", "urn:soul:x:Weave/Turn/") else {
            panic!("malformed event_id must refuse to project");
        };
        assert!(e2.to_string().contains("malformed event_id"));
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
            PREFIX prov: <{PROV}>
            SELECT ?event ?det ?me WHERE {{
                ?claim rdf:reifies <<( ?event weave:exhibits "emojikey/harvest" )>> ;
                       prov:wasDerivedFrom ?ann ;
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

    #[test]
    fn numeric_signal_leg_projects_typed_and_filters_as_a_number() {
        use crate::reader::{Annotation, SignalValue};
        use std::collections::BTreeMap;

        // a synthetic wave-math annotation with a NUMERIC magnitude leg
        let mut mk = |mag: i64| {
            let mut sig: BTreeMap<String, SignalValue> = BTreeMap::new();
            sig.insert("magnitude".into(), SignalValue::Int(mag));
            sig.insert("label".into(), SignalValue::Text("flux".into()));
            Annotation {
                reader: "wavemath".into(),
                event_type: "wave/flux".into(),
                ts: Some("2026-06-30T12:00:00Z".into()),
                event_id: format!("ev-{mag}"),
                start: 0,
                end: 1,
                source_kind: None,
                signal: sig,
            }
        };
        let anns = vec![mk(3), mk(8)];
        let store = project_annotations(&anns, "trans-num", "urn:soul:x:Weave/Turn/").unwrap();

        // NUMERIC filter: legs projected as plain strings would compare
        // lexically ("8" < "3" is false but "10" < "3" is TRUE lexically) — a
        // typed xsd:integer makes `> 5` a real number comparison.
        let q = format!(
            r#"PREFIX weave: <{WEAVE_NS}>
               SELECT (COUNT(*) AS ?big) WHERE {{
                 ?body weave:sig_magnitude ?m . FILTER(?m > 5) }}"#
        );
        let res = SparqlEvaluator::new().parse_query(&q).unwrap().on_store(&store).execute().unwrap();
        let QueryResults::Solutions(sols) = res else { panic!("expected solutions") };
        let got: i64 = sols
            .map(|s| s.unwrap().get("big").unwrap().to_string())
            .next()
            .unwrap()
            .trim_matches('"')
            .split('^')
            .next()
            .unwrap()
            .trim_matches('"')
            .parse()
            .unwrap();
        assert_eq!(got, 1, "only magnitude 8 is > 5 (numeric compare, not string)");

        // and the datatype is genuinely xsd:integer on the wire
        let dump = String::from_utf8(
            store
                .dump_to_writer(
                    oxigraph::io::RdfSerializer::from_format(oxigraph::io::RdfFormat::NQuads),
                    Vec::new(),
                )
                .unwrap(),
        )
        .unwrap();
        assert!(
            dump.contains("XMLSchema#integer"),
            "numeric leg must carry an xsd:integer datatype on the wire"
        );
        // text leg stays a plain literal (no datatype coercion)
        assert!(dump.contains("\"flux\""));
    }
}
