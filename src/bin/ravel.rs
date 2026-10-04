//! ravel — the command line. A thin client of raveld; it never opens a store.
//! When raveld is not running, any command here starts it first
//! (goodlux, 2026-09-22).
//!
//!   ravel                            what raveld is doing (same as `raveld status`)
//!   ravel souls                      the souls this machine's raveld serves
//!   ravel sync   [<soul> [<sessions-dir>]]  a pass now (raveld does this on its own anyway);
//!                                    <sessions-dir> reads another directory's session files
//!                                    into this soul — how a sibling slug's history comes home
//!   ravel import [<soul>] <export-dir>  copy a claude.ai data export into the soul
//!                                    and ingest it; safe to repeat, and a newer
//!                                    export later adds to it
//!   ravel memory [<soul>]            the whole history in about 96 lines: recent
//!                                    memories word for word, older ones summarized
//!   ravel memory open [<soul>] <id>  one line opened: a summary's two halves, or a
//!                                    memory's source turns
//!   ravel memory run [<soul>]        read new turns and write summaries now
//!   ravel health [<soul>]            the read-only diagnostic; exit 1 = attention
//!   ravel stats  [<soul>]            counts: quads, graphs, turns, claims, top predicates
//!   ravel query  [<soul>] "<sparql>" rows, one per line
//!   ravel emojikeys [<soul>] [authored]
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
           ravel souls\n  \
           ravel sync   [<soul> [<sessions-dir>]]\n  \
           ravel import [<soul>] <export-dir>\n  \
           ravel memory [<soul>] | memory open [<soul>] <id> | memory run [<soul>]\n  \
           ravel health [<soul>]\n  \
           ravel stats  [<soul>]\n  \
           ravel query  [<soul>] \"<sparql>\"\n  \
           ravel emojikeys [<soul>] [authored]\n\n\
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
        _ => {}
    }
    let cfg = DaemonConfig::load()?;
    let client = ensure_running(&cfg)?;
    match a.as_slice() {
        [] | ["status"] => status(&cfg, &client),
        ["souls"] => souls(&client),
        ["sync"] => sync(&client, &resolve(&client, None)?, None),
        ["sync", s] => sync(&client, &resolve(&client, Some(s))?, None),
        ["sync", s, dir] => sync(&client, &resolve(&client, Some(s))?, Some(dir)),
        ["import", dir] => import(&client, &resolve(&client, None)?, dir),
        ["import", s, dir] => import(&client, &resolve(&client, Some(s))?, dir),
        ["memory"] => memory(&client, &resolve(&client, None)?),
        ["memory", "run"] => memory_run(&client, &resolve(&client, None)?),
        ["memory", "run", s] => memory_run(&client, &resolve(&client, Some(s))?),
        ["memory", "open", n] => memory_open(&client, &resolve(&client, None)?, n),
        ["memory", "open", s, n] => memory_open(&client, &resolve(&client, Some(s))?, n),
        ["memory", s] => memory(&client, &resolve(&client, Some(s))?),
        ["health"] => health(&client, &resolve(&client, None)?),
        ["health", s] => health(&client, &resolve(&client, Some(s))?),
        ["stats"] => stats(&client, &resolve(&client, None)?),
        ["stats", s] => stats(&client, &resolve(&client, Some(s))?),
        ["query", q] => query(&client, &resolve(&client, None)?, q),
        ["query", s, q] => query(&client, &resolve(&client, Some(s))?, q),
        ["emojikeys"] => emojikeys(&client, &resolve(&client, None)?, None),
        ["emojikeys", "authored"] => emojikeys(&client, &resolve(&client, None)?, Some("authored")),
        ["emojikeys", s] => emojikeys(&client, &resolve(&client, Some(s))?, None),
        ["emojikeys", s, "authored"] => {
            emojikeys(&client, &resolve(&client, Some(s))?, Some("authored"))
        }
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
    anyhow!(
        "{} is not inside a registered soul. Registered: {}.\n  Add the repo under souls: in {} and run `raveld restart`.",
        p.display(),
        if souls.is_empty() {
            "(none)".to_string()
        } else {
            souls
                .iter()
                .map(|s| format!("{} {}", s["id"].as_str().unwrap_or("?"), s["path"].as_str().unwrap_or("?")))
                .collect::<Vec<_>>()
                .join("; ")
        },
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
    println!("  memory index: {}", h["memory"].as_str().unwrap_or("?"));
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
    }
    Ok(())
}

/// `2024-03-07T14:22:10Z` → `2024-03-07 14:22`.
fn short(ts: &str) -> String {
    ts.get(..16).unwrap_or(ts).replace('T', " ")
}

