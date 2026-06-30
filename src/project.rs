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
