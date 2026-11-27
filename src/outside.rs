//! A query that arrives from outside: `ravel sparql`, `ravel query` and raveld's query
//! routes. It is prepared here before the store sees it.
//!
//! Three things happen, in calamus's order (parse, rewrite, evaluate):
//!
//!   - it is parsed against a base, so a short address resolves;
//!   - calamus rewrites it from the ontology, so a question asked in
//!     `subtexture:` names finds what ravel stores under `ravel:` names;
//!   - a query that names no dataset sees every graph in the store. ravel
//!     keeps everything in named graphs, so without this the same question
//!     that works in git-lex returns nothing here, with no error
//!     (named-graph spec section 12: TranscriptGraph and MemoryGraph are
//!     both under subtexture:DefaultGraph).
//!
//! ravel's own working queries never come through here. They name stored
//! spellings and must not change, or slow down, when an ontology gains a
//! name.

use anyhow::{anyhow, Result};
use calamus::{Index, Options, Report};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use spargebra::{Query, SparqlParser};
use std::sync::OnceLock;

// The base an outside query is read against is the root of the store being
// asked, `https://repolex.ai/{squad}/{user}/` (`Names::root`). So
// `<ravel/Turn/x>` and `GRAPH <ravel/NamedGraph/optmem>` mean that store's
// own, and one saved query works on every soul.

/// The prefix of the names no store holds; calamus drops them from answers.
pub const ABSTRACT: &str = "https://repolex.ai/ontology/subtexture/";

const SUBTEXTURE_TTL: &str = include_str!("../ontology/subtexture/subtexture.ttl");
const RAVEL_TTL: &str = include_str!("../ontology/ravel/ravel.ttl");

/// Built once, from the ontology alone, never from what a store holds.
pub fn index() -> &'static Index {
    static INDEX: OnceLock<Index> = OnceLock::new();
    INDEX.get_or_init(|| {
        Index::from_turtle(&[SUBTEXTURE_TTL, RAVEL_TTL], &[ABSTRACT])
            .expect("the shipped ontologies parse; a test holds this")
    })
}

/// Parse and rewrite. Any error means the query is refused, never run
/// half-prepared.
pub fn prepare(text: &str, base: &str) -> Result<(Query, Report)> {
    let mut query = SparqlParser::new()
        .with_base_iri(base)
        .map_err(|e| anyhow!("base {base}: {e}"))?
        .parse_query(text)
        .map_err(|e| anyhow!("{e}"))?;
    // ravel keeps no ontology graph in a store, so nothing is exempt.
    let report = calamus::rewrite(&mut query, index(), &Options::default())
        .map_err(|e| anyhow!("rewrite: {e}"))?;
    Ok((query, report))
}

/// Prepare and run. The results borrow nothing from the query.
pub fn run<'a>(
    store: &'a Store,
    text: &str,
    base: &str,
) -> Result<(QueryResults<'a>, Report)> {
    run_until(store, text, base, None)
}

/// [`run`], with a token the caller can use to stop the work part-way.
pub fn run_until<'a>(
    store: &'a Store,
    text: &str,
    base: &str,
    stop: Option<oxigraph::sparql::CancellationToken>,
) -> Result<(QueryResults<'a>, Report)> {
    let (query, report) = prepare(text, base)?;
    let mut evaluator = SparqlEvaluator::new();
    if let Some(stop) = stop {
        evaluator = evaluator.with_cancellation_token(stop);
    }
    let mut prepared = evaluator.for_query(query);
    // FROM / FROM NAMED written by the caller is kept as written.
    if prepared.dataset().is_default_dataset() {
        prepared.dataset_mut().set_default_graph_as_union();
    }
    Ok((prepared.on_store(store).execute()?, report))
}

