//! The memory on disk: OptMem's log and tree as append-only files, the
//! canonical bytes. The graph is a projection of them, the same rule as for
//! transcripts. Everything lives under `<soul>/.ravel/_ignore/optmem/`:
//!
//! - `memories.jsonl`: OptMem's LOG. One line per note, in the order noted;
//!   the line's position is the note's OptMem identity (`#0, #1, …`). Each
//!   note also carries the turns it rests on — ravel's graph edges. Never
//!   edited.
//! - `summaries.jsonl`: OptMem's TREE. One line per block written, keyed by
//!   its OptMem id (`16-31`). Append-only; the last line for a block wins
//!   (OptMem's `forget` then `nap` rewrites a block the same way).
//! - `extracted.tsv`: turn ids already read, so a turn is read once however
//!   many transcript files repeat it.
//! - `dropped.tsv`: notes refused, and why. Audit only.

use crate::memory::{self, Block};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Where a soul's memory lives. (`memory/` and `memory-relive/` beside it
/// hold earlier runs, kept until goodlux decides their fate; nothing reads
/// them.)
pub const MEMORY_SUBDIR: &str = ".ravel/_ignore/optmem";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Memory {
    pub id: String,
    /// RFC 3339: the time of the earliest turn it rests on.
    pub ts: String,
    pub text: String,
    /// Turn ids (the same ids as the store's `ravel:turnId`).
    pub turns: Vec<String>,
    pub model: String,
}

impl Memory {
    /// OptMem's date for a note: the day it was noted, on this machine's
    /// clock (`datetime.date.today()`), here the local day of its first turn.
    pub fn date(&self) -> String {
        local_date(&self.ts)
    }
}

/// The local calendar day of an RFC 3339 time, `YYYY-MM-DD`.
pub fn local_date(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|_| ts.get(..10).unwrap_or(ts).to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    /// The block's OptMem id, inclusive at both ends: `16-31`.
    pub block: String,
    pub text: String,
    pub model: String,
    pub written: String,
}

pub struct MemoryLog {
    dir: PathBuf,
}

impl MemoryLog {
    pub fn for_soul(repo: &Path) -> Self {
        MemoryLog {
            dir: repo.join(MEMORY_SUBDIR),
        }
    }

