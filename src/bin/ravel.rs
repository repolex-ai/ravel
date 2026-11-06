//! ravel — the command line. A thin client of raveld; it never opens a store.
//! When raveld is not running, any command here starts it first
//! (goodlux, 2026-09-22).
//!
//!   ravel                            what raveld is doing, and every soul it serves
//!   ravel status [<soul>]            one soul: its health check (exit 1 = attention),
//!                                    its store's counts, and its memory
//!   ravel souls                      the souls this machine's raveld serves
//!   ravel sync   [<soul> [<sessions-dir>]]  a pass now (raveld does this on its own anyway);
//!                                    <sessions-dir> reads another directory's session files
//!                                    into this soul — how a sibling slug's history comes home
//!   ravel import [<soul>] <export-dir>  copy a claude.ai data export into the soul
//!                                    and ingest it; safe to repeat, and a newer
//!                                    export later adds to it
//!   ravel memory [<soul>]            OptMem's wake: the whole memory in at most 96
//!                                    lines, recent notes word for word, older ones merged
//!   ravel memory open [<soul>] <id>  one line opened, as OptMem's zoom: a block's two
//!                                    halves (`16-31`), or a note's source turns (`#12`)
//!   ravel memory recall <words>      one term across the whole history
//!   ravel memory check [<soul>]      the memory checked end to end; exit 1 on any failure
//!   ravel query  [<soul>] "<sparql>" rows, one per line
//!
//! `<soul>` is a six-character id (the start of the soul's genesis commit) or
//! a path inside the soul repo; absent = the repo this command runs in.
//! No flags.

use anyhow::{anyhow, Context, Result};
use ravel::client::{ensure_running, Client};
use ravel::daemon::config::DaemonConfig;
use serde_json::Value;

fn usage() -> ! {
    eprintln!(
        "ravel {} — talks to raveld (and starts it when it is not running)\n\n\
         USAGE:\n  \
           ravel\n  \
           ravel status [<soul>]\n  \
           ravel souls\n  \
           ravel sync   [<soul> [<sessions-dir>]]\n  \
           ravel import [<soul>] <export-dir>\n  \
           ravel memory [<soul>] | memory open [<soul>] <id> | memory recall <words> | memory check [<soul>]\n  \
           ravel query  [<soul>] \"<sparql>\"\n\n\
         <soul> = six-character id or a path inside the soul repo; absent = here.\n\
         Config: {}",
        env!("CARGO_PKG_VERSION"),
        ravel::daemon::config::config_path().display()
    );
    std::process::exit(2);
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["--version"] | ["-V"] => {
            println!("ravel {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        ["--help"] | ["-h"] | ["help"] => usage(),
        // ravel has no flags; `ravel memory --help` must not be read as a
        // soul id (w3bl0rd, 2026-10-05).
        args if args.iter().any(|x| x.starts_with('-')) => usage(),
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
        ["memory"] => memory(&client, &resolve(&client, None)?),
        ["memory", "recall", words @ ..] if !words.is_empty() => {
            memory_recall(&client, &resolve(&client, None)?, &words.join(" "))
        }
        ["memory", "open", n] => memory_open(&client, &resolve(&client, None)?, n),
        ["memory", "open", s, n] => memory_open(&client, &resolve(&client, Some(s))?, n),
        ["memory", "check"] => memory_check(&client, &resolve(&client, None)?),
        ["memory", "check", s] => memory_check(&client, &resolve(&client, Some(s))?),
        ["memory", s] => memory(&client, &resolve(&client, Some(s))?),
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

fn memory(client: &Client, id: &str) -> Result<()> {
    let v = client.get(&format!("/souls/{id}/memory"))?;
    let lines = v["lines"].as_array().cloned().unwrap_or_default();
    if lines.is_empty() {
        println!("[ravel memory] {id}: no memories yet.");
        return Ok(());
    }
    println!(
        "[ravel memory] {id}: {} notes, {} blocks — `ravel memory open <#i or lo-hi>` opens a line",
        v["memories"], v["summaries"]
    );
    // A merge reads like a statement of fact, but it is how things stood over
    // its dates (selkie, 2026-11-01: an April rule had since changed).
    println!("Oldest first. Each line is how things stood over its dates; where two disagree, the later one is current.");
    print_lines(&lines);
    Ok(())
}

fn memory_open(client: &Client, id: &str, node: &str) -> Result<()> {
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

/// `ravel memory recall <words>`: a compact picture of one term across a
/// soul's whole history, for the notes the wake has compressed away. How often
/// it was said and when, how often it was noted and in which months, the
/// highest merge that still names it, and the latest notes, with the ids
/// `memory open` takes. Matched as a whole word or phrase, ignoring case.
fn memory_recall(client: &Client, id: &str, term: &str) -> Result<()> {
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

/// `ravel memory check`: every failure by name; exit 1 on any.
fn memory_check(client: &Client, id: &str) -> Result<()> {
    let v = client.get(&format!("/souls/{id}/memory-check"))?;
    let c = &v["check"];
    println!(
        "[ravel memory check] {id}: {} notes, {} merges, {} turn links, {} wake lines",
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
}