/// What a caller should be told about names left as written, one line each.
pub fn notes(report: &Report) -> Vec<String> {
    report
        .left_as_written
        .iter()
        .map(|n| {
            format!(
                "{} has other stored spellings but is compared as written here \
                 (inside an expression); it will most likely match nothing",
                n.as_str()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxigraph::io::RdfFormat;

    /// A writer and a reader over one store directory, as raveld runs them:
    /// the writer opens read-write, adds a turn, flushes and closes, again
    /// and again; the reader opens read-only and runs the query, again and
    /// again. `RAVEL_TRIAL_STORE` must be a scratch COPY: it is written to.
    #[test]
    #[ignore]
    fn real_reader_beside_writer() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let dir = std::env::var("RAVEL_TRIAL_STORE").unwrap();
        assert!(dir.contains("scratchpad"), "a scratch copy only");
        let q = std::fs::read_to_string(std::env::var("RAVEL_TRIAL_QUERY").unwrap()).unwrap();
        let base = std::env::var("RAVEL_TRIAL_BASE").unwrap();
        let secs: u64 = std::env::var("RAVEL_TRIAL_SECS").map(|s| s.parse().unwrap()).unwrap_or(60);
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let (dir, base, done) = (dir.clone(), base.clone(), done.clone());
            std::thread::spawn(move || {
                let mut n = 0u64;
                while !done.load(Ordering::Relaxed) {
                    // `RAVEL_TRIAL_HOLD=1`: take turns at the store, as raveld does.
                    let _alone = std::env::var("RAVEL_TRIAL_HOLD")
                        .is_ok()
                        .then(|| crate::graph::writing(std::path::Path::new(&dir)));
                    let store = crate::graph::open(std::path::Path::new(&dir)).unwrap();
                    let g = oxigraph::model::NamedNode::new(format!("{base}ravel/NamedGraph/trial")).unwrap();
                    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                    let nt = format!(
                        "<{base}ravel/Turn/trial-{n}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <https://repolex.ai/ontology/ravel/Turn> .\n\
                         <{base}ravel/Turn/trial-{n}> <https://repolex.ai/ontology/ravel/timestamp> \"{ts}\"^^<http://www.w3.org/2001/XMLSchema#dateTime> .\n\
                         <{base}ravel/Turn/trial-{n}> <https://repolex.ai/ontology/ravel/role> \"assistant\" .\n\
                         <{base}ravel/Turn/trial-{n}> <https://repolex.ai/ontology/ravel/text> \"trial line {n}\" .\n\
                         <{base}ravel/Turn/trial-{n}> <https://repolex.ai/ontology/ravel/parentTurn> <{base}ravel/Turn/trial-{}> .\n",
                        n.saturating_sub(1)
                    );
                    let parser = oxigraph::io::RdfParser::from_format(RdfFormat::NTriples).with_default_graph(g);
                    store.load_from_reader(parser, nt.as_bytes()).unwrap();
                    store.flush().unwrap();
                    drop(store);
                    n += 1;
                }
                n
            })
        };
        let t0 = std::time::Instant::now();
        let (mut runs, mut worst, mut failed) = (0u32, 0f64, 0u32);
        while t0.elapsed().as_secs() < secs {
            let t = std::time::Instant::now();
            let _hold = std::env::var("RAVEL_TRIAL_HOLD")
                .is_ok()
                .then(|| crate::graph::reading(std::path::Path::new(&dir)));
            match crate::graph::open_read_only(std::path::Path::new(&dir)) {
                Ok(store) => match run(&store, &q, &base) {
                    Ok((QueryResults::Solutions(sols), _)) => {
                        let rows = sols.filter(|s| s.is_ok()).count();
                        let took = t.elapsed().as_secs_f64();
                        if took > 3.0 {
                            eprintln!("SLOW: {took:.1}s, {rows} rows, at {:.0}s", t0.elapsed().as_secs_f64());
                        }
                        worst = worst.max(took);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        failed += 1;
                        eprintln!("QUERY FAILED: {e:#}");
                    }
                },
                Err(e) => {
                    failed += 1;
                    eprintln!("OPEN FAILED: {e:#}");
                }
            }
            runs += 1;
        }
        done.store(true, Ordering::Relaxed);
        let written = writer.join().unwrap();
        eprintln!("DONE: {runs} queries, worst {worst:.2}s, {failed} failed, {written} turns written");
    }

    /// One query against a real store, read-only, timed. `RAVEL_TRIAL_STORE`
    /// is an oxigraph directory, `RAVEL_TRIAL_QUERY` a file, `RAVEL_TRIAL_BASE`
    /// the store's root address.
    #[test]
    #[ignore]
    fn real_query() {
        let dir = std::env::var("RAVEL_TRIAL_STORE").unwrap();
        let q = std::fs::read_to_string(std::env::var("RAVEL_TRIAL_QUERY").unwrap()).unwrap();
        let base = std::env::var("RAVEL_TRIAL_BASE").unwrap();
        let t = std::time::Instant::now();
        let store = crate::graph::open_read_only(std::path::Path::new(&dir)).unwrap();
        eprintln!("OPENED in {:.2}s", t.elapsed().as_secs_f64());
        // `RAVEL_TRIAL_CANCEL_MS`: cancel the query that long after it starts,
        // to show what a cancel does to spareval (see daemon/http.rs `answer`).
        let stop = oxigraph::sparql::CancellationToken::new();
        if let Ok(ms) = std::env::var("RAVEL_TRIAL_CANCEL_MS") {
            let (stop, ms) = (stop.clone(), ms.parse::<u64>().unwrap());
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                stop.cancel();
                eprintln!("CANCELLED at {ms} ms");
            });
        }
        let (r, _) = run_until(&store, &q, &base, Some(stop)).unwrap();
        let (mut n, mut errs) = (0usize, 0usize);
        if let QueryResults::Solutions(sols) = r {
            for s in sols {
                match s {
                    Ok(_) => n += 1,
                    Err(_) => errs += 1,
                }
            }
        }
        eprintln!("ROWS {n}, ERRORS {errs} in {:.2}s", t.elapsed().as_secs_f64());
    }

    /// On a copy of a real soul (`RAVEL_TRIAL_REPO`, in a scratchpad): list
    /// its newest turns for the first time, then time "the newest 40 turns"
    /// asked of that list and asked of the whole store.
    #[test]
    #[ignore]
    fn real_recent() {
        let repo = std::path::PathBuf::from(std::env::var("RAVEL_TRIAL_REPO").unwrap());
        assert!(repo.to_string_lossy().contains("scratchpad"), "a copy, never a soul");
        let names = crate::names::Names::for_soul(&repo).unwrap();
        let store = crate::graph::open(repo.join(crate::sync::STORE_SUBDIR)).unwrap();
        for i in 1..=2 {
            let t = std::time::Instant::now();
            crate::graph::keep_recent(&store, &names, &[]).unwrap();
            eprintln!("LISTED (call {i}) in {:.3}s", t.elapsed().as_secs_f64());
        }
        store.flush().unwrap();
        let ask = |what: &str, q: &str| {
            let t = std::time::Instant::now();
            let (r, _) = run(&store, q, names.root()).unwrap();
            let mut first = String::new();
            let mut n = 0;
            if let QueryResults::Solutions(sols) = r {
                for s in sols {
                    let s = s.unwrap();
                    if n == 0 {
                        first = s.get("ts").map(|t| t.to_string()).unwrap_or_default();
                    }
                    n += 1;
                }
            }
            eprintln!("{what}: {n} rows, newest {first}, in {:.3}s", t.elapsed().as_secs_f64());
        };
        let p = "PREFIX ravel: <https://repolex.ai/ontology/ravel/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>";
        let rest = "?t ravel:timestamp ?ts ; ravel:role ?role . OPTIONAL { ?t ravel:text ?text } } ORDER BY DESC(?ts) LIMIT 40";
        ask("OF THE LIST", &format!("{p} SELECT ?t ?ts ?role ?text WHERE {{ GRAPH <ravel/NamedGraph/recent> {{ ?l rdfs:member ?t }} {rest}"));
        ask("OF THE WHOLE STORE", &format!("{p} SELECT ?t ?ts ?role ?text WHERE {{ ?t a ravel:Turn . {rest}"));
        ask("OF THE LIST again", &format!("{p} SELECT ?t ?ts ?role ?text WHERE {{ GRAPH <ravel/NamedGraph/recent> {{ ?l rdfs:member ?t }} {rest}"));
    }

    const DATA: &str = r#"