    /// A log in any directory: a test writes beside the soul, never into its
    /// `.ravel/`.
    pub fn at(dir: PathBuf) -> Self {
        MemoryLog { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn memories(&self) -> Result<Vec<Memory>> {
        read_jsonl(&self.path("memories.jsonl"))
    }

    /// Every block written, last write per block winning.
    pub fn summaries(&self) -> Result<HashMap<String, Summary>> {
        let mut m = HashMap::new();
        for s in read_jsonl::<Summary>(&self.path("summaries.jsonl"))? {
            m.insert(s.block.clone(), s);
        }
        Ok(m)
    }

    pub fn extracted(&self) -> Result<HashSet<String>> {
        let p = self.path("extracted.tsv");
        if !p.exists() {
            return Ok(HashSet::new());
        }
        Ok(std::fs::read_to_string(&p)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Record a stretch's results: its notes first, then its turns as read.
    /// In that order on purpose: a crash between the two re-reads the stretch
    /// (a duplicate note, harmless and visible) rather than losing it.
    pub fn append_chunk(&self, memories: &[Memory], turn_ids: &[String]) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        append_jsonl(&self.path("memories.jsonl"), memories)?;
        let mut f = open_append(&self.path("extracted.tsv"))?;
        for t in turn_ids {
            writeln!(f, "{t}")?;
        }
        Ok(())
    }

    /// Notes refused, one per line with the reason: the audit trail, never
    /// read back.
    pub fn append_dropped(&self, lines: &[String]) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        let mut f = open_append(&self.path("dropped.tsv"))?;
        for l in lines {
            writeln!(
                f,
                "{}\t{}",
                chrono::Utc::now().to_rfc3339(),
                l.replace(['\n', '\t'], " ")
            )?;
        }
        Ok(())
    }

    pub fn append_summary(&self, s: &Summary) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        append_jsonl(&self.path("summaries.jsonl"), std::slice::from_ref(s))
    }
}

/// How many blocks of `size` are built as a dense prefix from position 0:
/// OptMem's level-file length.
pub fn have(summaries: &HashMap<String, Summary>, size: usize) -> usize {
    let mut k = 0;
    while summaries.contains_key(&memory::block_id((k * size, (k + 1) * size))) {
        k += 1;
    }
    k
}

/// The blocks due and not built over the first `t` notes, in OptMem's order.
pub fn pending(summaries: &HashMap<String, Summary>, t: usize) -> Vec<Block> {
    memory::pending(t, |s| have(summaries, s), None)
}

/// The merge prompt for block `b`, as OptMem's `nap_prompt` writes it.
pub fn nap_prompt(
    memories: &[Memory],
    summaries: &HashMap<String, Summary>,
    b: Block,
    left: usize,
) -> Result<String> {
    let body = memory::nap_body(
        b,
        |i| (memories[i].date(), memories[i].text.clone()),
        |c| summaries.get(&memory::block_id(c)).map(|s| s.text.clone()),
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    Ok(memory::nap_prompt(b, &body, left))
}

/// One line of the wake view.
#[derive(Debug, Clone, Serialize)]
pub struct Line {
    /// What opens this line: a note's id, or a block's OptMem id (`16-31`).
    pub id: String,
    /// The OptMem label: `#12` for a note, `#16-31` for a block.
    pub label: String,
    /// 0 for a note; log2 of the block's size for a block.
    pub level: u32,
    /// The time of the first and of the last note under it.
    pub from: String,
    pub to: String,
    pub text: String,
    /// The note's date (OptMem prints it on note lines only). For a block,
    /// the date of its first note.
    pub date: String,
    /// The date of the last note under it: a block's date span is
    /// `date..date_to`. Ravel's wake display prints the span on block lines
    /// (goodlux, 2026-10-15) so a superseded fact does not read as current
    /// beside its replacement; OptMem's own wake, and the reader's, print none.
    pub date_to: String,
    /// True for a block not built yet: OptMem's wake would stop and ask for
    /// the merge; a reader shows that it is missing instead of its text.
    pub pending: bool,
}

/// The line for a note at position `i` or a block, as wake shows it.
pub fn node_line(memories: &[Memory], summaries: &HashMap<String, Summary>, b: Block) -> Line {
    let (lo, hi) = b;
    let first = &memories[lo];
    let last = &memories[hi - 1];
    if hi - lo == 1 {
        return Line {
            id: first.id.clone(),
            label: format!("#{lo}"),
            level: 0,
            from: first.ts.clone(),
            to: first.ts.clone(),
            text: first.text.clone(),
            date: first.date(),
            date_to: first.date(),
            pending: false,
        };
    }
    let id = memory::block_id(b);
    let (text, pending) = match summaries.get(&id) {
        Some(s) => (s.text.clone(), false),
        None => (format!("(#{id} not compressed yet)"), true),
    };
    Line {
        label: format!("#{id}"),
        id,
        level: (hi - lo).trailing_zeros(),
        from: first.ts.clone(),
        to: last.ts.clone(),
        text,
        date: first.date(),
        date_to: last.date(),
        pending,
    }
}

/// The wake view over the first `t` notes: OptMem's `cover(t, budget)`.
pub fn wake_view(
    memories: &[Memory],
    summaries: &HashMap<String, Summary>,
    t: usize,
    budget: usize,
) -> Vec<Line> {
    let t = t.min(memories.len());
    memory::cover(t, budget)
        .into_iter()
        .map(|b| node_line(memories, summaries, b))
        .collect()
}

/// The wake view as OptMem prints it: `#i date text` and `#lo-hi text`.
pub fn wake_text(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|l| {
            if l.level == 0 {
                format!("{} {} {}", l.label, l.date, l.text)
            } else {
                format!("{} {}", l.label, l.text)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A block opened into its two halves, as OptMem's `zoom` does.
pub fn zoom(memories: &[Memory], summaries: &HashMap<String, Summary>, b: Block) -> Vec<Line> {
    let (lo, hi) = b;
    let mid = (lo + hi) / 2;
    [(lo, mid), (mid, hi)]
        .into_iter()
        .filter(|&(a, _)| a < memories.len())
        .map(|(a, z)| node_line(memories, summaries, (a, z.min(memories.len()))))
        .collect()
}

/// N-Triples for the whole memory, for the soul's `memory-v1` graph.
///
/// One node per note and per built block, all `ravel:Memory`. Every edge is
/// `git-lex:relatedToId` (goodlux, 2026-10-06): a note to the turns it rests
/// on, a block to its two halves — the tree OptMem's `zoom` walks.
pub fn project_nt(memories: &[Memory], summaries: &HashMap<String, Summary>) -> String {
    use crate::{RAVEL_BASE, RAVEL_NS};
    let iri = |id: &str| format!("<{RAVEL_BASE}Memory/{id}>");
    let p = |name: &str| format!("<{RAVEL_NS}{name}>");
    let lit = |s: &str| {
        format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
        )
    };
    let dt = |s: &str| format!("{}^^<http://www.w3.org/2001/XMLSchema#dateTime>", lit(s));
    let int = |n: usize| format!("\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>");
    let ty = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>";
    let related = "<https://repolex.ai/ontology/git-lex/relatedToId>";
    let node_iri = |(lo, hi): Block| {
        if hi - lo == 1 {
            iri(&memories[lo].id)
        } else {
            iri(&memory::block_id((lo, hi)))
        }
    };
    let mut nt = String::new();
    for (i, m) in memories.iter().enumerate() {
        let s = iri(&m.id);
        nt += &format!("{s} {ty} {} .\n", p("Memory"));
        nt += &format!("{s} {} {} .\n", p("memoryId"), lit(&m.id));
        nt += &format!("{s} {} {} .\n", p("memoryLevel"), int(0));
        nt += &format!("{s} {} {} .\n", p("logStart"), int(i));
        nt += &format!("{s} {} {} .\n", p("logEnd"), int(i));
        nt += &format!("{s} {} {} .\n", p("memoryText"), lit(&m.text));
        nt += &format!("{s} {} {} .\n", p("windowStart"), dt(&m.ts));
        nt += &format!("{s} {} {} .\n", p("windowEnd"), dt(&m.ts));
        nt += &format!("{s} {} {} .\n", p("memoryModel"), lit(&m.model));
        for t in &m.turns {
            nt += &format!("{s} {related} <{RAVEL_BASE}Turn/{t}> .\n");
        }
    }
    let mut blocks: Vec<(&String, &Summary)> = summaries.iter().collect();
    blocks.sort_by(|a, b| a.0.cmp(b.0));
    for (id, sm) in blocks {
        let Some((lo, hi)) = memory::parse_block(id) else {
            continue;
        };
        if hi > memories.len() {
            continue; // a block past the log: cannot happen, but never project one
        }
        let s = iri(id);
        nt += &format!("{s} {ty} {} .\n", p("Memory"));
        nt += &format!("{s} {} {} .\n", p("memoryId"), lit(id));
        nt += &format!(
            "{s} {} {} .\n",
            p("memoryLevel"),
            int((hi - lo).trailing_zeros() as usize)
        );
        nt += &format!("{s} {} {} .\n", p("logStart"), int(lo));
        nt += &format!("{s} {} {} .\n", p("logEnd"), int(hi - 1));
        nt += &format!("{s} {} {} .\n", p("memoryText"), lit(&sm.text));
        nt += &format!("{s} {} {} .\n", p("windowStart"), dt(&memories[lo].ts));
        nt += &format!("{s} {} {} .\n", p("windowEnd"), dt(&memories[hi - 1].ts));
        nt += &format!("{s} {} {} .\n", p("memoryModel"), lit(&sm.model));
        let mid = (lo + hi) / 2;
        for half in [(lo, mid), (mid, hi)] {
            nt += &format!("{s} {related} {} .\n", node_iri(half));
        }
    }
    nt
}

/// The named graph the memory projects into.
pub const MEMORY_GRAPH: &str = "memory-v1";

/// Replace the soul's `memory-v1` graph with a fresh projection of its logs.
/// A soul with no memory yet gets an empty graph: an older projection (the
/// clock-window index before 0.7.0) must not stand in for a memory that is
/// not there. A no-op when the soul has no store yet. The caller holds the
/// daemon's gate: this opens the store read-write.
pub fn project_into_store(repo: &Path) -> Result<()> {
    let log = MemoryLog::for_soul(repo);
    let memories = log.memories()?;
    let store_dir = repo.join(crate::sync::STORE_SUBDIR);
    if !store_dir.join("CURRENT").exists() {
        return Ok(());
    }
    let summaries = log.summaries()?;
    let store = crate::graph::open(&store_dir)?;
    let g = oxigraph::model::NamedNode::new(crate::graph::transcript_graph_iri(MEMORY_GRAPH))?;
    store.clear_graph(oxigraph::model::GraphNameRef::NamedNode(g.as_ref()))?;
    if memories.is_empty() {
        store.flush()?;
        return Ok(());
    }
    let parser = oxigraph::io::RdfParser::from_format(oxigraph::io::RdfFormat::NTriples)
        .with_default_graph(g);
    store.load_from_reader(parser, project_nt(&memories, &summaries).as_bytes())?;
    store.flush()?;
    Ok(())
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(p: &Path) -> Result<Vec<T>> {
    if !p.exists() {
        return Ok(Vec::new());
    }
    let mut s = std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
    // A writer appends whole lines; a last line with no newline yet is one
    // being written right now (the relive backfill writes while raveld
    // reads). Leave it for the next read.
    if !s.ends_with('\n') {
        s.truncate(s.rfind('\n').map_or(0, |i| i + 1));
    }
    let mut out = Vec::new();
    for (i, line) in s.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(line)
                .with_context(|| format!("{} line {}: not a valid record", p.display(), i + 1))?,
        );
    }
    Ok(out)
}

fn open_append(p: &Path) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
        .with_context(|| format!("open {} for append", p.display()))
}

fn append_jsonl<T: Serialize>(p: &Path, items: &[T]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let mut buf = String::new();
    for it in items {
        buf += &serde_json::to_string(it)?;
        buf.push('\n');
    }
    let mut f = open_append(p)?;
    f.write_all(buf.as_bytes())?;
    f.sync_data()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(i: usize) -> Memory {
        Memory {
            id: format!("m{i}"),
            ts: format!("2024-01-01T{:02}:00:00Z", i % 24),
            text: format!("note {i}"),
            turns: vec![format!("t{i}")],
            model: "test".into(),
        }
    }

    fn sum(b: Block, text: &str) -> Summary {
        Summary {
            block: memory::block_id(b),
            text: text.into(),
            model: "t".into(),
            written: "x".into(),
        }
    }

    #[test]
    fn pending_follows_what_is_built() {
        let mut s = HashMap::new();
        assert_eq!(pending(&s, 4), vec![(0, 2), (2, 4), (0, 4)]);
        s.insert("0-1".into(), sum((0, 2), "a"));
        assert_eq!(pending(&s, 4), vec![(2, 4), (0, 4)]);
        // a gap at 0-1 keeps 2-3 pending too: levels are dense prefixes
        let mut g = HashMap::new();
        g.insert("2-3".into(), sum((2, 4), "b"));
        assert_eq!(pending(&g, 4), vec![(0, 2), (2, 4), (0, 4)]);
    }

    #[test]
    fn wake_prints_optmem_lines() {
        let ms: Vec<Memory> = (0..200).map(mem).collect();
        let mut s = HashMap::new();
        for b in pending(&HashMap::new(), 200) {
            s.insert(
                memory::block_id(b),
                sum(b, &format!("S{}", memory::block_id(b))),
            );
        }
        let v = wake_view(&ms, &s, 200, 96);
        assert!(v.len() <= 96);
        assert!(v.iter().all(|l| !l.pending));
        let text = wake_text(&v);
        assert!(text.starts_with("#0-"), "{text}");
        assert!(text.ends_with(&format!("#199 {} note 199", ms[199].date())));
        // a missing block is shown as missing, never as someone else's text
        let v = wake_view(&ms, &HashMap::new(), 200, 96);
        assert!(v[0].pending && v[0].text.contains("not compressed yet"));
    }

    #[test]
    fn logs_round_trip_and_last_summary_wins() {
        let dir = std::env::temp_dir().join("ravel-optmem-log-test");
        let _ = std::fs::remove_dir_all(&dir);
        let log = MemoryLog::at(dir.clone());
        log.append_chunk(&[mem(0)], &["t1".into(), "t2".into()])
            .unwrap();
        assert_eq!(log.memories().unwrap().len(), 1);
        assert!(log.extracted().unwrap().contains("t2"));
        log.append_summary(&sum((0, 2), "first")).unwrap();
        log.append_summary(&sum((0, 2), "second")).unwrap();
        assert_eq!(log.summaries().unwrap()["0-1"].text, "second");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn projection_links_notes_to_turns_and_blocks_to_halves() {
        let ms: Vec<Memory> = (0..4).map(mem).collect();
        let mut s = HashMap::new();
        for b in [(0, 2), (2, 4), (0, 4)] {
            s.insert(memory::block_id(b), sum(b, "x"));
        }
        let nt = project_nt(&ms, &s);
        let rel = "<https://repolex.ai/ontology/git-lex/relatedToId>";
        let m = |id: &str| format!("<https://repolex.ai/ravel/Memory/{id}>");
        assert!(nt.contains(&format!(
            "{} {rel} <https://repolex.ai/ravel/Turn/t0> .",
            m("m0")
        )));
        assert!(nt.contains(&format!("{} {rel} {} .", m("0-1"), m("m0"))));
        assert!(nt.contains(&format!("{} {rel} {} .", m("0-1"), m("m1"))));
        assert!(nt.contains(&format!("{} {rel} {} .", m("0-3"), m("0-1"))));
        assert!(nt.contains(&format!("{} {rel} {} .", m("0-3"), m("2-3"))));
        let store = oxigraph::store::Store::new().unwrap();
        store
            .load_from_reader(oxigraph::io::RdfFormat::NTriples, nt.as_bytes())
            .unwrap();
    }

    /// The ontology declares what the projection emits, and nothing it
    /// emits is undeclared.
    #[test]
    fn the_ontology_declares_every_memory_term_the_projection_emits() {
        let ttl = include_str!("../ontology/ravel/ravel.ttl");
        let store = oxigraph::store::Store::new().unwrap();
        store
            .load_from_reader(oxigraph::io::RdfFormat::Turtle, ttl.as_bytes())
            .expect("ravel.ttl parses");
        let ms: Vec<Memory> = (0..2).map(mem).collect();
        let mut s = HashMap::new();
        s.insert("0-1".to_string(), sum((0, 2), "x"));
        let nt = project_nt(&ms, &s);
        let ns = crate::RAVEL_NS;
        let mut terms: Vec<String> = nt
            .split_whitespace()
            .filter_map(|w| w.strip_prefix(&format!("<{ns}")))
            .map(|w| w.trim_end_matches('>').to_string())
            .collect();
        terms.sort();
        terms.dedup();
        assert!(terms.contains(&"logStart".to_string()));
        for t in terms {
            let q = format!("ASK {{ <{ns}{t}> a ?k }}");
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
