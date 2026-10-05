//! The memory index: one-line memories extracted from transcripts, summarized
//! up a tree of time windows, read at wake under a fixed line budget.
//!
//! After OptMem (github.com/VictorTaelin/OptMem): a log of short memories, a
//! binary tree of one-line summaries over it, and a "cover" that shows the
//! whole history in a fixed number of lines — recent memories verbatim, older
//! ones only through summaries. Asked for by goodlux, 2026-10-04
//! (repolex-ai/ravel#15).
//!
//! **Where it differs from OptMem: blocks are TIME windows, not counts.**
//! OptMem pairs memories by position, so a memory inserted in the past would
//! re-pair everything after it. Ravel's history arrives out of order all the
//! time (an export from 2024 imported in 2026). Here a block is an aligned
//! window of 2^k hours from a fixed epoch, its children are its two halves,
//! and a back-dated memory touches only the windows that contain it — one per
//! level. Nothing else moves.
//!
//! Rules:
//! - A level-0 window (one hour) is a node only when it holds two or more
//!   memories; with one, that memory stands for the hour.
//! - A higher window is a node only when BOTH halves hold memories. With one
//!   non-empty half, the window is that half — no summary of a summary of the
//!   same thing.
//! - A window that has not ended yet is never summarized; the cover opens it.
//!
//! This module is pure: tree shape and cover. Storage and extraction live
//! elsewhere and call in.

use std::collections::BTreeMap;

/// Hour 0. Before the earliest transcript anywhere (2023-11-09).
pub const EPOCH: &str = "2020-01-01T00:00:00Z";

/// The highest level: 2^16 hours is about 7.5 years.
pub const MAX_LEVEL: u8 = 16;

/// Hours since [`EPOCH`] for an RFC 3339 timestamp.
pub fn hour_of(ts: &str) -> Option<i64> {
    let t = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    let e = chrono::DateTime::parse_from_rfc3339(EPOCH).ok()?;
    Some((t - e).num_seconds().div_euclid(3600))
}

/// RFC 3339 for an hour since [`EPOCH`].
pub fn hour_to_rfc3339(h: i64) -> String {
    let e = chrono::DateTime::parse_from_rfc3339(EPOCH).expect("epoch parses");
    (e + chrono::Duration::hours(h))
        .with_timezone(&chrono::Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// An aligned window of 2^level hours: hours `[index·2^level, (index+1)·2^level)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Window {
    pub level: u8,
    pub index: i64,
}

impl Window {
    pub fn containing(hour: i64, level: u8) -> Self {
        Window {
            level,
            index: hour.div_euclid(1i64 << level),
        }
    }
    pub fn size(self) -> i64 {
        1i64 << self.level
    }
    pub fn start(self) -> i64 {
        self.index * self.size()
    }
    /// Exclusive.
    pub fn end(self) -> i64 {
        self.start() + self.size()
    }
    pub fn halves(self) -> Option<(Window, Window)> {
        (self.level > 0).then(|| {
            let l = self.level - 1;
            (
                Window {
                    level: l,
                    index: self.index * 2,
                },
                Window {
                    level: l,
                    index: self.index * 2 + 1,
                },
            )
        })
    }
    /// Stable id, used in IRIs and on the command line: `w<level>-<index>`.
    pub fn id(self) -> String {
        format!("w{}-{}", self.level, self.index)
    }
    pub fn parse(id: &str) -> Option<Window> {
        let (l, i) = id.strip_prefix('w')?.split_once('-')?;
        Some(Window {
            level: l.parse().ok()?,
            index: i.parse().ok()?,
        })
    }
}

/// Why a node is in the wake view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// Kept whole by the age rule: its span is small next to its age.
    Age,
    /// Opened with lines left over after the age rule, nearest the present.
    Present,
    /// Shown because the window above it has no current summary yet.
    Opened,
}

impl Rule {
    pub fn tag(self) -> &'static str {
        match self {
            Rule::Age => "age",
            Rule::Present => "present",
            Rule::Opened => "opened",
        }
    }
}

/// What stands for a span of time in the tree: one memory, or a window node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Node {
    Memory(String),
    Window(Window),
}

/// The tree's shape over a set of memories, each `(id, hour)`.
#[derive(Debug, Default)]
pub struct Tree {
    /// Memory ids by hour, in insertion order within the hour.
    by_hour: BTreeMap<i64, Vec<String>>,
}

impl Tree {
    pub fn new<I, S>(memories: I) -> Self
    where
        I: IntoIterator<Item = (S, i64)>,
        S: Into<String>,
    {
        let mut by_hour: BTreeMap<i64, Vec<String>> = BTreeMap::new();
        for (id, h) in memories {
            by_hour.entry(h).or_default().push(id.into());
        }
        Tree { by_hour }
    }

    pub fn is_empty(&self) -> bool {
        self.by_hour.is_empty()
    }

