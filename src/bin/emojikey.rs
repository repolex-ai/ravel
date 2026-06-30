//! Weave — first REAL reader fires.
//!
//!   real transcript JSONL
//!     → adapter::parse_transcript      (dialect → generic Events)
//!     → reader::emojikey_read          (a PROVEN reader: harvest [ME|CONTENT|YOU])
//!     → annotate::project_annotations  (oa: + prov: + RDF-1.2 triple-term claims)
//!     → round-trip + SPARQL read-back through the triple term
//!
//! Run:  cargo run --bin weave-emojikey -- <path-to-transcript.jsonl>
//!
//! This is the promotion the gate demanded: not the spike's FAKE STRLEN>2000
//! "Verbose" detector, but a real reader proven green in the Python lab, ported
//! and wired through the engine to mint genuine annotations anchored to true
//! source spans. The emojikeys are real keys the conversation already carried.

use anyhow::{Context, Result};
use oxigraph::io::{RdfFormat, RdfSerializer};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use weave::{adapter, annotate, reader, WEAVE_NS};

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: weave-emojikey <transcript.jsonl>")?;
    let jsonl = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;

    // --- adapter: dialect → generic events ---
    let events = adapter::parse_transcript(&jsonl)?;
    println!("[adapter] parsed {} conversational turns", events.len());

    // --- reader: a REAL, proven reader. Harvest inline emojikeys. ---
    let anns = reader::emojikey_read(&events);
    println!("[reader:emojikey] harvested {} emojikey(s)", anns.len());
    if anns.is_empty() {
        println!("(no emojikeys in this transcript — try one where the agent emits [ME|…]~[CONTENT|…]~[YOU|…])");
        return Ok(());
    }

    // --- project: annotations → oxigraph as RDF 1.2 (oa:+prov:+triple term) ---
    // Partition mirrors Pool's urn:soul:<sha>: shape; the engine treats it opaquely.
    let partition = "urn:soul:demo-emojikey-sha:Weave/Turn/";
    let transcript_id = std::path::Path::new(&path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("transcript");
    let store = annotate::project_annotations(&anns, transcript_id, partition)?;
    println!("[project] store holds {} triples", store.len()?);

    // --- round-trip: dump → fresh store, proving the triple terms survive ---
    let dump = store.dump_to_writer(RdfSerializer::from_format(RdfFormat::NQuads), Vec::new())?;
    let store2 = Store::new()?;
    store2.load_from_reader(RdfFormat::NQuads, dump.as_slice())?;
    println!("[round-trip] re-loaded into fresh store: {} triples", store2.len()?);

    // --- read the detections back THROUGH the triple term ---
    let oa = "http://www.w3.org/ns/oa#";
    let select = format!(
        r#"
        PREFIX weave: <{WEAVE_NS}>
        PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
        PREFIX oa: <{oa}>
        SELECT ?det ?me ?content ?you ?start ?end WHERE {{
            ?claim rdf:reifies <<( ?ann weave:exhibits "emojikey/harvest" )>> ;
                   weave:detector ?det .
            ?ann oa:hasBody ?body ;
                 oa:hasTarget ?t .
            ?body weave:sig_me ?me ; weave:sig_content ?content ; weave:sig_you ?you .
            ?t oa:hasSelector ?sel .
            ?sel oa:start ?start ; oa:end ?end .
        }}
        ORDER BY ?start
        "#
    );
    let results = SparqlEvaluator::new()
        .parse_query(&select)?
        .on_store(&store2)
        .execute()?;
    let QueryResults::Solutions(solutions) = results else {
        anyhow::bail!("expected SELECT solutions");
    };
    let mut n = 0;
    for sol in solutions {
        let sol = sol?;
        n += 1;
        let g = |k: &str| sol.get(k).map(|t| t.to_string()).unwrap_or_default();
        println!(
            "  ✓ emojikey {n}: [ME|{}]~[CONTENT|{}]~[YOU|{}]  span={}..{}  via {}",
            unq(&g("me")),
            unq(&g("content")),
            unq(&g("you")),
            unq(&g("start")),
            unq(&g("end")),
            unq(&g("det")),
        );
    }
    if n == 0 {
        println!("  (nothing read back — the triple-term query found no detections)");
    } else {
        println!(
            "\n[READER FIRES] {n} REAL emojikey annotation(s) minted as RDF 1.2 triple terms, \
             anchored to true source spans, survived store→query→store."
        );
    }
    Ok(())
}

/// Strip the surrounding quotes / datatype suffix from an oxigraph term's
/// display form, for friendly printing. (`"🐐"` → `🐐`, `"12"^^<…integer>` → `12`).
fn unq(s: &str) -> String {
    let s = s.strip_prefix('"').unwrap_or(s);
    if let Some(idx) = s.rfind('"') {
        return s[..idx].to_string();
    }
    s.to_string()
}
