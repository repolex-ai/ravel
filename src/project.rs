//! Project generic `Event`s into an oxigraph Store as RDF 1.2.
//!
//! Subject URNs are partition-scoped: `<partition><event_id>` where the adapter
//! supplies `partition` (for the soul adapter, `https://repolex.ai/ravel/Turn/`,
//! carrying no soul identity — soul scoping is the store). The engine
//! treats `partition` as an opaque prefix — it does not know it means "soul".

use crate::{Event, RAVEL_NS};
use anyhow::Result;
use oxigraph::store::Store;

/// Build the subject IRI for an event under a partition.
pub fn event_iri(partition: &str, event_id: &str) -> String {
    format!("{partition}{event_id}")
}

/// Validate an event_id before it's embedded in an IRI. VALIDATE, don't mangle:
/// a `safe()`-style character mangle on an IDENTITY key can collide two
/// distinct ids ("a b" and "a_b" → same IRI) — identity-loss hidden as
/// robustness. The claude-code adapter sources event_id from JSONL uuids (hex +
/// hyphens); the agy adapter synthesizes `<conversation-uuid>:<step_index>`.
///
/// `:` was added to the allowlist on 2026-08-26 when the agy adapter — the
/// first non-UUID dialect, which this gate was written to catch — hit it. It is
/// a deliberate WIDENING, not a mangle: `:` is a legal `pchar` in an IRI path
/// segment (RFC 3987), and since neither a UUID nor a decimal index can contain
/// one, no two distinct agy ids can collide through it and no agy id can
/// collide with a claude uuid.
pub fn validate_event_id(event_id: &str) -> Result<()> {
    if event_id.trim().is_empty() {
        anyhow::bail!("empty event_id — refusing to derive an IRI for a non-event");
    }
    if !event_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == ':')
    {
        anyhow::bail!(
            "malformed event_id {event_id:?} — contains characters unsafe to embed in an IRI; fix the adapter, don't mangle the id"
        );
    }
    Ok(())
}

/// Load events into a fresh in-memory Store, emitting the spine + scalar props.
/// Returns the Store. Idempotent by construction: subject IRIs are deterministic
/// (partition + event_id), so re-projecting the same events overwrites, never
/// duplicates — the w4r3z idempotency discipline, at the triple level.
pub fn project(events: &[Event], partition: &str) -> Result<Store> {
    let store = Store::new()?;
    store.load_from_reader(
        oxigraph::io::RdfFormat::NTriples,
        project_nt(events, partition)?.as_bytes(),
    )?;
    Ok(store)
}

/// Project events to N-Triples (the Turn nodes: type, role, spine, timestamp,
/// text). Exposed alongside `annotate::annotation_nt` so the persistent-graph
/// ingest can load the Turn NODES into the same named graph as the annotations
/// that reference them — without this, an annotation's soul-prefixed
/// `ravel:atTurn` / `prov:used` anchor points at a Turn that was never derived,
/// and the federation TIME anchor (`ravel:timestamp` xsd:dateTime) is absent, so
/// a soul+time cross-store join has soul but not time. The Turn node is what
/// carries the time half of the anchor contract.
pub fn project_nt(events: &[Event], partition: &str) -> Result<String> {
    let mut nt = String::new();

    let turn_class = format!("{RAVEL_NS}Turn");
    let p_turn_id = format!("{RAVEL_NS}turnId");
    let p_parent = format!("{RAVEL_NS}parentTurn");
    let p_ts = format!("{RAVEL_NS}timestamp");
    let p_text = format!("{RAVEL_NS}text");
    let p_role = format!("{RAVEL_NS}role");
    let p_model = format!("{RAVEL_NS}model");
    let p_setting = format!("{RAVEL_NS}modelSetting");
    let xsd_dt = "http://www.w3.org/2001/XMLSchema#dateTime";

    for e in events {
        validate_event_id(&e.event_id)?;
        let s = event_iri(partition, &e.event_id);
        // rdf:type ravel:Turn
        nt.push_str(&format!(
            "<{s}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{turn_class}> .\n"
        ));
        // per-class id (kit-ontology law: every class declares <class>Id)
        nt.push_str(&format!("<{s}> <{p_turn_id}> {} .\n", lit(&e.event_id)));
        // role (plain literal)
        nt.push_str(&format!("<{s}> <{p_role}> {} .\n", lit(&e.role)));
        // spine
        if let Some(pid) = &e.parent_id {
            let p = event_iri(partition, pid);
            nt.push_str(&format!("<{s}> <{p_parent}> <{p}> .\n"));
        }
        // timestamp as xsd:dateTime (federation anchor lexical form)
        if let Some(ts) = &e.timestamp {
            nt.push_str(&format!(
                "<{s}> <{p_ts}> \"{}\"^^<{xsd_dt}> .\n",
                escape(ts)
            ));
        }
        // text (plain literal)
        if let Some(tx) = &e.text {
            nt.push_str(&format!("<{s}> <{p_text}> {} .\n", lit(tx)));
        }
        if let Some(m) = &e.model {
            nt.push_str(&format!("<{s}> <{p_model}> {} .\n", lit(m)));
        }
        if let Some(m) = &e.model_setting {
            nt.push_str(&format!("<{s}> <{p_setting}> {} .\n", lit(m)));
        }
    }

    Ok(nt)
}

/// N-Triples-escape a string and wrap as a plain literal.
fn lit(s: &str) -> String {
    format!("\"{}\"", escape(s))
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ontology_declares_every_turn_term_the_projection_emits() {
        let ttl = include_str!("../ontology/ravel/ravel.ttl");
        let store = Store::new().unwrap();
        store
            .load_from_reader(oxigraph::io::RdfFormat::Turtle, ttl.as_bytes())
            .expect("ravel.ttl parses");
        let e = Event {
            event_id: "e1".into(),
            parent_id: Some("e0".into()),
            role: "assistant".into(),
            timestamp: Some("2026-10-14T12:00:00Z".into()),
            text: Some("hello".into()),
            text_provenance: vec![],
            thinking: None,
            model: Some("claude-opus-5-5".into()),
            model_setting: Some("Gemini 4 Argon (Medium)".into()),
        };
        let nt = project_nt(&[e], "urn:test:").unwrap();
        let mut terms: Vec<String> = nt
            .split_whitespace()
            .filter_map(|w| w.strip_prefix(&format!("<{RAVEL_NS}")))
            .map(|w| w.trim_end_matches('>').to_string())
            .collect();
        terms.sort();
        terms.dedup();
        assert!(
            terms.contains(&"model".to_string()) && terms.contains(&"modelSetting".to_string())
        );
        for t in terms {
            let q = format!("ASK {{ <{RAVEL_NS}{t}> a ?k }}");
            let ok = matches!(
                oxigraph::sparql::SparqlEvaluator::new()
                    .parse_query(&q)
                    .unwrap()
                    .on_store(&store)
                    .execute()
                    .unwrap(),
                oxigraph::sparql::QueryResults::Boolean(true)
            );
            assert!(ok, "ravel:{t} is emitted but not declared in ravel.ttl");
        }
    }
}
