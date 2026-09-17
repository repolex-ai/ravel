//! Ravel Day-39 spike — prove the loop on REAL data.
//!
//!   real transcript JSONL
//!     → adapter::parse_transcript  (dialect → generic Events)
//!     → project::project           (Events → oxigraph, RDF 1.2, partition-scoped)
//!     → ONE SPARQL CONSTRUCT detector derives an annotation as a TRIPLE TERM
//!     → round-trip the derived triple term back out and print it
//!
//! Run:  cargo run --bin ravel-spike -- <path-to-transcript.jsonl>
//!
//! The detector here is deliberately trivial (flags long assistant turns) —
//! the POINT is not the detection, it's proving the derive+round-trip of a
//! genuine RDF 1.2 triple term (`rdf:reifies <<( s p o )>>`, the UNASSERTED /
//! belief form for a detection) works in Rust/oxigraph, against real triples.

use anyhow::{Context, Result};
use oxigraph::model::GraphName;
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use ravel::{adapter, project, RAVEL_NS};

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: ravel-spike <transcript.jsonl>")?;
    let jsonl = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;

    // --- adapter: dialect → generic events ---
    let events = adapter::parse_transcript(&jsonl)?;
    println!("[adapter] parsed {} conversational turns", events.len());
    if events.is_empty() {
        println!("(no user/assistant turns found — nothing to project)");
        return Ok(());
    }

    // --- engine: project under a partition. The engine doesn't know this is a
    // "soul"; the adapter chose it (soul scoping is the store, not the
    // subject — see soul.rs). ---
    let partition = ravel::soul::TURN_PARTITION;
    let store = project::project(&events, partition)?;
    println!("[project] store holds {} triples", store.len()?);

    // --- detector: ONE SPARQL CONSTRUCT that DERIVES an annotation as a triple
    // term. "Long assistant turn" = text longer than 2000 chars. We derive an
    // unasserted belief: a reifier node that rdf:reifies the (turn, detected,
    // "verbose") proposition, carrying detector metadata + confidence. ---
    let detected = format!("{RAVEL_NS}detected");
    let verbose = format!("{RAVEL_NS}Verbose");
    let p_text = format!("{RAVEL_NS}text");
    let p_detector = format!("{RAVEL_NS}detector");
    let p_confidence = format!("{RAVEL_NS}confidence");
    let xsd_decimal = "http://www.w3.org/2001/XMLSchema#decimal";

    // RDF 1.2 triple term in CONSTRUCT: the reifier is a blank node that
    // rdf:reifies the (unasserted) proposition `?turn ravel:detected ravel:Verbose`.
    let construct = format!(
        r#"
        PREFIX ravel: <{RAVEL_NS}>
        PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
        CONSTRUCT {{
            _:r rdf:reifies <<( ?turn <{detected}> <{verbose}> )>> ;
                <{p_detector}> "verbose-v0" ;
                <{p_confidence}> "0.9"^^<{xsd_decimal}> .
        }}
        WHERE {{
            ?turn <{p_text}> ?txt .
            FILTER(STRLEN(?txt) > 2000)
        }}
        "#
    );

    let derived = match SparqlEvaluator::new()
        .parse_query(&construct)?
        .on_store(&store)
        .execute()?
    {
        QueryResults::Graph(triples) => {
            let mut v = Vec::new();
            for t in triples {
                v.push(t?);
            }
            v
        }
        _ => anyhow::bail!("CONSTRUCT did not return a graph"),
    };
    println!(
        "[detector] CONSTRUCT derived {} triples ({} detection(s))",
        derived.len(),
        derived.len() / 3 // each detection = reifier + detector + confidence
    );

    // --- round-trip: load the derived triples back into a fresh store and read
    // them out via SELECT, proving the triple term survives store→query→store. ---
    let store2 = oxigraph::store::Store::new()?;
    for t in &derived {
        // a Store is a quad store — drop the triple into the default graph
        store2.insert(t.clone().in_graph(GraphName::DefaultGraph).as_ref())?;
    }
    println!(
        "[round-trip] re-loaded into fresh store: {} triples",
        store2.len()?
    );

    // Read the derived detections back: find reifiers, follow rdf:reifies to the
    // triple term, and pull out the subject + detector + confidence.
    let select = format!(
        r#"
        PREFIX ravel: <{RAVEL_NS}>
        PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
        SELECT ?turn ?detector ?conf WHERE {{
            ?r rdf:reifies <<( ?turn <{detected}> <{verbose}> )>> ;
               <{p_detector}> ?detector ;
               <{p_confidence}> ?conf .
        }}
        "#
    );
    let results = SparqlEvaluator::new()
        .parse_query(&select)?
        .on_store(&store2)
        .execute()?;
    if let QueryResults::Solutions(solutions) = results {
        let mut n = 0;
        for sol in solutions {
            let sol = sol?;
            n += 1;
            let turn = sol.get("turn").map(|t| t.to_string()).unwrap_or_default();
            let det = sol
                .get("detector")
                .map(|t| t.to_string())
                .unwrap_or_default();
            let conf = sol.get("conf").map(|t| t.to_string()).unwrap_or_default();
            println!("  ✓ detection {n}: turn={turn}  detector={det}  conf={conf}");
        }
        if n == 0 {
            println!("  (no detections fired — try a transcript with a >2000-char assistant turn)");
        } else {
            println!("\n[SPIKE PASS] triple term derived by CONSTRUCT survived store→query→store round-trip.");
        }
    }

    Ok(())
}
