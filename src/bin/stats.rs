//! `ravel-stats` — a read-only inspector for a persistent ravel store. A dev
//! lens, not part of the shipped read surface (that's the HTTP server, later).
//!
//!   ravel-stats <store-dir>            # summary: quads, graphs, predicates, anchors
//!   ravel-stats <store-dir> "SELECT …" # run an ad-hoc SPARQL query, dump rows
//!
//! Summary mode reports total quads, named-graph count, top predicates, and
//! whether the federation anchors are present (soul-prefixed Turn subjects +
//! xsd:dateTime timestamps). Ad-hoc mode is how you poke the graph by hand.

use anyhow::Result;
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

fn scalar(store: &Store, q: &str) -> Result<String> {
    if let QueryResults::Solutions(mut sols) = SparqlEvaluator::new()
        .parse_query(q)?
        .on_store(store)
        .execute()?
    {
        if let Some(s) = sols.next() {
            let s = s?;
            let val = s.iter().next().map(|(_, t)| t.to_string());
            if let Some(t) = val {
                return Ok(t);
            }
        }
    }
    Ok("(none)".into())
}

fn main() -> Result<()> {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: ravel-stats <store-dir> [SPARQL]");
    // A stats call must NEVER create a store: a mis-aimed path should refuse,
    // not litter an empty RocksDB dir and report a trustworthy-looking 0.
    // (th34's first field report, 2026-08-04.) Read-only open requires the
    // path to exist and never writes.
    if !std::path::Path::new(&dir).join("CURRENT").exists() {
        anyhow::bail!(
            "{dir} is not an oxigraph store (no RocksDB CURRENT file) — refusing to create one; check the path (expected e.g. <soul-repo>/.ravel/_ignore/oxigraph)"
        );
    }
    let store = Store::open_read_only(&dir)?;

    // Ad-hoc mode: `ravel-stats <dir> "SELECT ..."` runs the query and dumps rows.
    if let Some(q) = std::env::args().nth(2) {
        println!("=== ad-hoc query on {dir} ===\n{q}\n");
        match SparqlEvaluator::new()
            .parse_query(&q)?
            .on_store(&store)
            .execute()?
        {
            QueryResults::Solutions(sols) => {
                let mut n = 0;
                for s in sols {
                    let s = s?;
                    let row: Vec<String> = s
                        .iter()
                        .map(|(v, t)| format!("{}={}", v.as_str(), t))
                        .collect();
                    println!("  {}", row.join("   "));
                    n += 1;
                }
                println!("\n[{n} row(s)]");
            }
            QueryResults::Boolean(b) => println!("  → {b}"),
            QueryResults::Graph(_) => println!("  (graph result)"),
        }
        return Ok(());
    }

    println!("=== STORE: {dir} ===\n");

    println!(
        "total quads:      {}",
        scalar(
            &store,
            "SELECT (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }"
        )?
    );
    println!(
        "named graphs:     {}",
        scalar(
            &store,
            "SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }"
        )?
    );
    println!(
        "distinct subjects:{}",
        scalar(
            &store,
            "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }"
        )?
    );

    println!("\n--- top predicates ---");
    if let QueryResults::Solutions(sols) = SparqlEvaluator::new()
        .parse_query(
            "SELECT ?p (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } } GROUP BY ?p ORDER BY DESC(?n) LIMIT 20",
        )?
        .on_store(&store)
        .execute()?
    {
        for s in sols {
            let s = s?;
            let p = s.get("p").map(|t| t.to_string()).unwrap_or_default();
            let n = s.get("n").map(|t| t.to_string()).unwrap_or_default();
            println!("  {n:>6}  {p}");
        }
    }

    println!("\n--- FEDERATION ANCHORS ---");
    println!("Turn subjects (…/ravel/Turn/*):       {}",
        scalar(&store, "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } FILTER(STRSTARTS(STR(?s), \"https://repolex.ai/ravel/Turn/\")) }")?);
    println!("Turn nodes carrying xsd:dateTime:     {}",
        scalar(&store, "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE { GRAPH ?g { ?s ?ts ?t } FILTER(DATATYPE(?t) = <http://www.w3.org/2001/XMLSchema#dateTime>) }")?);
    println!("earliest timestamp: {}",
        scalar(&store, "SELECT (MIN(?t) AS ?m) WHERE { GRAPH ?g { ?s ?p ?t } FILTER(DATATYPE(?t) = <http://www.w3.org/2001/XMLSchema#dateTime>) }")?);
    println!("latest timestamp:   {}",
        scalar(&store, "SELECT (MAX(?t) AS ?m) WHERE { GRAPH ?g { ?s ?p ?t } FILTER(DATATYPE(?t) = <http://www.w3.org/2001/XMLSchema#dateTime>) }")?);

    println!("\n--- RDF 1.2 belief layer ---");
    println!("reified (unasserted) claims: {}",
        scalar(&store, "SELECT (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> ?tt } }")?);

    Ok(())
}