<https://repolex.ai/ravel/Turn/t1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <https://repolex.ai/ontology/ravel/Turn> <https://repolex.ai/ravel/NamedGraph/a> .
<https://repolex.ai/ravel/Turn/t1> <https://repolex.ai/ontology/ravel/role> "user" <https://repolex.ai/ravel/NamedGraph/a> .
<https://repolex.ai/ravel/Turn/t2> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <https://repolex.ai/ontology/ravel/Turn> <https://repolex.ai/ravel/NamedGraph/b> .
<https://repolex.ai/ravel/Memory/0-1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <https://repolex.ai/ontology/ravel/Memory> <https://repolex.ai/ravel/NamedGraph/memory-v1> .
<https://repolex.ai/ravel/Memory/0-1> <https://repolex.ai/ontology/ravel/relatedToId> <https://repolex.ai/ravel/Turn/t1> <https://repolex.ai/ravel/NamedGraph/memory-v1> .
"#;

    fn store() -> Store {
        let s = Store::new().unwrap();
        s.load_from_reader(RdfFormat::NQuads, DATA.as_bytes()).unwrap();
        s
    }

    fn count(store: &Store, q: &str) -> usize {
        match run(store, q, crate::names::BARE_ROOT).unwrap().0 {
            QueryResults::Solutions(s) => s.map(|r| r.unwrap()).count(),
            _ => panic!("expected rows"),
        }
    }

    const P: &str = "PREFIX subtexture: <https://repolex.ai/ontology/subtexture/> \
                     PREFIX ravel: <https://repolex.ai/ontology/ravel/> ";

    #[test]
    fn a_query_with_no_graph_sees_every_graph() {
        let s = store();
        assert_eq!(count(&s, &format!("{P} SELECT ?t WHERE {{ ?t a ravel:Turn }}")), 2);
    }

    #[test]
    fn a_base_class_finds_turns_and_memory_once_each() {
        let s = store();
        let q = format!("{P} SELECT ?x WHERE {{ ?x a subtexture:Thing }}");
        assert_eq!(count(&s, &q), 3);
    }

    #[test]
    fn a_base_property_finds_the_stored_spelling() {
        let s = store();
        let q = format!("{P} SELECT ?m ?t WHERE {{ ?m subtexture:relatedToId ?t }}");
        assert_eq!(count(&s, &q), 1);
    }

    #[test]
    fn a_short_address_resolves_against_the_base() {
        let s = store();
        let q = format!("{P} SELECT ?r WHERE {{ <ravel/Turn/t1> ravel:role ?r }}");
        assert_eq!(count(&s, &q), 1);
        let q = format!("{P} SELECT ?t WHERE {{ GRAPH <ravel/NamedGraph/b> {{ ?t a ravel:Turn }} }}");
        assert_eq!(count(&s, &q), 1);
    }

    #[test]
    fn a_named_graph_still_narrows_the_question() {
        let s = store();
        let q = format!("{P} SELECT ?t WHERE {{ GRAPH ?g {{ ?t a subtexture:Thing }} }}");
        assert_eq!(count(&s, &q), 3);
    }

    /// calamus 0.1.2: a fanned-out name inside GRAPH ?g used to come back
    /// with ?g empty and no error.
    #[test]
    fn a_fanned_out_name_keeps_its_graph() {
        let s = store();
        let q = format!("{P} SELECT ?g ?t WHERE {{ GRAPH ?g {{ ?t a subtexture:Thing }} }}");
        let QueryResults::Solutions(sols) = run(&s, &q, crate::names::BARE_ROOT).unwrap().0 else {
            panic!("expected rows")
        };
        let rows: Vec<_> = sols.map(|r| r.unwrap()).collect();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.get("g").is_some()), "a row lost its graph");
    }

    #[test]
    fn a_name_inside_a_filter_is_reported() {
        let q = format!("{P} SELECT ?s WHERE {{ ?s ?p ?o FILTER(?p = subtexture:relatedToId) }}");
        let (_, report) = prepare(&q, crate::names::BARE_ROOT).unwrap();
        assert_eq!(notes(&report).len(), 1);
    }

    #[test]
    fn a_query_that_does_not_parse_is_refused() {
        assert!(prepare("SELECT WHERE", crate::names::BARE_ROOT).is_err());
    }

    /// Not a test of correctness: the price of the union view and the
    /// rewrite on a real store. `RAVEL_REAL_STORE=<soul>/.ravel/_ignore/store
    /// cargo test --release real_store -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_store_timings() {
        let dir = std::env::var("RAVEL_REAL_STORE").expect("RAVEL_REAL_STORE");
        let s = crate::graph::open_read_only(std::path::Path::new(&dir)).unwrap();
        for (label, q) in [
            ("turns, GRAPH ?g", format!("{P} SELECT (COUNT(?t) AS ?n) WHERE {{ GRAPH ?g {{ ?t a ravel:Turn }} }}")),
            ("turns, no GRAPH", format!("{P} SELECT (COUNT(?t) AS ?n) WHERE {{ ?t a ravel:Turn }}")),
            ("Thing, no GRAPH", format!("{P} SELECT (COUNT(?t) AS ?n) WHERE {{ ?t a subtexture:Thing }}")),
            ("latest 20 turns, no GRAPH", format!("{P} SELECT ?t ?ts WHERE {{ ?t a ravel:Turn ; ravel:timestamp ?ts }} ORDER BY DESC(?ts) LIMIT 20")),
            ("latest 20 turns, GRAPH ?g", format!("{P} SELECT ?t ?ts WHERE {{ GRAPH ?g {{ ?t a ravel:Turn ; ravel:timestamp ?ts }} }} ORDER BY DESC(?ts) LIMIT 20")),
            ("relatedToId base name", format!("{P} SELECT (COUNT(*) AS ?n) WHERE {{ ?m subtexture:relatedToId ?t }}")),
        ] {
            let t = std::time::Instant::now();
            let (r, _) = run(&s, &q, crate::names::BARE_ROOT).unwrap();
            let out = match r {
                QueryResults::Solutions(sols) => {
                    let rows: Vec<_> = sols.map(|r| r.unwrap()).collect();
                    if rows.len() == 1 { rows[0].iter().map(|(_, v)| v.to_string()).collect::<Vec<_>>().join(" ") } else { format!("{} rows", rows.len()) }
                }
                _ => String::new(),
            };
            println!("{:>8.3}s  {label}: {out}", t.elapsed().as_secs_f64());
        }
    }

    /// calamus drops abstract names from every answer, so a stored one would
    /// be hidden. Nothing ravel projects may use one.
    #[test]
    fn the_projection_writes_no_subtexture_name() {
        for src in [
            include_str!("project.rs"),
            include_str!("memory_log.rs"),
            include_str!("graph.rs"),
        ] {
            let code: String = src
                .lines()
                .take_while(|l| !l.contains("#[cfg(test)]"))
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !code.contains("ontology/subtexture/"),
                "a writer names a subtexture: address"
            );
        }
    }
}
