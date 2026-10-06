//! The memory index on disk: append-only logs, the canonical bytes.
//!
//! The graph is a projection of these files, the same rule as transcripts:
//! the bytes are canonical, the store is rebuilt from them. Everything lives
//! under `<soul>/.ravel/_ignore/memory/`:
//!
//! - `memories.jsonl`: one line per memory — id, time, text, the turns it
//!   rests on, the model that wrote it. Never edited.
//! - `summaries.jsonl`: one line per window summary written. A window can be
//!   summarized again when what is under it changes (a back-dated import);
//!   the LAST line for a window wins and the earlier ones stay as history.
//! - `extracted.tsv`: turn ids already read by the extractor, so each turn is
//!   read once however many transcript files repeat it.
//! - `dropped.tsv`: memories the number check refused (a number the model
//!   was never shown), with that number. Audit only.

use crate::memory::{hour_of, hour_to_rfc3339, Node, Rule, Tree, Window};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const MEMORY_SUBDIR: &str = ".ravel/_ignore/memory";

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    /// `w<level>-<index>`.
    pub window: String,
    /// Hash of the children's ids and texts when this was written. A window
    /// whose children now hash differently is stale.
    pub digest: String,
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

    /// A log in any directory: a relive test writes beside the soul, never
    /// into its `.ravel/`.
    pub fn at(dir: PathBuf) -> Self {
        MemoryLog { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn memories(&self) -> Result<Vec<Memory>> {
        read_jsonl(&self.path("memories.jsonl"))
    }

    /// Every summary, last write per window winning.
    pub fn summaries(&self) -> Result<HashMap<String, Summary>> {
        let mut m = HashMap::new();
        for s in read_jsonl::<Summary>(&self.path("summaries.jsonl"))? {
            m.insert(s.window.clone(), s);
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

    /// Record a chunk's results: its memories first, then its turns as read.
    /// In that order on purpose — a crash between the two re-reads the chunk
    /// (a duplicate memory, harmless and visible) rather than losing it.
    pub fn append_chunk(&self, memories: &[Memory], turn_ids: &[String]) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        append_jsonl(&self.path("memories.jsonl"), memories)?;
        let mut f = open_append(&self.path("extracted.tsv"))?;
        for t in turn_ids {
            writeln!(f, "{t}")?;
        }
        Ok(())
    }

    /// Memories the number check threw away, one per line with the number it
    /// could not find: the audit trail for the guard, never read back.
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

/// The tree over a soul's memories, and the memories by id.
pub fn tree_of(memories: &[Memory]) -> (Tree, HashMap<String, &Memory>) {
    let by_id: HashMap<String, &Memory> = memories.iter().map(|m| (m.id.clone(), m)).collect();
    let tree = Tree::new(
        memories
            .iter()
            .filter_map(|m| hour_of(&m.ts).map(|h| (m.id.clone(), h))),
    );
    (tree, by_id)
}

/// The text standing for a node: a memory's own text, or its window's
/// summary. `None` for a window not summarized yet.
pub fn node_text(
    n: &Node,
    by_id: &HashMap<String, &Memory>,
    summaries: &HashMap<String, Summary>,
) -> Option<String> {
    match n {
        Node::Memory(id) => by_id.get(id).map(|m| m.text.clone()),
        Node::Window(w) => summaries.get(&w.id()).map(|s| s.text.clone()),
    }
}

/// The digest a window's summary must have been written from to be current.
pub fn children_digest(
    tree: &Tree,
    w: Window,
    by_id: &HashMap<String, &Memory>,
    summaries: &HashMap<String, Summary>,
) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for c in tree.children(w) {
        let id = match &c {
            Node::Memory(id) => id.clone(),
            Node::Window(cw) => cw.id(),
        };
        h.update(id.as_bytes());
        h.update([0]);
        h.update(node_text(&c, by_id, summaries)?.as_bytes());
        h.update([0]);
    }
    Some(format!("{:x}", h.finalize())[..16].to_string())
}

/// One line of the wake view.
#[derive(Debug, Clone, Serialize)]
pub struct Line {
    /// A memory id, or a window id (`w<level>-<index>`) to open.
    pub id: String,
    pub level: u8,
    pub from: String,
    pub to: String,
    pub text: String,
    /// True when the line stands in for a summary not written yet, or one
    /// gone stale: the UI can mark it without matching its text.
    pub pending: bool,
    /// Which rule put the line in the view: "age", "present" or "opened".
    pub rule: &'static str,
}

/// The wake view within a character budget: the most lines (up to
/// `max_lines`) whose texts add up to at most `max_chars`. Fewer lines
/// forces the age rule to coarser windows in the past — a line budget alone
/// let a bursty history fill 96 lines with 1- to 32-hour pieces and never use
/// its multi-week summaries (w3bl0rd, 2026-10-05).
pub fn wake_view_fit(
    memories: &[Memory],
    summaries: &HashMap<String, Summary>,
    now_hour: i64,
    max_lines: usize,
    max_chars: usize,
) -> Vec<Line> {
    let size = |v: &[Line]| v.iter().map(|l| l.text.chars().count()).sum::<usize>();
    let (mut lo, mut hi) = (1usize, max_lines.max(1));
    let mut best = wake_view(memories, summaries, now_hour, lo);
    if size(&best) > max_chars {
        return best;
    }
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let v = wake_view(memories, summaries, now_hour, mid);
        if size(&v) <= max_chars {
            lo = mid;
            best = v;
        } else {
            hi = mid - 1;
        }
    }
    best
}

/// The wake view: the whole history in at most `budget` lines.
///
/// A window whose summary is missing or stale is opened — its children shown
/// instead — while the view has room; past the budget it is one line saying it
/// is unsummarized or stale. Never a summary passed off as covering memories it
/// does not mention, and never more than `budget` lines (the view printed
/// 3.5 MB mid-backfill before this rule).
pub fn wake_view(
    memories: &[Memory],
    summaries: &HashMap<String, Summary>,
    now_hour: i64,
    budget: usize,
) -> Vec<Line> {
    let (tree, by_id) = tree_of(memories);
    let mut stack: Vec<(Node, Rule)> = tree
        .cover_ruled(now_hour, budget)
        .into_iter()
        .rev()
        .collect();
    let mut out = Vec::new();
    while let Some((n, rule)) = stack.pop() {
        match &n {
            Node::Memory(id) => {
                if let Some(m) = by_id.get(id) {
                    out.push(Line {
                        id: id.clone(),
                        level: 0,
                        from: m.ts.clone(),
                        to: m.ts.clone(),
                        text: m.text.clone(),
                        pending: false,
                        rule: rule.tag(),
                    });
                }
            }
            Node::Window(w) => {
                let current = summaries.get(&w.id()).filter(|s| {
                    children_digest(&tree, *w, &by_id, summaries).as_deref()
                        == Some(s.digest.as_str())
                });
                match current {
                    Some(s) => out.push(Line {
                        id: w.id(),
                        level: w.level + 1,
                        from: hour_to_rfc3339(w.start()),
                        to: hour_to_rfc3339(w.end()),
                        text: s.text.clone(),
                        pending: false,
                        rule: rule.tag(),
                    }),
                    None => {
                        // Open it only while the view stays within budget.
                        // Past that, one honest line stands for the window:
                        // the old summary marked stale, or a count.
                        let kids = tree.children(*w);
                        if out.len() + stack.len() + kids.len() <= budget {
                            for c in kids.into_iter().rev() {
                                stack.push((c, Rule::Opened));
                            }
                            continue;
                        }
                        let n = tree.memories_in(*w).len();
                        let text = match summaries.get(&w.id()) {
                            Some(old) => {
                                format!("[stale — {n} memories under it now] {}", old.text)
                            }
                            None => format!(
                                "[not summarized yet — {n} memories; open {} to read them]",
                                w.id()
                            ),
                        };
                        out.push(Line {
                            id: w.id(),
                            level: w.level + 1,
                            from: hour_to_rfc3339(w.start()),
                            to: hour_to_rfc3339(w.end()),
                            text,
                            pending: true,
                            rule: rule.tag(),
                        });
                    }
                }
            }
        }
    }
    out
}

/// N-Triples for the whole index, for the soul's `memory-v1` graph.
pub fn project_nt(memories: &[Memory], summaries: &HashMap<String, Summary>) -> String {
    use crate::{RAVEL_BASE, RAVEL_NS};
    let (tree, by_id) = tree_of(memories);
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
    let int = |n: u8| format!("\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>");
    let ty = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>";
    let mut nt = String::new();
    for m in memories {
        let s = iri(&m.id);
        nt += &format!("{s} {ty} {} .\n", p("Memory"));
        nt += &format!("{s} {} {} .\n", p("memoryId"), lit(&m.id));
        nt += &format!("{s} {} {} .\n", p("memoryLevel"), int(0));
        nt += &format!("{s} {} {} .\n", p("memoryText"), lit(&m.text));
        nt += &format!("{s} {} {} .\n", p("windowStart"), dt(&m.ts));
        nt += &format!("{s} {} {} .\n", p("windowEnd"), dt(&m.ts));
        nt += &format!("{s} {} {} .\n", p("memoryModel"), lit(&m.model));
        for t in &m.turns {
            nt += &format!("{s} {} <{RAVEL_BASE}Turn/{t}> .\n", p("fromTurn"));
        }
    }
    for (wid, sm) in summaries {
        let Some(w) = Window::parse(wid) else {
            continue;
        };
        if tree.representative(w) != Some(Node::Window(w)) {
            continue; // a window that is no longer a node (cannot shrink today, but say so in code)
        }
        let s = iri(wid);
        nt += &format!("{s} {ty} {} .\n", p("Memory"));
        nt += &format!("{s} {} {} .\n", p("memoryId"), lit(wid));
        nt += &format!("{s} {} {} .\n", p("memoryLevel"), int(w.level + 1));
        nt += &format!("{s} {} {} .\n", p("memoryText"), lit(&sm.text));
        nt += &format!(
            "{s} {} {} .\n",
            p("windowStart"),
            dt(&hour_to_rfc3339(w.start()))
        );
        nt += &format!(
            "{s} {} {} .\n",
            p("windowEnd"),
            dt(&hour_to_rfc3339(w.end()))
        );
        nt += &format!("{s} {} {} .\n", p("memoryModel"), lit(&sm.model));
        for c in tree.children(w) {
            let cid = match c {
                Node::Memory(id) => id,
                Node::Window(cw) => cw.id(),
            };
            nt += &format!("{s} {} {} .\n", p("summarizes"), iri(&cid));
        }
    }
    let _ = by_id;
    nt
}

/// The named graph the memory index projects into.
pub const MEMORY_GRAPH: &str = "memory-v1";

/// Replace the soul's `memory-v1` graph with a fresh projection of its logs.
/// A no-op when the soul has no memory logs or no store yet. The caller holds
/// the daemon's gate: this opens the store read-write.
pub fn project_into_store(repo: &Path) -> Result<()> {
    let log = MemoryLog::for_soul(repo);
    let memories = log.memories()?;
    let store_dir = repo.join(crate::sync::STORE_SUBDIR);
    if memories.is_empty() || !store_dir.join("CURRENT").exists() {
        return Ok(());
    }
    let summaries = log.summaries()?;
    let store = crate::graph::open(&store_dir)?;
    let g = oxigraph::model::NamedNode::new(crate::graph::transcript_graph_iri(MEMORY_GRAPH))?;
    store.clear_graph(oxigraph::model::GraphNameRef::NamedNode(g.as_ref()))?;
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
    let s = std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
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

    fn mem(id: &str, ts: &str) -> Memory {
        Memory {
            id: id.into(),
            ts: ts.into(),
            text: format!("text {id}"),
            turns: vec![format!("t-{id}")],
            model: "test".into(),
        }
    }

    #[test]
    fn a_window_without_a_current_summary_is_shown_opened() {
        let ms = vec![
            mem("a", "2024-01-01T00:10:00Z"),
            mem("b", "2024-01-01T00:20:00Z"),
        ];
        let now = hour_of("2026-01-01T00:00:00Z").unwrap();
        let (tree, by_id) = tree_of(&ms);
        let w = Window::containing(hour_of(&ms[0].ts).unwrap(), 0);
        assert_eq!(tree.representative(w), Some(Node::Window(w)));
        // No summary, room to open: both memories show.
        let v = wake_view(&ms, &HashMap::new(), now, 2);
        assert_eq!(
            v.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        // No summary, no room: one line says so, and the budget holds.
        let v = wake_view(&ms, &HashMap::new(), now, 1);
        assert_eq!(v.len(), 1);
        assert!(
            v[0].text.starts_with("[not summarized yet — 2 memories"),
            "{}",
            v[0].text
        );
        // A current summary: one line.
        let mut sums = HashMap::new();
        let d = children_digest(&tree, w, &by_id, &sums).unwrap();
        sums.insert(
            w.id(),
            Summary {
                window: w.id(),
                digest: d,
                text: "both".into(),
                model: "t".into(),
                written: "x".into(),
            },
        );
        let v = wake_view(&ms, &sums, now, 1);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].text, "both");
        // A stale one (a back-dated memory landed in the hour): opened again.
        let mut ms2 = ms.clone();
        ms2.push(mem("c", "2024-01-01T00:05:00Z"));
        let v = wake_view(&ms2, &sums, now, 3);
        assert_eq!(
            v.len(),
            3,
            "the old summary does not mention c, so it must not stand for it"
        );
        // No room to open it: the old text, marked stale with the new count.
        let v = wake_view(&ms2, &sums, now, 1);
        assert_eq!(v.len(), 1);
        assert!(
            v[0].text
                .starts_with("[stale — 3 memories under it now] both"),
            "{}",
            v[0].text
        );
    }

    #[test]
    fn logs_round_trip_and_last_summary_wins() {
        let dir = std::env::temp_dir().join("ravel-memory-log-test");
        let _ = std::fs::remove_dir_all(&dir);
        let log = MemoryLog { dir: dir.clone() };
        log.append_chunk(
            &[mem("a", "2024-01-01T00:10:00Z")],
            &["t1".into(), "t2".into()],
        )
        .unwrap();
        assert_eq!(log.memories().unwrap().len(), 1);
        assert!(log.extracted().unwrap().contains("t2"));
        let s = |t: &str| Summary {
            window: "w0-1".into(),
            digest: "d".into(),
            text: t.into(),
            model: "m".into(),
            written: "x".into(),
        };
        log.append_summary(&s("first")).unwrap();
        log.append_summary(&s("second")).unwrap();
        assert_eq!(log.summaries().unwrap()["w0-1"].text, "second");
    }

    #[test]
    fn projection_links_memories_to_turns_and_summaries_to_children() {
        let ms = vec![
            mem("a", "2024-01-01T00:10:00Z"),
            mem("b", "2024-01-01T00:20:00Z"),
        ];
        let (tree, by_id) = tree_of(&ms);
        let w = Window::containing(hour_of(&ms[0].ts).unwrap(), 0);
        let mut sums = HashMap::new();
        let d = children_digest(&tree, w, &by_id, &sums).unwrap();
        sums.insert(
            w.id(),
            Summary {
                window: w.id(),
                digest: d,
                text: "both".into(),
                model: "t".into(),
                written: "x".into(),
            },
        );
        let nt = project_nt(&ms, &sums);
        assert!(nt.contains("<https://repolex.ai/ravel/Memory/a> <https://repolex.ai/ontology/ravel/fromTurn> <https://repolex.ai/ravel/Turn/t-a> ."));
        assert!(nt.contains(&format!("<https://repolex.ai/ravel/Memory/{}> <https://repolex.ai/ontology/ravel/summarizes> <https://repolex.ai/ravel/Memory/b> .", w.id())));
        // It loads.
        let store = oxigraph::store::Store::new().unwrap();
        store
            .load_from_reader(oxigraph::io::RdfFormat::NTriples, nt.as_bytes())
            .unwrap();
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    /// Mid-backfill: thousands of memories, no summaries yet. The view must
    /// still fit its budget.
    #[test]
    fn a_view_with_no_summaries_still_fits_the_budget() {
        let ms: Vec<Memory> = (0..5000)
            .map(|i| Memory {
                id: format!("m{i}"),
                ts: hour_to_rfc3339(30_000 + i * 2),
                text: format!("memory {i}"),
                turns: vec![format!("t{i}")],
                model: "t".into(),
            })
            .collect();
        let v = wake_view(&ms, &HashMap::new(), 50_000, 96);
        assert!(v.len() <= 96, "{} lines", v.len());
        assert!(v.len() > 50);
    }
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    /// A bursty history with every summary written: a tight character budget
    /// must reach coarser windows than the line budget alone would use.
    #[test]
    fn a_character_budget_climbs_to_coarser_windows() {
        // bursts of 6 memories every ~200 hours, 3,000 memories, long texts
        let ms: Vec<Memory> = (0..3000)
            .map(|i| Memory {
                id: format!("m{i}"),
                ts: hour_to_rfc3339(30_000 + (i / 6) * 200 + (i % 6)),
                text: "x".repeat(150),
                turns: vec![format!("t{i}")],
                model: "t".into(),
            })
            .collect();
        let (tree, by_id) = tree_of(&ms);
        let now = 30_000 + 500 * 200 + 50;
        let mut sums: HashMap<String, Summary> = HashMap::new();
        for w in tree.summary_windows(now) {
            let d = children_digest(&tree, w, &by_id, &sums).unwrap();
            sums.insert(
                w.id(),
                Summary {
                    window: w.id(),
                    digest: d,
                    text: "y".repeat(150),
                    model: "t".into(),
                    written: "x".into(),
                },
            );
        }
        let wide = wake_view_fit(&ms, &sums, now, 96, usize::MAX);
        let tight = wake_view_fit(&ms, &sums, now, 96, 3_000);
        let size = |v: &[Line]| v.iter().map(|l| l.text.chars().count()).sum::<usize>();
        let top = |v: &[Line]| v.iter().map(|l| l.level).max().unwrap();
        assert!(size(&tight) <= 3_000, "{}", size(&tight));
        assert!(tight.len() < wide.len());
        assert!(
            top(&tight) > top(&wide),
            "tight {} vs wide {}",
            top(&tight),
            top(&wide)
        );
        assert!(tight.iter().all(|l| !l.pending));
        assert!(tight.iter().any(|l| l.rule == "age"));
    }
}
