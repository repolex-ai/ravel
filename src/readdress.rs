//! Move a store that was written under the old addresses to the soul's own.
//!
//! Until 0.10.0 every soul's store used the same addresses,
//! `https://repolex.ai/ravel/…`. They now start with the soul's root,
//! `https://repolex.ai/{squad}/{user}/ravel/…` ([`crate::names`]).
//!
//! The move builds a second store beside the first, copies every statement
//! across with its addresses changed, describes each graph, checks the two
//! agree, and only then puts the new one in place. The old store is kept,
//! renamed, and never removed here: removing a database is the machine
//! owner's call.
//!
//! Only addresses change. Text is never touched, so a turn that quotes an
//! old address still quotes it. The memory graph is not copied: it is
//! rebuilt from the soul's memory logs under its new name.

use crate::graph::{self, GraphKind};
use crate::names::{Names, OLD_MEMORY_GRAPH};
use anyhow::{bail, Context, Result};
use oxigraph::model::{GraphName, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use oxigraph::store::Store;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Beside the store while it is being built; a leftover one is from a run
/// that was stopped, and is rebuilt from nothing.
const BUILDING: &str = "oxigraph.readdressing";
/// What the old store is renamed to.
pub const KEPT: &str = "oxigraph.before-0.10-addresses";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    /// Statements copied, addresses changed.
    pub copied: usize,
    /// Statements of the old memory graph, left out and rebuilt from the logs.
    pub memory_left_out: usize,
    /// Transcript graphs, each now described.
    pub graphs: usize,
    /// Distinct turns, the same before and after or the move is refused.
    pub turns: usize,
    /// Where the old store is now.
    pub kept: PathBuf,
}

fn sibling(store_dir: &Path, name: &str) -> PathBuf {
    store_dir.with_file_name(name)
}

