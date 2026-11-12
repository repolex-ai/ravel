//! ravel — the command line. A thin client of raveld; it never opens a store.
//! When raveld is not running, any command here starts it first
//! (goodlux, 2026-09-22). Every command is explained in [`HELP`], which
//! `ravel --help` prints: what it does, what it reads, and when to use it
//! (goodlux, 2026-11-12). No flags.

use anyhow::{anyhow, Context, Result};
use ravel::client::{ensure_running, Client};
use ravel::daemon::config::DaemonConfig;
use serde_json::Value;

/// `ravel --help`. Each command says what it does, what it reads, and when
/// to reach for it: a list of names alone told a reader nothing (goodlux,
/// 2026-11-12).
const HELP: &str = "\
ravel {version}: keeps every conversation a soul has, in every harness, as a
graph. Talks to raveld, the background daemon, and starts it if it is not
running.

USAGE:

  ravel
      What raveld is doing: whether it runs, which souls it serves, and each
      one's backup and memory. The first look when something seems off.

  ravel status [<soul>]
      One soul in detail: its health check (exit 1 = needs attention), what
      its store holds, and where its memory stands.

  ravel souls
      Every soul this machine's raveld serves, with its id and folder.

  ravel sync [<soul> [<sessions-dir>]]
      Back up and read in new conversations now. raveld does this every 30
      seconds on its own; run it when you cannot wait. <sessions-dir> reads
      session files from another folder into this soul.

  ravel import [<soul>] <export-dir>
      Bring in a claude.ai data export. Safe to repeat; a newer export adds
      to what is there.

  ravel biography [<soul>]
      The soul's life so far: read it at wake. One section per memory
      architecture ravel runs; today that is optmem alone, the story the
      soul wrote for itself. About 25,000 characters.

  ravel optmem [<soul>]
      What optmem is, where this soul's stands, and its commands. Short.
      optmem is ravel's adaptation of OptMem, the memory architecture by
      Victor Taelin (https://github.com/VictorTaelin/OptMem).

  ravel optmem wake [<soul>]
      optmem's whole index, about 96 lines and 25,000 characters, into your
      context at once: the optmem section of `ravel biography`. Only to
      get your bearings again, for example after a compaction lost what you
      read at wake. If you know what you are looking for, use recall or open.

  ravel optmem open [<soul>] <label>
      Drill into one line of the index. A merged line (#0-1023) shows the two
      lines it was made from; a note (#12) shows the note and the
      conversation turns it was written from, each cut at 600 characters.

  ravel optmem recall [<soul>] <words>
      Where words appear across the soul's life, matched whole and ignoring
      case: how many conversation turns said them and when (every harness,
      from the graph), how many optmem notes recorded them and in which
      months, the highest merged line that still names them, and the five
      latest notes, with labels to open.

  ravel optmem check [<soul>]
      Test optmem end to end: every line within its limits, every note citing
      real turns, the graph matching the record. Exit 1 on any failure.
      Maintenance, not reading.

  ravel query [<soul>] \"<sparql>\"
      Ask the transcript graph anything, in SPARQL. One row per line.

  ravel query help
      What can be queried and where it is stored: every class and property,
      the graph layout a query must name, and three queries that work as
      written. About 4,000 characters. Read it before a first query.

<soul> = which soul: its six-character id (from `ravel souls`), or a path
inside its folder. Leave it off for the soul whose folder you are in.

Config: {config}
";

fn help() -> String {
    HELP.replace("{version}", env!("CARGO_PKG_VERSION")).replace(
        "{config}",
        &ravel::daemon::config::config_path().display().to_string(),
    )
}

/// Asked for: the help on stdout. Misused: on stderr, exit 2.
fn usage() -> ! {
    eprint!("{}", help());
    std::process::exit(2);
}

/// What optmem's index starts with, wherever it is shown.
const OPTMEM_IS: &str = "optmem: ravel's adaptation of OptMem, the memory architecture by Victor Taelin (https://github.com/VictorTaelin/OptMem)";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["--version"] | ["-V"] => {
            println!("ravel {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        ["--help"] | ["-h"] | ["help"] => {
            print!("{}", help());
            return Ok(());
        }
        // `memory` became `biography` and `optmem` in 0.9.4-0.9.5 (goodlux,
        // 2026-11-12: the word was too general for one of many memories).
        ["memory", ..] => {
            eprintln!("ravel memory is gone: the whole story is `ravel biography`, and its tools are `ravel optmem` (open, recall, check, wake). `ravel --help` explains each.");
            std::process::exit(2);
        }
        // ravel has no flags; `ravel optmem --help` must not be read as a
        // soul id (w3bl0rd, 2026-10-05).
        args if args.iter().any(|x| x.starts_with('-')) => usage(),
        _ => {}
    }
    match a.as_slice() {
        ["query"] => {
            println!("ravel query [<soul>] \"<sparql>\": ask the transcript graph anything, in SPARQL.\n`ravel query help` shows what can be queried, where it is stored, and three queries that work as written.");
            return Ok(());
        }
        ["query", "help"] => {
            print!("{}", query_help()?);
            return Ok(());
        }
        _ => {}
    }
    let cfg = DaemonConfig::load()?;
    let client = ensure_running(&cfg)?;
    match a.as_slice() {
        [] => status(&cfg, &client),
        ["status"] => soul_status(&client, &resolve(&client, None)?),
        ["status", s] => soul_status(&client, &resolve(&client, Some(s))?),
        ["souls"] => souls(&client),
        ["sync"] => sync(&client, &resolve(&client, None)?, None),
        ["sync", s] => sync(&client, &resolve(&client, Some(s))?, None),
        ["sync", s, dir] => sync(&client, &resolve(&client, Some(s))?, Some(dir)),
        ["import", dir] => import(&client, &resolve(&client, None)?, dir),
        ["import", s, dir] => import(&client, &resolve(&client, Some(s))?, dir),
        ["biography"] => biography(&client, &resolve(&client, None)?),
        ["biography", s] => biography(&client, &resolve(&client, Some(s))?),
        ["optmem"] => optmem(&client, &resolve(&client, None)?),
        ["optmem", "wake"] => optmem_wake(&client, &resolve(&client, None)?),
        ["optmem", "wake", s] => optmem_wake(&client, &resolve(&client, Some(s))?),
        ["optmem", "open", n] => optmem_open(&client, &resolve(&client, None)?, n),
        ["optmem", "open", s, n] => optmem_open(&client, &resolve(&client, Some(s))?, n),
        ["optmem", "recall", words @ ..] if !words.is_empty() => {
            // A first word that names a soul is the soul; the rest are the
            // words. A single word is always the words.
            match words {
                [s, rest @ ..] if !rest.is_empty() && names_soul(&client, s) => {
                    optmem_recall(&client, &resolve(&client, Some(s))?, &rest.join(" "))
                }
                _ => optmem_recall(&client, &resolve(&client, None)?, &words.join(" ")),
            }
        }
        ["optmem", "check"] => optmem_check(&client, &resolve(&client, None)?),
        ["optmem", "check", s] => optmem_check(&client, &resolve(&client, Some(s))?),
        ["optmem", s] if !["wake", "open", "recall", "check"].contains(s) => {
            optmem(&client, &resolve(&client, Some(s))?)
        }
        ["query", q] => query(&client, &resolve(&client, None)?, q),
        ["query", s, q] => query(&client, &resolve(&client, Some(s))?, q),
        _ => usage(),
    }
}

/// Which soul: the id given, the soul whose repo contains the path given, or
/// the soul whose repo contains the current directory.
fn resolve(client: &Client, given: Option<&str>) -> Result<String> {
    let souls = client.get("/souls")?;
    let souls = souls.as_array().cloned().unwrap_or_default();
    if let Some(g) = given {
        if souls.iter().any(|s| s["id"] == g) {
            return Ok(g.to_string());
        }
        let p = std::path::Path::new(g)
            .canonicalize()
            .with_context(|| format!("{g:?} is neither a registered soul id nor a path"))?;
        return by_path(&souls, &p).ok_or_else(|| not_registered(&p, &souls));
    }
    let here = std::env::current_dir()?.canonicalize()?;
    by_path(&souls, &here).ok_or_else(|| not_registered(&here, &souls))
}

/// Whether `s` names a soul raveld serves, by id or by a path inside it.
fn names_soul(client: &Client, s: &str) -> bool {
    let Ok(souls) = client.get("/souls") else {
        return false;
    };
    let souls = souls.as_array().cloned().unwrap_or_default();
    souls.iter().any(|x| x["id"] == s)
        || std::path::Path::new(s)
            .canonicalize()
            .is_ok_and(|p| by_path(&souls, &p).is_some())
}

/// A soul's name: its folder's.
fn soul_name(client: &Client, id: &str) -> String {
    client
        .get("/souls")
        .ok()
        .and_then(|v| {
            v.as_array()?.iter().find(|s| s["id"] == id).and_then(|s| {
                std::path::Path::new(s["path"].as_str()?)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
        })
        .unwrap_or_else(|| id.to_string())
}

fn by_path(souls: &[Value], p: &std::path::Path) -> Option<String> {
    souls
        .iter()
        .find(|s| {
            s["path"]
                .as_str()
                .map(|sp| p.starts_with(sp))
                .unwrap_or(false)
        })
        .and_then(|s| s["id"].as_str().map(str::to_string))
}

fn not_registered(p: &std::path::Path, souls: &[Value]) -> anyhow::Error {
    let list = if souls.is_empty() {
        "    (none)".to_string()
    } else {
        souls
            .iter()
            .map(|s| {
                format!(
                    "    {}  {}",
                    s["id"].as_str().unwrap_or("?"),
                    s["path"].as_str().unwrap_or("?")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let eg = souls
        .first()
        .and_then(|s| s["id"].as_str())
        .unwrap_or("<id>");
    anyhow!(
        "{} is not inside a registered soul.\n  Run the command inside a soul repo, or name the soul right after the command, by id or path:\n    ravel status {eg}\n    ravel import {eg} ~/Downloads/<export-folder>\n  Registered souls:\n{list}\n  To add this repo as a soul: put it under souls: in {} and run `raveld restart`.",
        p.display(),
        ravel::daemon::config::config_path().display()
    )
}

fn status(cfg: &DaemonConfig, client: &Client) -> Result<()> {
    let h = client.health()?;
    let up = h["uptime_secs"].as_u64().unwrap_or(0);
    println!(
        "raveld is RUNNING — pid {}, up {}h {:02}m {:02}s, version {}, {} on {}",
        h["pid"].as_u64().unwrap_or(0),
        up / 3600,
        (up % 3600) / 60,
        up % 60,
        h["version"].as_str().unwrap_or("?"),
        h["passes"].as_u64().unwrap_or(0),
        cfg.base_url()
    );
    if !h["config_present"].as_bool().unwrap_or(false) {
        println!(
            "  NO CONFIG at {} — no souls are being synced. Create it with a souls: list.",
            h["config"].as_str().unwrap_or("?")
        );
    }
    for p in h["config_problems"].as_array().unwrap_or(&Vec::new()) {
        println!("  CONFIG PROBLEM: {}", p.as_str().unwrap_or("?"));
    }
    println!("  memory: {}", h["memory"].as_str().unwrap_or("?"));
    souls(client)
}

fn souls(client: &Client) -> Result<()> {
    let souls = client.get("/souls")?;
    let rows = souls.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("  (no souls registered)");
    }
    for s in rows {
        let last = &s["last"];
        println!(
            "  {}  {}\n      last pass {}  {}",
            s["id"].as_str().unwrap_or("?"),
            s["path"].as_str().unwrap_or("?"),
            s["last_sync"].as_str().unwrap_or("never"),
            match s["last_error"].as_str() {
                Some(e) => format!("ERROR: {e}"),
                None if last.is_null() => String::new(),
                None => format!(
                    "mirrored {} (unchanged {}), ingested {} session(s) ({} skipped), {} turns; agy {} conversation(s)",
                    last["mirrored"], last["unchanged"], last["sessions"], last["skipped"], last["turns"], last["agy_sessions"]
                ),
            }
        );
        if let Some(m) = memory_line(&s["memory"]) {
            println!("      memory {m}");
        }
    }
    Ok(())
}

/// An RFC 3339 instant in this machine's local time: `2024-03-07 06:22`.
/// Memories are stored in UTC; shown in UTC, a late-evening event lands on
/// the next day (4m41th34's "conscious on 08-04" was 08-03 local).
fn short(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| ts.get(..16).unwrap_or(ts).replace('T', " "))
}

/// Wake lines as OptMem prints them: `#i date text` and `#lo-hi text`.
fn print_lines(lines: &[Value]) {
    for l in lines {
        let label = l["label"].as_str().unwrap_or("");
        let text = l["text"].as_str().unwrap_or("");
        let date = l["date"].as_str().unwrap_or("");
        if l["level"].as_u64().unwrap_or(0) == 0 {
            println!("{label} {date} {text}");
        } else {
            println!(
                "{label} {} {text}",
                span(date, l["date_to"].as_str().unwrap_or(""))
            );
        }
    }
}

/// A block's date span as the wake shows it: `2026-04-03..04-08`, with the
/// end's year left off when it repeats the start's.
fn span(from: &str, to: &str) -> String {
    if from.is_empty() || to.is_empty() || from == to {
        return from.to_string();
    }
    let end = if from.get(..4) == to.get(..4) {
        to.get(5..).unwrap_or(to)
    } else {
        to
    };
    format!("{from}..{end}")
}

/// `ravel biography`: the soul's life so far, one section per memory
/// architecture. Today optmem's index, which the soul wrote for itself, is
/// the only section; a statistics section, or another architecture's, would
/// follow it. "biography", not "autobiography": not every section will be
/// self-written (goodlux, 2026-11-12).
fn biography(client: &Client, id: &str) -> Result<()> {
    println!("# {} ({id}): biography\n", soul_name(client, id));
    optmem_wake(client, id)
}

/// `ravel optmem wake`: optmem's whole index, as its section of the
/// biography.
fn optmem_wake(client: &Client, id: &str) -> Result<()> {
    let v = client.get(&format!("/souls/{id}/memory"))?;
    println!("## {OPTMEM_IS}");
    let lines = v["lines"].as_array().cloned().unwrap_or_default();
    if lines.is_empty() {
        println!("No notes yet.");
        return Ok(());
    }
    println!(
        "{} notes, {} merges. Open a line by its label: `ravel optmem open <label>`.",
        v["memories"], v["summaries"]
    );
    // A merge reads like a statement of fact, but it is how things stood over
    // its dates (selkie, 2026-11-01: an April rule had since changed).
    println!("Oldest first. Each line is how things stood over its dates; where two disagree, the later one is current.");
    print_lines(&lines);
    Ok(())
}

/// `ravel optmem`: a short page, never the index. What optmem is, where this
/// soul's stands, and the commands, so a bare `ravel optmem` cannot pour
/// 25,000 characters into a context (goodlux, 2026-11-12).
fn optmem(client: &Client, id: &str) -> Result<()> {
    let v = client.get(&format!("/souls/{id}/memory"))?;
    println!("{OPTMEM_IS}.");
    println!("raveld writes it every 10 minutes from new conversation turns.\n");
    println!(
        "{} ({id}): {} notes, {} merges.",
        soul_name(client, id),
        v["memories"],
        v["summaries"]
    );
    let row = client.get("/souls").ok().and_then(|s| {
        s.as_array()?
            .iter()
            .find(|x| x["id"] == id)
            .and_then(|x| memory_line(&x["memory"]))
    });
    if let Some(l) = row {
        println!("Last pass {l}");
    }
    println!(
        "\n  ravel optmem wake [<soul>]            the whole index, about 25,000 characters:\n\
         \x20                                       only to get your bearings again\n\
         \x20 ravel optmem open [<soul>] <label>    drill into one line\n\
         \x20 ravel optmem recall [<soul>] <words>  where words appear across the soul's life\n\
         \x20 ravel optmem check [<soul>]           test it end to end\n\n\
         `ravel --help` says what each reads."
    );
    Ok(())
}

fn optmem_open(client: &Client, id: &str, node: &str) -> Result<()> {
    // `#12` is how a note is written, but `#` would end the URL path, so the
    // daemon would only ever see `/memory/` and answer 404.
    let node = node.trim_start_matches('#');
    let v = client.get(&format!("/souls/{id}/memory/{node}"))?;
    if v["kind"] == "block" {
        print_lines(v["children"].as_array().map(Vec::as_slice).unwrap_or(&[]));
        return Ok(());
    }
    let m = &v["memory"];
    println!(
        "#{} {}  {}",
        v["position"],
        short(m["ts"].as_str().unwrap_or("")),
        m["text"].as_str().unwrap_or("")
    );
    for t in v["turns"].as_array().cloned().unwrap_or_default() {
        let text = t["text"].as_str().unwrap_or("(no text)");
        let clipped: String = text.chars().take(600).collect();
        println!(
            "\n--- {} {} turn {}\n{}{}",
            short(t["ts"].as_str().unwrap_or("")),
            t["role"].as_str().unwrap_or("?"),
            t["turn"].as_str().unwrap_or("?"),
            clipped,
            if text.chars().count() > 600 {
                " […]"
            } else {
                ""
            }
        );
    }
    Ok(())
}

/// `ravel optmem recall <words>`: a compact picture of one term across a
/// soul's whole history, for the notes the wake has compressed away. How often
/// it was said and when, how often it was noted and in which months, the
/// highest merge that still names it, and the latest notes, with the ids
/// `optmem open` takes. Matched as a whole word or phrase, ignoring case.
fn optmem_recall(client: &Client, id: &str, term: &str) -> Result<()> {
    const MEM: &str = "<https://repolex.ai/ravel/NamedGraph/memory-v1>";
    let ns = ravel::RAVEL_NS;
    let lower = term.to_lowercase();
    let lit = serde_json::to_string(&lower)?;
    let word = "[^A-Za-z0-9_]";
    let re = serde_json::to_string(&format!("(^|{word}){}($|{word})", regex_escape(&lower)))?;
    let hit =
        |var: &str| format!("FILTER(CONTAINS(LCASE({var}), {lit}) && REGEX({var}, {re}, \"i\"))");
    let q = |sparql: String| -> Result<Vec<Value>> {
        let v = client.post(
            &format!("/souls/{id}/query"),
            Some(serde_json::json!({ "query": format!("PREFIX ravel: <{ns}>\n{sparql}") })),
        )?;
        Ok(v["results"]["bindings"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    };
    let val = |b: &Value, k: &str| b[k]["value"].as_str().unwrap_or("").to_string();
    let notes = q(format!(
        "SELECT ?i ?ts ?x WHERE {{ GRAPH {MEM} {{ ?m ravel:memoryLevel 0 ; ravel:logStart ?i ; ravel:windowStart ?ts ; ravel:memoryText ?x . {} }} }} ORDER BY ?i",
        hit("?x")
    ))?;
    let top = q(format!(
        "SELECT ?a ?b ?x WHERE {{ GRAPH {MEM} {{ ?m ravel:memoryLevel ?l ; ravel:logStart ?a ; ravel:logEnd ?b ; ravel:memoryText ?x . FILTER(?l > 0) {} }} }} ORDER BY DESC(?l) ?a LIMIT 1",
        hit("?x")
    ))?;
    let turns = q(format!(
        "SELECT (COUNT(DISTINCT ?u) AS ?n) (MIN(STR(?ts)) AS ?first) (MAX(STR(?ts)) AS ?last) WHERE {{ GRAPH ?g {{ ?u a ravel:Turn ; ravel:text ?x ; ravel:timestamp ?ts . {} }} }}",
        hit("?x")
    ))?;
    let t = turns.first().cloned().unwrap_or_default();
    let said = val(&t, "n");
    if said.is_empty() || said == "0" {
        println!("«{term}»: never said in this soul's turns.");
    } else {
        println!(
            "«{term}»: said in {said} turns, {} to {}",
            &val(&t, "first")[..10.min(val(&t, "first").len())],
            &val(&t, "last")[..10.min(val(&t, "last").len())]
        );
    }
    if notes.is_empty() {
        println!("  never noted.");
        return Ok(());
    }
    let mut months: std::collections::BTreeMap<String, usize> = Default::default();
    for n in &notes {
        *months
            .entry(val(n, "ts").chars().take(7).collect())
            .or_default() += 1;
    }
    let by: Vec<String> = months.iter().map(|(m, c)| format!("{m} {c}")).collect();
    println!("  noted {} times: {}", notes.len(), by.join(" · "));
    if let Some(b) = top.first() {
        // logEnd is inclusive, as OptMem writes a block id.
        println!(
            "  highest merge naming it: #{}-{} {}",
            val(b, "a"),
            val(b, "b"),
            val(b, "x")
        );
    }
    for n in notes.iter().rev().take(5).rev() {
        println!(
            "  #{} {} {}",
            val(n, "i"),
            short(&val(n, "ts")),
            val(n, "x")
        );
    }
    Ok(())
}

/// Escape a literal for an XPath regex (SPARQL REGEX).
fn regex_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if "\\.^$|?*+()[]{}-".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

fn import(client: &Client, id: &str, dir: &str) -> Result<()> {
    let abs = std::path::Path::new(dir)
        .canonicalize()
        .with_context(|| format!("{dir} does not exist"))?;
    let v = client.post(
        &format!("/souls/{id}/import"),
        Some(serde_json::json!({ "path": abs.display().to_string() })),
    )?;
    let s = &v["summary"];
    let export = v["export"].as_str().unwrap_or("?");
    if v["status"] == "copied" {
        println!(
            "[ravel import] {id} {export}: copied ({} file(s), {} bytes)",
            v["files"], v["bytes"]
        );
    } else {
        println!("[ravel import] {id} {export}: already imported, nothing copied");
    }
    println!(
        "[ravel import] {id} claude.ai: {} export(s) → ingested {} conversation(s), {} turns ({} already current)",
        s["claude_ai_exports"], s["claude_ai_conversations"], s["claude_ai_turns"], s["claude_ai_skipped"]
    );
    if let Some(f) = s["claude_ai_failed"].as_array().filter(|f| !f.is_empty()) {
        for x in f {
            eprintln!("[ravel import] FAILED {}", x.as_str().unwrap_or("?"));
        }
        std::process::exit(1);
    }
    Ok(())
}

fn sync(client: &Client, id: &str, sessions_dir: Option<&str>) -> Result<()> {
    let body = sessions_dir.map(|d| {
        let abs = std::path::Path::new(d)
            .canonicalize()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| d.to_string());
        serde_json::json!({ "sessions_dir": abs })
    });
    let v = client.post(&format!("/souls/{id}/sync"), body)?;
    let s = &v["summary"];
    println!(
        "[ravel sync] {id} claude-code: mirrored {} (unchanged {}) → ingested {} session(s) ({} skipped unchanged), {} turns, {} emojikeys",
        s["mirrored"], s["unchanged"], s["sessions"], s["skipped"], s["turns"], s["keys"]
    );
    let agy_any = [
        "agy_mirrored",
        "agy_unchanged",
        "agy_sessions",
        "agy_skipped",
    ]
    .iter()
    .map(|k| s[k].as_u64().unwrap_or(0))
    .sum::<u64>();
    if agy_any > 0 {
        println!(
            "[ravel sync] {id} agy:         mirrored {} (unchanged {}) → ingested {} conversation(s) ({} skipped unchanged), {} turns, {} spine gap(s)",
            s["agy_mirrored"], s["agy_unchanged"], s["agy_sessions"], s["agy_skipped"], s["agy_turns"], s["agy_gaps"]
        );
    }
    for k in ["failed", "agy_lossy", "agy_unattributed", "agy_failed"] {
        if let Some(list) = s[k].as_array() {
            if !list.is_empty() {
                println!(
                    "[ravel sync] {id} {}: {}",
                    k.trim_start_matches("agy_"),
                    list.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }
    Ok(())
}

/// A soul's memory pass in words, or `None` before the first one.
fn memory_line(m: &Value) -> Option<String> {
    let last = m["last"].as_str()?;
    let when = m["last_pass"].as_str().map(short).unwrap_or_default();
    Some(match m["error"].as_str() {
        Some(e) => format!(
            "{when}: FAILED — {e}; next try after {}",
            m["retry_after"].as_str().map(short).unwrap_or_default()
        ),
        None => format!("{when}: {last}"),
    })
}

/// `ravel status <soul>`: the health check, the store's counts and the
/// memory, in one place. Exit 1 when the health check wants attention.
fn soul_status(client: &Client, id: &str) -> Result<()> {
    let v = client.get(&format!("/souls/{id}/health"))?;
    println!("[ravel status] {id} {}", v["path"].as_str().unwrap_or("?"));
    for f in v["findings"].as_array().unwrap_or(&Vec::new()) {
        println!(
            "  {}{}",
            if f["attention"].as_bool().unwrap_or(false) {
                "ATTENTION — "
            } else {
                ""
            },
            f["line"].as_str().unwrap_or("")
        );
    }
    let attention = v["attention"].as_bool().unwrap_or(false);
    if let Ok(st) = client.get(&format!("/souls/{id}/stats")) {
        println!(
            "  store: {} turns ({} dated, {} to {}), {} quads in {} graphs, {} unasserted claims",
            st["turns"],
            st["dated_turns"],
            st["earliest"].as_str().map(short).unwrap_or_default(),
            st["latest"].as_str().map(short).unwrap_or_default(),
            st["quads"],
            st["graphs"],
            st["claims"]
        );
    }
    if let Ok(p) = client.get(&format!("/souls/{id}/memory-progress")) {
        println!(
            "  memory: {} notes, {} merges ({} due), {} of {} turns read",
            p["memories"], p["blocks"], p["pending_blocks"], p["turns_read"], p["turns"]
        );
    }
    let souls = client.get("/souls")?;
    if let Some(m) = souls
        .as_array()
        .and_then(|a| a.iter().find(|s| s["id"] == id))
        .and_then(|s| memory_line(&s["memory"]))
    {
        println!("  last memory pass {m}");
    }
    println!(
        "  verdict: {}",
        if attention {
            "ATTENTION (see above)"
        } else {
            "OK"
        }
    );
    if attention {
        std::process::exit(1);
    }
    Ok(())
}

/// `ravel optmem check`: every failure by name; exit 1 on any.
fn optmem_check(client: &Client, id: &str) -> Result<()> {
    let v = client.get(&format!("/souls/{id}/memory-check"))?;
    let c = &v["check"];
    println!(
        "[ravel optmem check] {id}: {} notes, {} merges, {} turn links, {} wake lines",
        c["notes"], c["blocks"], c["turn_links"], c["wake_lines"]
    );
    let failures = c["failures"].as_array().cloned().unwrap_or_default();
    for f in &failures {
        println!("  FAILED: {}", f.as_str().unwrap_or("?"));
    }
    if failures.is_empty() {
        println!("  every check passed");
        return Ok(());
    }
    std::process::exit(1);
}

fn query(client: &Client, id: &str, q: &str) -> Result<()> {
    let v = client.post(
        &format!("/souls/{id}/query"),
        Some(serde_json::json!({ "query": q })),
    )?;
    if let Some(b) = v["boolean"].as_bool() {
        println!("{b}");
        return Ok(());
    }
    if let Some(triples) = v["triples"].as_array() {
        for t in triples {
            println!("{}", t.as_str().unwrap_or(""));
        }
        return Ok(());
    }
    let vars: Vec<&str> = v["head"]["vars"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    let rows = v["results"]["bindings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for row in &rows {
        let cells: Vec<String> = vars
            .iter()
            .map(|var| {
                let t = &row[*var];
                match t["type"].as_str() {
                    Some("uri") => format!("{var}=<{}>", t["value"].as_str().unwrap_or("")),
                    Some(_) => format!("{var}={}", t["value"].as_str().unwrap_or("")),
                    None => format!("{var}="),
                }
            })
            .collect();
        println!("  {}", cells.join("   "));
    }
    println!("[{} row(s)]", rows.len());
    Ok(())
}

/// `ravel query help`: what a query can ask, read from the ontology built
/// into this binary, so it can never drift from what ravel declares. The
/// layout and the examples are what the ontology cannot say, and what a first
/// query trips on: without `GRAPH`, nothing matches (goodlux, 2026-11-12).
fn query_help() -> Result<String> {
    use oxigraph::io::{RdfFormat, RdfParser};
    use oxigraph::model::{Term, TermRef};
    use std::collections::BTreeMap;
    const TTL: &str = include_str!("../../ontology/ravel/ravel.ttl");
    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    const COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
    const DOMAIN: &str = "http://www.w3.org/2000/01/rdf-schema#domain";
    const RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";
    let ns = ravel::RAVEL_NS;
    #[derive(Default)]
    struct Term_ {
        kind: String,
        comment: String,
        domain: String,
        range: String,
    }
    let mut terms: BTreeMap<String, Term_> = BTreeMap::new();
    let local = |t: &Term| -> String {
        match t.as_ref() {
            TermRef::NamedNode(n) => {
                let s = n.as_str();
                s.rsplit(['/', '#']).next().unwrap_or(s).to_string()
            }
            TermRef::Literal(l) => l.value().to_string(),
            _ => String::new(),
        }
    };
    for q in RdfParser::from_format(RdfFormat::Turtle).for_slice(TTL.as_bytes()) {
        let q = q.context("ravel.ttl does not parse")?;
        let oxigraph::model::NamedOrBlankNode::NamedNode(subj) = &q.subject else {
            continue;
        };
        let Some(name) = subj.as_str().strip_prefix(ns) else {
            continue;
        };
        let e = terms.entry(name.to_string()).or_default();
        match q.predicate.as_str() {
            RDF_TYPE => e.kind = local(&q.object),
            COMMENT => e.comment = local(&q.object),
            DOMAIN => e.domain = local(&q.object),
            RANGE => e.range = local(&q.object),
            _ => {}
        }
    }
    // A comment's first sentence, which is its claim; the rest is detail.
    // "e.g." and "i.e." are not the end of a sentence.
    let first = |c: &str| -> String {
        let mut from = 0;
        while let Some(i) = c[from..].find(". ").map(|i| i + from) {
            if !(c[..i].ends_with("e.g") || c[..i].ends_with("i.e")) {
                return c[..=i].trim().to_string();
            }
            from = i + 2;
        }
        c.trim().to_string()
    };
    let mut out = String::from(
        "ravel query help: what the transcript graph holds, and how to ask it.\n\n\
         VOCABULARY (from ravel's ontology; prefix ravel: <https://repolex.ai/ontology/ravel/>)\n",
    );
    let classes: Vec<&String> = terms
        .iter()
        .filter(|(_, t)| t.kind == "Class")
        .map(|(n, _)| n)
        .collect();
    let mut groups: Vec<(String, String)> = classes
        .iter()
        .map(|c| ((*c).clone(), format!("ravel:{c}: {}", first(&terms[*c].comment))))
        .collect();
    groups.push((
        String::new(),
        "Reader annotations: what a reader found in a turn, as W3C annotations (oa:, prov:)".into(),
    ));
    for (class, heading) in &groups {
        out += &format!("\n  {heading}\n");
        for (n, t) in &terms {
            if !t.kind.ends_with("Property") || t.domain != *class {
                continue;
            }
            if class.is_empty() && classes.contains(&&t.domain) {
                continue;
            }
            out += &format!("    ravel:{n} ({}): {}\n", t.range, first(&t.comment));
        }
    }
    out += r#"
WHERE THINGS ARE STORED (each soul has its own store; `ravel query <soul> ...`)
  Nothing is in the default graph: every pattern goes inside GRAPH, or the
  query finds nothing.
  - Each transcript is its own graph, so a query over every conversation
    says GRAPH ?g { ... }.
  - optmem is the graph <https://repolex.ai/ravel/NamedGraph/memory-v1>. A
    note (memoryLevel 0) links to each turn it was written from, and a merge
    to its two halves, with <https://repolex.ai/ontology/git-lex/relatedToId>.
  - Instances are named under https://repolex.ai/ravel/<Class>/<id>, for
    example <https://repolex.ai/ravel/Turn/<event id>>.

THREE QUERIES THAT WORK AS WRITTEN
  Which models answered this soul, and how often:
    PREFIX ravel: <https://repolex.ai/ontology/ravel/>
    SELECT ?model (COUNT(?t) AS ?turns)
    WHERE { GRAPH ?g { ?t a ravel:Turn ; ravel:model ?model } }
    GROUP BY ?model ORDER BY DESC(?turns)
  (Antigravity turns carry ravel:modelSetting instead of ravel:model.)

  One day's turns, in order:
    PREFIX ravel: <https://repolex.ai/ontology/ravel/>
    SELECT ?ts ?role (SUBSTR(?text, 1, 80) AS ?start)
    WHERE { GRAPH ?g { ?t a ravel:Turn ; ravel:timestamp ?ts ;
                         ravel:role ?role ; ravel:text ?text .
                       FILTER(STRSTARTS(STR(?ts), "2026-11-11")) } }
    ORDER BY ?ts LIMIT 50

  An optmem note, with the turns it was written from (note #12):
    PREFIX ravel: <https://repolex.ai/ontology/ravel/>
    PREFIX lex: <https://repolex.ai/ontology/git-lex/>
    SELECT ?note ?ts (SUBSTR(?said, 1, 80) AS ?start) WHERE {
      GRAPH <https://repolex.ai/ravel/NamedGraph/memory-v1> {
        ?m ravel:memoryLevel 0 ; ravel:logStart 12 ;
           ravel:memoryText ?note ; lex:relatedToId ?t }
      GRAPH ?g { ?t ravel:timestamp ?ts ; ravel:text ?said } }
    ORDER BY ?ts
"#;
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_span_drops_only_a_repeated_year() {
        assert_eq!(super::span("2026-04-03", "2026-04-08"), "2026-04-03..04-08");
        assert_eq!(
            super::span("2025-12-30", "2026-01-02"),
            "2025-12-30..2026-01-02"
        );
        assert_eq!(super::span("2026-04-03", "2026-04-03"), "2026-04-03");
    }

    #[test]
    fn query_help_names_every_declared_property_with_its_description() {
        let h = super::query_help().unwrap();
        let ttl = include_str!("../../ontology/ravel/ravel.ttl");
        for line in ttl.lines() {
            let Some(rest) = line.strip_prefix("ravel:") else {
                continue;
            };
            if line.contains("Property ;") {
                let name = rest.split_whitespace().next().unwrap();
                assert!(h.contains(&format!("ravel:{name} (")), "{name} missing");
            }
        }
        assert!(!h.contains("): \n"), "a property without a description");
        assert!(h.contains("GRAPH ?g"));
    }
}
