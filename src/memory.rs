//! OptMem, exactly: the memory's tree shape, its merge prompts and its wake
//! cover, ported line for line from OptMem's `memo`
//! (github.com/VictorTaelin/OptMem, commit 1fb164c). goodlux, 2026-10-13:
//! implement OptMem's algorithm as a graph in ravel — not something similar,
//! not something clever.
//!
//! - The LOG is the soul's notes in the order they were noted. A note's
//!   identity in the tree is its position, `#0, #1, …`.
//! - A BLOCK is an aligned power-of-two range of positions, `[lo,hi)`, merged
//!   into one line. Block `[lo,hi)` is the merge of `[lo,mid)` and `[mid,hi)`:
//!   neighbours by position, never by topic or time.
//! - A block comes due once the log holds `hi` notes. Due blocks are built
//!   smallest size first, left to right.
//! - A block of at most [`RAW_MAX`] notes is merged from the notes
//!   themselves; a bigger one from its two halves' lines.
//! - Wake shows `cover(T, 96)`: the whole log in at most 96 lines, recent
//!   notes verbatim, older ones through blocks.
//!
//! Pure: no storage, no model. `memory_log` stores; `memory_relive` asks.
//! The conformance test in `tests/optmem_conformance.rs` runs OptMem's own
//! `memo` beside this module and requires the same bytes.

/// How many lines wake prints (`WAKE_LINES`).
pub const WAKE_LINES: usize = 96;
/// The longest one memory may be, in bytes (`ENTRY_CHARS`).
pub const ENTRY_CHARS: usize = 280;
/// Blocks up to this many memories merge from the raw log (`RAW_MAX`).
pub const RAW_MAX: usize = 16;

/// A block `[lo, hi)` of log positions.
pub type Block = (usize, usize);

/// `_cover`: tile `[0,T)` with aligned power-of-two blocks; keep a block
/// whole iff its size is at most `alpha` times its age.
fn cover_at(t: usize, alpha: f64) -> Vec<Block> {
    let mut root = 1usize;
    while root < t {
        root *= 2;
    }
    let mut out = Vec::new();
    let mut stack = vec![(0usize, root)];
    while let Some((lo, hi)) = stack.pop() {
        if lo >= t {
            continue;
        }
        let size = hi - lo;
        if size > 1 && (hi > t || (size as f64) > alpha * ((t - lo) as f64)) {
            let mid = (lo + hi) / 2;
            stack.push((mid, hi));
            stack.push((lo, mid));
        } else {
            out.push((lo, hi));
        }
    }
    out.sort();
    out
}

