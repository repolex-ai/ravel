//! Conformance with OptMem's own code: run OptMem's `memo` and ravel's port
//! side by side on the same notes, the way an agent uses OptMem — one note,
//! then every merge it asks for — and require the same bytes for every
//! merge prompt (which block, in which order, shown what) and for the wake
//! view at every log length.
//!
//! Needs OptMem's `memo` (github.com/VictorTaelin/OptMem, commit 1fb164c)
//! and python3, so it is opt-in and fails loudly without them:
//!
//!   OPTMEM_MEMO=~/repos/VictorTaelin/OptMem/memo \
//!     cargo test --release --test optmem_conformance -- --ignored

use ravel::memory::{self, Block};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

struct Memo {
    script: PathBuf,
    dir: PathBuf,
}

impl Memo {
    fn run(&self, args: &[&str]) -> String {
        let out = Command::new("python3")
            .arg("-I")
            .arg(&self.script)
            .args(args)
            .env("MEMORY_DIR", &self.dir)
            .output()
            .expect("python3 runs");
        assert!(
            out.status.success(),
            "memo {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
}

/// A note text that exercises what can differ: multi-byte characters,
/// quotes, a `#`, lengths up to the 280-byte limit.
fn note_text(i: usize) -> String {
    let base = match i % 5 {
        0 => format!("note {i}: plain"),
        1 => format!("note {i}: Ünïcödé — “quoted” and 'single' #tag"),
        2 => format!("note {i}: {}", "x".repeat(i % 250)),
        3 => format!("note {i}: {}", "é".repeat((i % 130) + 1)),
        _ => format!("note {i}: ends with spaces"),
    };
    let mut t = base;
    while t.len() > memory::ENTRY_CHARS {
        t.pop();
    }
    t.trim().to_string()
}

fn date(i: usize) -> String {
    format!("2026-{:02}-{:02}", 1 + (i / 28) % 12, 1 + i % 28)
}

#[test]
#[ignore = "needs OptMem's memo: set OPTMEM_MEMO and run with --ignored"]
fn ravel_matches_optmem_byte_for_byte() {
    let script = PathBuf::from(
        std::env::var("OPTMEM_MEMO")
            .expect("set OPTMEM_MEMO to OptMem's `memo` script (commit 1fb164c)"),
    );
    assert!(script.is_file(), "{} is not a file", script.display());
    let base = std::env::temp_dir().join(format!("optmem-conformance-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let memo = Memo {
        script,
        dir: base.join("mem"),
    };
    memo.run(&["init"]);

    const N: usize = 300;
    let mut notes: Vec<(String, String)> = Vec::new();
    let mut tree: HashMap<Block, String> = HashMap::new();
    let mut prompts = 0;
    for i in 0..N {
        // 1. One note, through OptMem's import (it keeps the date we give).
        let (d, t) = (date(i), note_text(i));
        let f = base.join("one.txt");
        std::fs::write(&f, format!("{d} {t}\n")).unwrap();
        memo.run(&["import", f.to_str().unwrap()]);
        notes.push((d, t));

        // 2. Every merge OptMem asks for, compared with ravel's.
        loop {
            let have = |size: usize| {
                (0..)
                    .take_while(|k| tree.contains_key(&(k * size, (k + 1) * size)))
                    .count()
            };
            let todo = memory::pending(notes.len(), have, None);
            let out = memo.run(&["nap"]);
            let Some(&b) = todo.first() else {
                assert_eq!(out.trim(), "Nothing left to compress.", "after note {i}");
                break;
            };
            let body = memory::nap_body(
                b,
                |k| notes[k].clone(),
                |c| tree.get(&c).cloned(),
            )
            .unwrap();
            let ours = memory::nap_prompt(b, &body, todo.len() - 1);
            let theirs = out
                .rsplit_once("Run: ")
                .map(|(head, _)| head)
                .unwrap_or_else(|| panic!("memo nap printed no Run line: {out}"));
            assert_eq!(ours, theirs, "merge prompt for #{} after note {i}", memory::block_id(b));
            prompts += 1;
            let line = format!("S{} {}", memory::block_id(b), "ü".repeat(b.0 % 7));
            memo.run(&["nap", &memory::block_id(b), &line]);
            // Both sides keep what OptMem's check() returns: the line trimmed.
            tree.insert(b, memory::check(&line).unwrap());
        }

        // 3. The wake view at this length.
        let t = notes.len();
        let mut theirs: Vec<String> = Vec::new();
        for part in 1.. {
            let out = memo.run(&["wake", &part.to_string(), &t.to_string()]);
            let done = out.contains("You are awake.");
            theirs.extend(out.lines().filter(|l| l.starts_with('#')).map(str::to_string));
            if done {
                break;
            }
        }
        let ours: Vec<String> = memory::cover(t, memory::WAKE_LINES)
            .into_iter()
            .map(|b| {
                if b.1 - b.0 == 1 {
                    memory::note_line(b.0, &notes[b.0].0, &notes[b.0].1)
                } else {
                    memory::block_line(b, &tree[&b])
                }
            })
            .collect();
        assert_eq!(ours, theirs, "wake at T={t}");
    }
    assert!(prompts > N - 10, "only {prompts} merges compared");
    let _ = std::fs::remove_dir_all(&base);
}

/// The wake cover for long logs, against OptMem's own `cover` function:
/// every T up to 5,000, then sparse up to a million.
#[test]
#[ignore = "needs OptMem's memo: set OPTMEM_MEMO and run with --ignored"]
fn cover_matches_optmem_for_long_logs() {
    let script = std::env::var("OPTMEM_MEMO")
        .expect("set OPTMEM_MEMO to OptMem's `memo` script (commit 1fb164c)");
    let ts: Vec<usize> = (1..=5000)
        .chain((5001..1_000_000).step_by(7919))
        .collect();
    let py = format!(
        "import runpy,sys\ng=runpy.run_path({script:?})\nfor t in map(int,sys.stdin.read().split()):\n print(t,' '.join('%d-%d'%b for b in g['cover'](t,96)))\n"
    );
    let input: String = ts.iter().map(|t| format!("{t}\n")).collect();
    let mut child = Command::new("python3")
        .args(["-I", "-c", &py])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("python3 runs");
    use std::io::Write;
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let theirs = String::from_utf8(out.stdout).unwrap();
    let mut n = 0;
    for line in theirs.lines() {
        let (t, blocks) = line.split_once(' ').unwrap();
        let t: usize = t.parse().unwrap();
        let ours: Vec<String> = memory::cover(t, memory::WAKE_LINES)
            .into_iter()
            .map(|(lo, hi)| format!("{lo}-{hi}"))
            .collect();
        assert_eq!(ours.join(" "), blocks, "cover({t}, 96)");
        n += 1;
    }
    assert_eq!(n, ts.len());
}