fn print_lines(lines: &[Value]) {
    for l in lines {
        let level = l["level"].as_u64().unwrap_or(0);
        let when = if level == 0 {
            short(l["from"].as_str().unwrap_or(""))
        } else {
            format!(
                "{} → {}",
                l["from"].as_str().unwrap_or("").get(..10).unwrap_or(""),
                l["to"].as_str().unwrap_or("").get(..10).unwrap_or("")
            )
        };
        println!(
            "{when:<23}  {}  [{}]",
            l["text"].as_str().unwrap_or(""),
            l["id"].as_str().unwrap_or("")
        );
    }
}

fn memory(client: &Client, id: &str) -> Result<()> {
    let v = client.get(&format!("/souls/{id}/memory"))?;
    let lines = v["lines"].as_array().cloned().unwrap_or_default();
    if lines.is_empty() {
        println!(
            "[ravel memory] {id}: no memories yet. `ravel` shows whether the memory index is on."
        );
        return Ok(());
    }
    println!(
        "[ravel memory] {id}: {} memories, {} summaries — `ravel memory open <id>` opens any line",
        v["memories"], v["summaries"]
    );
    print_lines(&lines);
    Ok(())
}

fn memory_open(client: &Client, id: &str, node: &str) -> Result<()> {
    let v = client.get(&format!("/souls/{id}/memory/{node}"))?;
    if v["kind"] == "window" {
        print_lines(v["children"].as_array().map(Vec::as_slice).unwrap_or(&[]));
        return Ok(());
    }
    let m = &v["memory"];
    println!(
        "{}  {}",
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

fn memory_run(client: &Client, id: &str) -> Result<()> {
    let v = client.post(&format!("/souls/{id}/memory/run"), None)?;
    let r = &v["report"];
    println!(
        "[ravel memory run] {id}: read {} chunk(s) → {} memories, {} summaries; {} in / {} out tokens",
        r["chunks"], r["memories"], r["summaries"], r["input_tokens"], r["output_tokens"]
    );
    if let Some(s) = r["stopped"].as_str() {
        println!("[ravel memory run] stopped: {s}");
    }
    let failed = r["failed"].as_array().cloned().unwrap_or_default();
    for f in &failed {
        eprintln!("[ravel memory run] FAILED {}", f.as_str().unwrap_or("?"));
    }
    if !failed.is_empty() {
        std::process::exit(1);
    }
    Ok(())
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
    println!(
        "[ravel import] {id} {}: {} ({} file(s), {} bytes)",
        v["export"].as_str().unwrap_or("?"),
        v["status"].as_str().unwrap_or("?"),
        v["files"],
        v["bytes"]
    );
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

fn health(client: &Client, id: &str) -> Result<()> {
    let v = client.get(&format!("/souls/{id}/health"))?;
    println!("[ravel health] {id} {}", v["path"].as_str().unwrap_or("?"));
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

fn stats(client: &Client, id: &str) -> Result<()> {
    let v = client.get(&format!("/souls/{id}/stats"))?;
    println!("=== {id} {} ===", v["path"].as_str().unwrap_or("?"));
    println!("total quads:       {}", v["quads"]);
    println!("named graphs:      {}", v["graphs"]);
    println!("distinct subjects: {}", v["subjects"]);
    println!("Turn subjects:     {}", v["turns"]);
    println!("dated turns:       {}", v["dated_turns"]);
    println!(
        "earliest:          {}",
        v["earliest"].as_str().unwrap_or("")
    );
    println!("latest:            {}", v["latest"].as_str().unwrap_or(""));
    println!("unasserted claims: {}", v["claims"]);
    println!("--- top predicates ---");
    for p in v["top_predicates"].as_array().unwrap_or(&Vec::new()) {
        println!(
            "  {:>7}  {}",
            p["n"],
            p["predicate"].as_str().unwrap_or("?")
        );
    }
    Ok(())
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

fn emojikeys(client: &Client, id: &str, origin: Option<&str>) -> Result<()> {
    let path = match origin {
        Some(o) => format!("/souls/{id}/emojikeys?origin={o}"),
        None => format!("/souls/{id}/emojikeys"),
    };
    let v = client.get(&path)?;
    let hits = v.as_array().cloned().unwrap_or_default();
    let mut last = String::new();
    for h in &hits {
        let t = h["transcript"].as_str().unwrap_or("");
        if t != last {
            println!("▸ session {}", if t.len() > 12 { &t[..12] } else { t });
            last = t.to_string();
        }
        println!(
            "    [{}] [ME|{}]~[CONTENT|{}]~[YOU|{}]  @ {}",
            h["origin"].as_str().unwrap_or("?"),
            h["me"].as_str().unwrap_or(""),
            h["content"].as_str().unwrap_or(""),
            h["you"].as_str().unwrap_or(""),
            h["ts"].as_str().unwrap_or("")
        );
    }
    println!("[{} emojikey(s)]", hits.len());
    Ok(())
}