    fn count_in(&self, w: Window) -> usize {
        self.by_hour
            .range(w.start()..w.end())
            .map(|(_, v)| v.len())
            .sum()
    }

    /// The memories in a level-0 window.
    pub fn memories_in_hour(&self, hour: i64) -> &[String] {
        self.by_hour.get(&hour).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The memories in a window, oldest first.
    pub fn memories_in(&self, w: Window) -> Vec<String> {
        self.by_hour
            .range(w.start()..w.end())
            .flat_map(|(_, v)| v.clone())
            .collect()
    }

    /// What stands for window `w`: itself when it is a real node, else the
    /// one thing inside it. `None` when it is empty.
    pub fn representative(&self, w: Window) -> Option<Node> {
        if w.level == 0 {
            let v = self.by_hour.get(&w.start())?;
            return Some(if v.len() >= 2 {
                Node::Window(w)
            } else {
                Node::Memory(v[0].clone())
            });
        }
        let (a, b) = w.halves()?;
        match (self.count_in(a) > 0, self.count_in(b) > 0) {
            (true, true) => Some(Node::Window(w)),
            (true, false) => self.representative(a),
            (false, true) => self.representative(b),
            (false, false) => None,
        }
    }

    /// The children of a window node, as what stands for each: two
    /// representatives for a higher window, the memories for an hour.
    pub fn children(&self, w: Window) -> Vec<Node> {
        match w.halves() {
            None => self
                .memories_in_hour(w.start())
                .iter()
                .cloned()
                .map(Node::Memory)
                .collect(),
            Some((a, b)) => [a, b]
                .into_iter()
                .filter_map(|h| self.representative(h))
                .collect(),
        }
    }

    /// Every window node that is closed by `now` (ends at or before it), in
    /// the order summaries must be written: children before parents.
    pub fn summary_windows(&self, now_hour: i64) -> Vec<Window> {
        let mut out = std::collections::BTreeSet::new();
        for &h in self.by_hour.keys() {
            for level in 0..=MAX_LEVEL {
                let w = Window::containing(h, level);
                if w.end() <= now_hour && self.representative(w) == Some(Node::Window(w)) {
                    out.insert(w);
                }
            }
        }
        let mut v: Vec<Window> = out.into_iter().collect();
        v.sort_by_key(|w| (w.level, w.index));
        v
    }

    /// The top-level windows that together cover every memory.
    fn roots(&self) -> Vec<Window> {
        let mut roots: Vec<Window> = self
            .by_hour
            .keys()
            .map(|&h| Window::containing(h, MAX_LEVEL))
            .collect();
        roots.dedup();
        roots
    }

    /// The wake view: at most `budget` nodes covering the whole history,
    /// oldest first, coarse in the past and fine near `now_hour`.
    ///
    /// OptMem's rule, on time: a window is kept whole when it is closed and
    /// its size is at most `alpha` times its age; otherwise it is opened.
    /// `alpha` is found by bisection so the result fits the budget, then any
    /// lines left over are spent opening the newest windows.
    pub fn cover(&self, now_hour: i64, budget: usize) -> Vec<Node> {
        self.cover_ruled(now_hour, budget)
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    }

    /// [`Tree::cover`], with the rule that put each node in the view: the
    /// age rule, or the leftover budget spent on the present.
    pub fn cover_ruled(&self, now_hour: i64, budget: usize) -> Vec<(Node, Rule)> {
        if self.is_empty() || budget == 0 {
            return Vec::new();
        }
        let (mut lo, mut hi) = (0.0f64, 1.0e9f64);
        let mut best = self.cover_at(now_hour, hi);
        for _ in 0..80 {
            let mid = (lo + hi) / 2.0;
            let c = self.cover_at(now_hour, mid);
            if c.len() > budget {
                lo = mid;
            } else {
                hi = mid;
                best = c;
            }
        }
        let mut best: Vec<(Node, Rule)> = best.into_iter().map(|n| (n, Rule::Age)).collect();
        // Spend what is left on the present, where detail is worth most.
        loop {
            let open = best.iter().rposition(|(n, _)| matches!(n, Node::Window(_)));
            let Some(i) = open else { break };
            let Node::Window(w) = best[i].0.clone() else {
                break;
            };
            let kids = self.children(w);
            if best.len() - 1 + kids.len() > budget {
                break;
            }
            best.splice(i..=i, kids.into_iter().map(|k| (k, Rule::Present)));
        }
        best
    }

    fn cover_at(&self, now_hour: i64, alpha: f64) -> Vec<Node> {
        let mut out = Vec::new();
        let mut stack: Vec<Window> = self.roots().into_iter().rev().collect();
        while let Some(w) = stack.pop() {
            let Some(rep) = self.representative(w) else {
                continue;
            };
            let Node::Window(rw) = rep else {
                out.push(rep);
                continue;
            };
            let age = (now_hour - rw.start()).max(1) as f64;
            let keep = rw.end() <= now_hour && (rw.size() as f64) <= alpha * age;
            if keep {
                out.push(Node::Window(rw));
            } else if let Some((a, b)) = rw.halves() {
                stack.push(b);
                stack.push(a);
            } else {
                out.extend(
                    self.memories_in_hour(rw.start())
                        .iter()
                        .cloned()
                        .map(Node::Memory),
                );
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(hours: &[i64]) -> Tree {
        Tree::new(hours.iter().enumerate().map(|(i, &h)| (format!("m{i}"), h)))
    }

    #[test]
    fn hours_round_trip_through_timestamps() {
        assert_eq!(hour_of("2020-01-01T00:59:59Z"), Some(0));
        assert_eq!(hour_of("2020-01-01T01:00:00Z"), Some(1));
        assert_eq!(hour_to_rfc3339(1), "2020-01-01T01:00:00Z");
        let h = hour_of("2023-11-09T14:52:21.061177Z").unwrap();
        assert_eq!(hour_to_rfc3339(h), "2023-11-09T14:00:00Z");
    }

    #[test]
    fn a_window_with_one_nonempty_half_is_that_half() {
        let t = tree(&[0, 1, 2]); // hours 0,1 in w1-0; hour 2 alone in w1-1
        assert_eq!(
            t.representative(Window { level: 1, index: 0 }),
            Some(Node::Window(Window { level: 1, index: 0 }))
        );
        assert_eq!(
            t.representative(Window { level: 1, index: 1 }),
            Some(Node::Memory("m2".into()))
        );
        // w3-0 covers hours 0..8; its right half (4..8) is empty, so it IS w2-0
        assert_eq!(
            t.representative(Window { level: 3, index: 0 }),
            Some(Node::Window(Window { level: 2, index: 0 }))
        );
    }

    #[test]
    fn an_hour_with_several_memories_is_a_node() {
        let t = tree(&[5, 5, 5]);
        let w = Window { level: 0, index: 5 };
        assert_eq!(t.representative(w), Some(Node::Window(w)));
        assert_eq!(t.children(w).len(), 3);
    }

    #[test]
    fn a_backdated_memory_only_touches_the_windows_that_contain_it() {
        let now = 10_000;
        let before = tree(&[100, 101, 5000, 5001, 9000]);
        let mut hours = vec![100, 101, 5000, 5001, 9000];
        hours.push(102); // an import, long after the fact
        let after = tree(&hours);
        let a: std::collections::BTreeSet<_> = before.summary_windows(now).into_iter().collect();
        let b: std::collections::BTreeSet<_> = after.summary_windows(now).into_iter().collect();
        // Every summary window that changed contains hour 102.
        for w in a.symmetric_difference(&b) {
            assert!(
                w.start() <= 102 && 102 < w.end(),
                "{w:?} changed but does not contain hour 102"
            );
        }
        // And the far-away windows are untouched.
        assert!(
            b.contains(&Window {
                level: 0,
                index: 5000
            }) == a.contains(&Window {
                level: 0,
                index: 5000
            })
        );
    }

    #[test]
    fn open_windows_are_not_summarized() {
        let t = tree(&[10, 11]);
        assert!(
            t.summary_windows(11).is_empty(),
            "hour 11 has not ended at hour 11"
        );
        assert_eq!(t.summary_windows(12), vec![Window { level: 1, index: 5 }]);
    }

    #[test]
    fn cover_fits_the_budget_and_keeps_the_present_verbatim() {
        // 2,000 memories, one every 3 hours.
        let hours: Vec<i64> = (0..2000).map(|i| i * 3).collect();
        let t = tree(&hours);
        let now = 6000;
        let c = t.cover(now, 96);
        assert!(c.len() <= 96 && c.len() > 80, "{}", c.len());
        // The newest memory is shown as itself.
        assert_eq!(c.last(), Some(&Node::Memory("m1999".into())));
        // Every memory is covered exactly once.
        let mut covered = 0usize;
        for n in &c {
            covered += match n {
                Node::Memory(_) => 1,
                Node::Window(w) => t.memories_in(*w).len(),
            };
        }
        assert_eq!(covered, 2000);
    }

    #[test]
    fn a_small_history_is_shown_whole() {
        let t = tree(&[1, 50, 900]);
        let c = t.cover(1000, 96);
        assert_eq!(
            c,
            vec![
                Node::Memory("m0".into()),
                Node::Memory("m1".into()),
                Node::Memory("m2".into())
            ]
        );
    }

    #[test]
    fn window_ids_round_trip() {
        let w = Window {
            level: 7,
            index: 312,
        };
        assert_eq!(Window::parse(&w.id()), Some(w));
        assert_eq!(Window::parse("x1-2"), None);
    }
}