/// True when the store still holds graphs under the old addresses and this
/// soul's addresses are different ones.
pub fn is_old(store: &Store, names: &Names) -> Result<bool> {
    let old = Names::bare();
    if names == &old {
        return Ok(false);
    }
    let old_prefix = old.graph_prefix();
    for g in store.named_graphs() {
        if let NamedOrBlankNode::NamedNode(n) = g? {
            if n.as_str().starts_with(&old_prefix) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Open a soul's store read-write, moving it to the soul's addresses first
/// if it is still under the old ones. Every writer opens through here.
pub fn open_current(repo: &Path, names: &Names) -> Result<Store> {
    let store_dir = repo.join(crate::sync::STORE_SUBDIR);
    let store = graph::open(&store_dir)?;
    if !is_old(&store, names)? {
        return Ok(store);
    }
    drop(store);
    eprintln!(
        "[raveld] {}: moving the store to {}",
        repo.display(),
        names.base()
    );
    let moved = move_store(&store_dir, names)?;
    eprintln!(
        "[raveld] {}: moved {} statements in {} graphs, {} turns; the old store is kept at {}",
        repo.display(),
        moved.copied,
        moved.graphs,
        moved.turns,
        moved.kept.display()
    );
    let store = graph::open(&store_dir)?;
    crate::memory_log::project_into(&store, repo, names)?;
    Ok(store)
}

fn turns(store: &Store) -> Result<usize> {
    let rows = graph::query_rows(
        store,
        "SELECT (COUNT(DISTINCT ?t) AS ?n) WHERE { GRAPH ?g { ?t a <https://repolex.ai/ontology/ravel/Turn> } }",
    )?;
    rows.first()
        .and_then(|r| r.get("n"))
        .and_then(|n| n.parse().ok())
        .context("counting turns")
}

/// The move itself. `store_dir` must not be open anywhere in this process.
pub fn move_store(store_dir: &Path, names: &Names) -> Result<Moved> {
    let old_base = Names::bare().base();
    let new_base = names.base();
    if old_base == new_base {
        bail!("{} has no squad and username; its addresses do not move", store_dir.display());
    }
    let building = sibling(store_dir, BUILDING);
    let kept = sibling(store_dir, KEPT);
    if kept.exists() {
        bail!(
            "{} already exists: an earlier move kept the old store there, and this one will not \
             write over it",
            kept.display()
        );
    }
    if building.exists() {
        std::fs::remove_dir_all(&building)
            .with_context(|| format!("clear the half-built {}", building.display()))?;
    }

    let iri = |n: NamedNode| -> NamedNode {
        match n.as_str().strip_prefix(&old_base) {
            Some(rest) => NamedNode::new_unchecked(format!("{new_base}{rest}")),
            None => n,
        }
    };
    fn subject(s: NamedOrBlankNode, iri: &dyn Fn(NamedNode) -> NamedNode) -> NamedOrBlankNode {
        match s {
            NamedOrBlankNode::NamedNode(n) => NamedOrBlankNode::NamedNode(iri(n)),
            b => b,
        }
    }
    fn term(t: Term, iri: &dyn Fn(NamedNode) -> NamedNode) -> Term {
        match t {
            Term::NamedNode(n) => Term::NamedNode(iri(n)),
            Term::Triple(t) => {
                let t = *t;
                Term::Triple(Box::new(Triple::new(
                    subject(t.subject, iri),
                    iri(t.predicate),
                    term(t.object, iri),
                )))
            }
            other => other,
        }
    }

    let old = graph::open_read_only(store_dir)?;
    let old_memory = format!("{old_base}NamedGraph/{OLD_MEMORY_GRAPH}");
    let turns_before = turns(&old)?;

    let new = graph::open(&building)?;
    let mut copied = 0usize;
    let mut memory_left_out = 0usize;
    let mut graphs: BTreeSet<String> = BTreeSet::new();
    {
        let mut loader = new.bulk_loader();
        let moved = old.iter().filter_map(|q| {
            let q = match q {
                Ok(q) => q,
                Err(e) => return Some(Err(anyhow::Error::from(e))),
            };
            if matches!(&q.graph_name, GraphName::NamedNode(g) if g.as_str() == old_memory) {
                memory_left_out += 1;
                return None;
            }
            let graph_name = match q.graph_name {
                GraphName::NamedNode(g) => {
                    let g = iri(g);
                    if !graphs.contains(g.as_str()) {
                        graphs.insert(g.as_str().to_string());
                    }
                    GraphName::NamedNode(g)
                }
                other => other,
            };
            copied += 1;
            Some(Ok(Quad::new(
                subject(q.subject, &iri),
                iri(q.predicate),
                term(q.object, &iri),
                graph_name,
            )))
        });
        loader.load_ok_quads::<anyhow::Error, anyhow::Error>(moved)?;
        loader.commit()?;
    }
    for g in &graphs {
        let g = NamedNode::new(g)?;
        graph::describe_graph(&new, names, &g, GraphKind::Transcript)?;
    }
    new.flush()?;

    // Nothing is put in place unless the two stores agree.
    let turns_after = turns(&new)?;
    let total = old.len()?;
    if turns_after != turns_before || copied + memory_left_out != total {
        drop(new);
        let _ = std::fs::remove_dir_all(&building);
        bail!(
            "{}: the moved store does not match and was thrown away. Turns {turns_before} before, \
             {turns_after} after; {total} statements before, {copied} copied and \
             {memory_left_out} memory statements left out. The old store is untouched",
            store_dir.display()
        );
    }
    drop(new);
    drop(old);
    std::fs::rename(store_dir, &kept)
        .with_context(|| format!("set the old store aside at {}", kept.display()))?;
    if let Err(e) = std::fs::rename(&building, store_dir) {
        // Put the old one back, so the soul is never left with no store.
        let _ = std::fs::rename(&kept, store_dir);
        return Err(e).with_context(|| format!("put the moved store at {}", store_dir.display()));
    }
    Ok(Moved {
        copied,
        memory_left_out,
        graphs: graphs.len(),
        turns: turns_after,
        kept,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxigraph::io::RdfFormat;

    const OLD: &str = r#"
<https://repolex.ai/ravel/Turn/t1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <https://repolex.ai/ontology/ravel/Turn> <https://repolex.ai/ravel/NamedGraph/a> .
<https://repolex.ai/ravel/Turn/t1> <https://repolex.ai/ontology/ravel/text> "see <https://repolex.ai/ravel/Turn/t0>" <https://repolex.ai/ravel/NamedGraph/a> .
<https://repolex.ai/ravel/Turn/t2> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <https://repolex.ai/ontology/ravel/Turn> <https://repolex.ai/ravel/NamedGraph/b> .
<https://repolex.ai/ravel/Claim/c1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> <<( <https://repolex.ai/ravel/Turn/t2> <https://repolex.ai/ontology/ravel/replyTo> <https://repolex.ai/ravel/Turn/t1> )>> <https://repolex.ai/ravel/NamedGraph/b> .
<https://repolex.ai/ravel/Turn/t2> <https://repolex.ai/ontology/ravel/claimedSenderSoul> <https://repolex.ai/7r1pl3f0rc3/m4xx/soul/Soul/m4xx> <https://repolex.ai/ravel/NamedGraph/b> .
<https://repolex.ai/ravel/Memory/0-1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <https://repolex.ai/ontology/ravel/Memory> <https://repolex.ai/ravel/NamedGraph/memory-v1> .
"#;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ravel-readdress-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_store_moves_whole_and_the_old_one_is_kept() {
        let dir = scratch("move");
        let store_dir = dir.join("oxigraph");
        {
            let s = graph::open(&store_dir).unwrap();
            s.load_from_reader(RdfFormat::NQuads, OLD.as_bytes()).unwrap();
            s.flush().unwrap();
        }
        let names = Names::new("7r1pl3f0rc3", "spacegoat").unwrap();
        let moved = move_store(&store_dir, &names).unwrap();
        assert_eq!(moved.copied, 5);
        assert_eq!(moved.memory_left_out, 1);
        assert_eq!(moved.graphs, 2);
        assert_eq!(moved.turns, 2);
        assert!(moved.kept.join("CURRENT").exists(), "the old store is kept");

        let s = graph::open(&store_dir).unwrap();
        assert!(!is_old(&s, &names).unwrap());
        let root = "https://repolex.ai/7r1pl3f0rc3/spacegoat/ravel/";
        let ask = |q: &str| graph::query_rows(&s, q).unwrap();
        // No address is left under the old base, as subject, object, graph,
        // or inside a triple term.
        let dump: Vec<String> = s.iter().map(|q| q.unwrap().to_string()).collect();
        for line in &dump {
            let without_text = line.replace("\"see <https://repolex.ai/ravel/Turn/t0>\"", "");
            assert!(
                !without_text.contains("<https://repolex.ai/ravel/"),
                "old address left in: {line}"
            );
        }
        // Text is not an address: the quoted one is exactly as it was.
        assert!(dump
            .iter()
            .any(|l| l.contains("\"see <https://repolex.ai/ravel/Turn/t0>\"")));
        // Another soul's address is not ravel's to move.
        assert!(dump
            .iter()
            .any(|l| l.contains("<https://repolex.ai/7r1pl3f0rc3/m4xx/soul/Soul/m4xx>")));
        // The triple term moved with everything else.
        assert!(dump
            .iter()
            .any(|l| l.contains(&format!("<<( <{root}Turn/t2>"))));
        // Each transcript graph is described in the unnamed graph.
        let described = ask(
            "SELECT ?g ?o WHERE { ?g a <https://repolex.ai/ontology/ravel/TranscriptGraph> ; \
             <https://repolex.ai/ontology/ravel/owner> ?o ; \
             <https://repolex.ai/ontology/ravel/visibility> \"private\" } ORDER BY ?g",
        );
        assert_eq!(described.len(), 2);
        assert_eq!(described[0]["g"], format!("{root}NamedGraph/a"));
        assert_eq!(
            described[0]["o"],
            "https://repolex.ai/7r1pl3f0rc3/spacegoat/soul/Soul/spacegoat"
        );
        drop(s);

        // A second move has nothing to do and will not write over the kept store.
        assert!(move_store(&store_dir, &names).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The move on a copy of a real soul's `.lex/repo.yml` and `.ravel/`,
    /// never on the soul itself:
    /// `RAVEL_TRIAL_REPO=<copy> cargo test --release real_store_move -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_store_move() {
        let repo = PathBuf::from(std::env::var("RAVEL_TRIAL_REPO").expect("RAVEL_TRIAL_REPO"));
        let names = Names::for_soul(&repo).unwrap();
        let store_dir = repo.join(crate::sync::STORE_SUBDIR);
        let counts = |s: &Store, label: &str| {
            let one = |q: &str| {
                graph::query_rows(s, q).unwrap()[0]
                    .values()
                    .next()
                    .cloned()
                    .unwrap_or_default()
            };
            let r = "https://repolex.ai/ontology/ravel/";
            println!("{label}");
            println!("  statements        {}", s.len().unwrap());
            println!("  named graphs      {}", one("SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }"));
            println!("  turns             {}", one(&format!("SELECT (COUNT(DISTINCT ?t) AS ?n) WHERE {{ GRAPH ?g {{ ?t a <{r}Turn> }} }}")));
            println!("  turn rows         {}", one(&format!("SELECT (COUNT(?t) AS ?n) WHERE {{ GRAPH ?g {{ ?t a <{r}Turn> }} }}")));
            println!("  memory notes      {}", one(&format!("SELECT (COUNT(?m) AS ?n) WHERE {{ GRAPH ?g {{ ?m <{r}memoryLevel> 0 }} }}")));
            println!("  memory merges     {}", one(&format!("SELECT (COUNT(?m) AS ?n) WHERE {{ GRAPH ?g {{ ?m <{r}memoryLevel> ?l FILTER(?l > 0) }} }}")));
            println!("  note-to-turn links that land on a turn  {}", one(&format!("SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH ?mg {{ ?m <{r}memoryLevel> 0 ; <{r}relatedToId> ?t }} FILTER EXISTS {{ GRAPH ?g {{ ?t a <{r}Turn> }} }} }}")));
            println!("  graphs described  {}", one(&format!("SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE {{ ?g <{r}visibility> ?v }}")));
            println!("  old addresses     {}", one("SELECT (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } FILTER(STRSTARTS(STR(?s), \"https://repolex.ai/ravel/\")) }"));
        };
        {
            let s = graph::open_read_only(&store_dir).unwrap();
            counts(&s, "BEFORE");
        }
        let t = std::time::Instant::now();
        let s = open_current(&repo, &names).unwrap();
        println!("moved and memory rebuilt in {:.1}s", t.elapsed().as_secs_f64());
        counts(&s, "AFTER");
        // What an outside caller sees: base names, and a short address read
        // against this soul's root.
        let p = "PREFIX subtexture: <https://repolex.ai/ontology/subtexture/> PREFIX ravel: <https://repolex.ai/ontology/ravel/>";
        for (label, q) in [
            ("?g a subtexture:NamedGraph", format!("{p} SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE {{ ?g a subtexture:NamedGraph }}")),
            ("?g a subtexture:DefaultGraph", format!("{p} SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE {{ ?g a subtexture:DefaultGraph }}")),
            ("?g subtexture:owner ?o", format!("{p} SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE {{ ?g subtexture:owner ?o }}")),
            ("memory in GRAPH <ravel/NamedGraph/optmem>", format!("{p} SELECT (COUNT(?m) AS ?n) WHERE {{ GRAPH <ravel/NamedGraph/optmem> {{ ?m a ravel:Memory }} }}")),
            ("turns, no GRAPH", format!("{p} SELECT (COUNT(?t) AS ?n) WHERE {{ ?t a ravel:Turn }}")),
        ] {
            let t = std::time::Instant::now();
            let (r, _) = crate::outside::run(&s, &q, names.root()).unwrap();
            let oxigraph::sparql::QueryResults::Solutions(sols) = r else { panic!() };
            let rows: Vec<_> = sols.map(|r| r.unwrap()).collect();
            println!("  outside: {label} = {}  ({:.2}s)", rows[0].iter().map(|(_, v)| v.to_string()).collect::<String>(), t.elapsed().as_secs_f64());
        }
    }

    #[test]
    fn a_repo_with_no_squad_is_never_old() {
        let dir = scratch("bare");
        let s = graph::open(dir.join("oxigraph")).unwrap();
        s.load_from_reader(RdfFormat::NQuads, OLD.as_bytes()).unwrap();
        assert!(!is_old(&s, &Names::bare()).unwrap());
        assert!(is_old(&s, &Names::new("7r1pl3f0rc3", "spacegoat").unwrap()).unwrap());
        drop(s);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