/// `cover`: the blocks wake prints, at most `budget` of them, finest near T.
pub fn cover(t: usize, budget: usize) -> Vec<Block> {
    if t == 0 {
        return Vec::new();
    }
    if t <= budget {
        return (0..t).map(|i| (i, i + 1)).collect();
    }
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..60 {
        let mid = (lo + hi) / 2.0;
        if cover_at(t, mid).len() > budget {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let mut out = cover_at(t, hi);
    // Block sizes jump in powers of two, so alpha alone can undershoot the
    // budget. Spend what is left on the present, where detail is worth most.
    while out.len() < budget {
        let Some(i) = out.iter().rposition(|b| b.1 - b.0 > 1) else {
            break;
        };
        let (l, h) = out[i];
        let mid = (l + h) / 2;
        out.splice(i..=i, [(l, mid), (mid, h)]);
    }
    out
}

/// `pending`: blocks that can be built and have not been, smallest first.
/// `have(size)` is how many blocks of that size are built, as a dense prefix
/// from position 0 (OptMem's level-file length).
pub fn pending(t: usize, have: impl Fn(usize) -> usize, limit: Option<usize>) -> Vec<Block> {
    let mut todo = Vec::new();
    let mut size = 2;
    while size <= t {
        for k in have(size)..t / size {
            todo.push((k * size, (k + 1) * size));
            if limit.is_some_and(|n| todo.len() >= n) {
                return todo;
            }
        }
        size *= 2;
    }
    todo
}

/// `pending_count`: how many blocks `pending` would list.
pub fn pending_count(t: usize, have: impl Fn(usize) -> usize) -> usize {
    let mut n = 0;
    let mut size = 2;
    while size <= t {
        n += (t / size).saturating_sub(have(size));
        size *= 2;
    }
    n
}

/// A block's id as OptMem prints it: inclusive at both ends, `lo-(hi-1)`.
pub fn block_id((lo, hi): Block) -> String {
    format!("{}-{}", lo, hi - 1)
}

/// `block_id` in reverse: a real block (aligned power of two, size ≥ 2).
pub fn parse_block(s: &str) -> Option<Block> {
    let (a, b) = s.trim_start_matches('#').split_once('-')?;
    let lo: usize = a.parse().ok()?;
    let hi = b.parse::<usize>().ok()? + 1;
    let n = hi.checked_sub(lo)?;
    (n >= 2 && n.is_power_of_two() && lo % n == 0).then_some((lo, hi))
}

/// `check`: one line, not empty, at most [`ENTRY_CHARS`] bytes. The error is
/// OptMem's message, word for word: it is what the writer is told.
pub fn check(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Empty. A memory is one line of text.".into());
    }
    if text.contains('\n') || text.contains('\r') {
        return Err(format!(
            "{} lines. A memory is one line: merge them, or note them separately.",
            text.matches('\n').count() + 1
        ));
    }
    let n = text.len();
    if n > ENTRY_CHARS {
        return Err(format!(
            "Too long: {n} bytes, limit {ENTRY_CHARS}. Accented characters cost 2 bytes. Compress it further."
        ));
    }
    Ok(text.to_string())
}

/// One raw log line as OptMem prints it: `#i date text`.
pub fn note_line(i: usize, date: &str, text: &str) -> String {
    format!("#{i} {date} {text}")
}

/// One block line as OptMem prints it: `#lo-hi text`.
pub fn block_line(b: Block, text: &str) -> String {
    format!("#{} {text}", block_id(b))
}

/// What a merge is shown: OptMem's `nap_prompt` body. A block of at most
/// [`RAW_MAX`] notes shows its notes; a bigger one its two halves' lines.
/// `note(i)` is `(date, text)` of note i; `line(b)` is block b's text.
pub fn nap_body(
    (lo, hi): Block,
    note: impl Fn(usize) -> (String, String),
    line: impl Fn(Block) -> Option<String>,
) -> Result<String, String> {
    if hi - lo <= RAW_MAX {
        return Ok((lo..hi)
            .map(|i| {
                let (d, t) = note(i);
                format!("  {}", note_line(i, &d, &t))
            })
            .collect::<Vec<_>>()
            .join("\n"));
    }
    let mid = (lo + hi) / 2;
    let mut halves = Vec::new();
    for b in [(lo, mid), (mid, hi)] {
        let s = line(b).ok_or_else(|| format!("The summary of #{} is blank.", block_id(b)))?;
        halves.push(format!("  {}", block_line(b, &s)));
    }
    Ok(halves.join("\n"))
}

/// OptMem's `nap_prompt`, word for word, up to its last line. OptMem ends
/// with `Run: memo nap lo-hi "<your line>"` — how its agent hands the line
/// back. Ravel's writer hands it back as its answer instead, so that line is
/// the one difference.
pub fn nap_prompt(b: Block, body: &str, left: usize) -> String {
    let tail = match left {
        0 => String::new(),
        1 => "\n1 compression remains after this one.".to_string(),
        n => format!("\n{n} compressions remain after this one."),
    };
    format!(
        "Compress memories #{} into one line of at most {ENTRY_CHARS} bytes.\nKeep what has lasting effect, drop what does not. Invent nothing.\n\n{body}\n{tail}\n",
        block_id(b)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_log_is_shown_whole() {
        assert_eq!(cover(3, 96), vec![(0, 1), (1, 2), (2, 3)]);
        assert!(cover(0, 96).is_empty());
    }

    #[test]
    fn cover_fits_the_budget_tiles_the_log_and_keeps_the_present_raw() {
        for t in [97, 100, 300, 1000, 4097, 50_000] {
            let c = cover(t, 96);
            assert!(c.len() <= 96, "{t}: {}", c.len());
            // contiguous tiling of [0,T), every block aligned power of two
            let mut at = 0;
            for &(lo, hi) in &c {
                assert_eq!(lo, at);
                let n = hi - lo;
                assert!(n.is_power_of_two() && lo % n == 0);
                at = hi;
            }
            assert_eq!(at, t);
            assert_eq!(c.last(), Some(&(t - 1, t)));
        }
    }

    #[test]
    fn blocks_come_due_smallest_first() {
        // 8 notes, nothing built: 4 pairs, 2 quads, 1 octet.
        let p = pending(8, |_| 0, None);
        assert_eq!(
            p,
            vec![(0, 2), (2, 4), (4, 6), (6, 8), (0, 4), (4, 8), (0, 8)]
        );
        assert_eq!(pending_count(8, |_| 0), 7);
        // pairs built: the quads are next
        let p = pending(8, |s| if s == 2 { 4 } else { 0 }, Some(1));
        assert_eq!(p, vec![(0, 4)]);
        // 5 notes: only complete blocks
        assert_eq!(pending(5, |_| 0, None), vec![(0, 2), (2, 4), (0, 4)]);
    }

    #[test]
    fn block_ids_round_trip_and_refuse_non_blocks() {
        assert_eq!(block_id((16, 32)), "16-31");
        assert_eq!(parse_block("16-31"), Some((16, 32)));
        assert_eq!(parse_block("#0-1"), Some((0, 2)));
        assert_eq!(parse_block("4-5"), Some((4, 6)));
        assert_eq!(parse_block("5-6"), None);
        assert_eq!(parse_block("0-2"), None);
        assert_eq!(parse_block("3-3"), None);
    }

    #[test]
    fn check_says_what_optmem_says() {
        assert_eq!(check("  ok  "), Ok("ok".into()));
        assert_eq!(
            check(&"é".repeat(141)),
            Err("Too long: 282 bytes, limit 280. Accented characters cost 2 bytes. Compress it further.".into())
        );
        assert!(check("a\nb").unwrap_err().starts_with("2 lines."));
        assert!(check(" ").is_err());
    }

    #[test]
    fn small_blocks_merge_from_notes_big_ones_from_halves() {
        let note = |i: usize| ("2026-01-01".to_string(), format!("n{i}"));
        let body = nap_body((0, 4), note, |_| None).unwrap();
        assert_eq!(
            body,
            "  #0 2026-01-01 n0\n  #1 2026-01-01 n1\n  #2 2026-01-01 n2\n  #3 2026-01-01 n3"
        );
        let body = nap_body((0, 32), note, |b| Some(format!("s{}", block_id(b)))).unwrap();
        assert_eq!(body, "  #0-15 s0-15\n  #16-31 s16-31");
        assert!(nap_body((0, 32), note, |_| None).is_err());
        assert_eq!(
            nap_prompt((0, 2), "  #0 d a\n  #1 d b", 2),
            "Compress memories #0-1 into one line of at most 280 bytes.\nKeep what has lasting effect, drop what does not. Invent nothing.\n\n  #0 d a\n  #1 d b\n\n2 compressions remain after this one.\n"
        );
    }
}
