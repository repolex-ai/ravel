//! Project generic `Event`s into an oxigraph Store as RDF 1.2.
//!
//! Subject URNs are partition-scoped: `<partition><event_id>` where the adapter
//! supplies `partition` (for the soul adapter, `urn:soul:<sha>:`). The engine
//! treats `partition` as an opaque prefix — it does not know it means "soul".

use crate::{Event, WEAVE_NS};
use anyhow::Result;
use oxigraph::store::Store;

/// Build the subject IRI for an event under a partition.
pub fn event_iri(partition: &str, event_id: &str) -> String {
    format!("{partition}{event_id}")
}

/// Validate an event_id before it's embedded in an IRI. VALIDATE, don't mangle:
/// a `safe()`-style character mangle on an IDENTITY key can collide two
/// distinct ids ("a b" and "a_b" → same IRI) — identity-loss hidden as
/// robustness. Today's adapter sources event_id from JSONL uuids (hex +
/// hyphens, always valid); the first non-UUID dialect adapter hits this gate
/// instead of silently minting a broken or colliding IRI.
pub fn validate_event_id(event_id: &str) -> Result<()> {
    if event_id.trim().is_empty() {
        anyhow::bail!("empty event_id — refusing to mint an IRI for a non-event");
    }
    if !event_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
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
    let mut nt = String::new();

    let turn_class = format!("{WEAVE_NS}Turn");
    let p_parent = format!("{WEAVE_NS}parentEvent");
    let p_ts = format!("{WEAVE_NS}timestamp");
    let p_text = format!("{WEAVE_NS}text");
    let p_role = format!("{WEAVE_NS}role");
    let xsd_dt = "http://www.w3.org/2001/XMLSchema#dateTime";

    for e in events {
        validate_event_id(&e.event_id)?;
        let s = event_iri(partition, &e.event_id);
        // rdf:type weave:Turn
        nt.push_str(&format!(
            "<{s}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{turn_class}> .\n"
        ));
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
    }

    store.load_from_reader(oxigraph::io::RdfFormat::NTriples, nt.as_bytes())?;
    Ok(store)
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
