//! raveld — the ravel daemon.
//!
//!   raveld          run in the foreground; refuses if a raveld is already up
//!   raveld restart  kill EVERY raveld on this machine, then run in the foreground
//!   raveld start    the same as restart, kept because fingers remember it
//!   raveld stop     kill EVERY raveld on this machine and exit
//!   raveld status   say whether one is running, and what it has been doing
//!
//! No flags. Everything else is in `~/.config/ravel/config.yml` (port,
//! interval, souls). A missing file means no souls, and status says so.
//!
//! `ravel` (the command line) starts raveld detached when none is running,
//! so most days nobody types `raveld` at all.
//!
//! "Every raveld" means: every process named raveld other than this one gets
//! SIGTERM, then SIGKILL if it is still there five seconds later. Two daemons
//! alive at once cost pan a morning (goodlux, 2026-09-04); start and stop
//! must leave exactly one or zero.

use anyhow::Result;
use ravel::client::{self, Client};
use ravel::daemon::{self, config::DaemonConfig, Daemon};
use std::sync::Arc;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => {
            refuse_if_already_running();
            serve()
        }
        ["restart"] | ["start"] => {
            stop_all();
            serve()
        }
        ["stop"] => {
            stop_all();
            Ok(())
        }
        ["status"] => status(),
        ["--version"] | ["-V"] => {
            println!("raveld {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!(
                "raveld {}\n\nusage: raveld | raveld restart | raveld stop | raveld status\n  (no flags; configure in {})",
                env!("CARGO_PKG_VERSION"),
                daemon::config::config_path().display()
            );
            std::process::exit(2);
        }
    }
}

/// Say so and stop, rather than letting the store layer fail on a lock file
/// somebody has to recognise.
fn refuse_if_already_running() {
    let pids = client::other_daemons();
    if pids.is_empty() {
        return;
    }
    let who = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!("raveld is already running (pid {who}).");
    match DaemonConfig::load().map(|c| Client::new(&c).health()) {
        Ok(Ok(h)) => {
            let up = h["uptime_secs"].as_u64().unwrap_or(0);
            eprintln!(
                "  it is answering, version {}, up {}h {:02}m.",
                h["version"].as_str().unwrap_or("?"),
                up / 3600,
                (up % 3600) / 60
            );
        }
        _ => eprintln!("  it is not answering on its port, so it may be wedged."),
    }
    eprintln!("  raveld restart   stop it and run this build here");
    eprintln!("  raveld stop      stop it and leave nothing running");
    eprintln!("  raveld status    what it is doing right now");
    std::process::exit(1);
}

fn stop_all() {
    // Ask nicely first, so an in-flight sync pass finishes its write.
    if let Ok(cfg) = DaemonConfig::load() {
        let _ = Client::new(&cfg).post("/shutdown", None);
    }
    let pids = client::other_daemons();
    if pids.is_empty() {
        return;
    }
    for p in &pids {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &p.to_string()])
            .status();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && !client::other_daemons().is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    for p in client::other_daemons() {
        eprintln!("raveld pid {p} ignored SIGTERM; sending SIGKILL");
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &p.to_string()])
            .status();
    }
    eprintln!(
        "stopped raveld (pid {})",
        pids.iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

fn serve() -> Result<()> {
    let cfg = DaemonConfig::load()?;
    daemon::log(format!(
        "raveld {} starting; config {}{}; {} soul(s); every {}s",
        env!("CARGO_PKG_VERSION"),
        cfg.path.display(),
        if cfg.present { "" } else { " (absent)" },
        cfg.souls.len(),
        cfg.interval_secs
    ));
    let d = Arc::new(Daemon::open(cfg)?);
    for p in &d.problems {
        daemon::log(format!("config problem: {p}"));
    }
    for s in &d.souls {
        daemon::log(format!("soul {} at {}", s.id, s.path.display()));
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let looper = d.clone();
        tokio::spawn(daemon::run_sync_loop(looper));
        daemon::http::serve(d).await
    })
}

/// Plain words, exit 0 when running and answering, 1 otherwise — so a
/// script can ask too.
fn status() -> Result<()> {
    let cfg = DaemonConfig::load()?;
    let client = Client::new(&cfg);
    let pids: Vec<String> = client::other_daemons()
        .iter()
        .map(|p| p.to_string())
        .collect();
    match client.health() {
        Ok(h) => {
            let up = h["uptime_secs"].as_u64().unwrap_or(0);
            println!(
                "raveld is RUNNING — pid {}, up {}h {:02}m {:02}s, version {}",
                h["pid"].as_u64().unwrap_or(0),
                up / 3600,
                (up % 3600) / 60,
                up % 60,
                h["version"].as_str().unwrap_or("?")
            );
            println!(
                "  serving {} for {} soul(s); {} pass(es) since start; config {}{}",
                cfg.base_url(),
                h["souls"].as_array().map(|a| a.len()).unwrap_or(0),
                h["passes"].as_u64().unwrap_or(0),
                h["config"].as_str().unwrap_or("?"),
                if h["config_present"].as_bool().unwrap_or(false) {
                    ""
                } else {
                    " (ABSENT — no souls will be synced)"
                }
            );
            if let Some(problems) = h["config_problems"].as_array() {
                for p in problems {
                    println!("  CONFIG PROBLEM: {}", p.as_str().unwrap_or("?"));
                }
            }
            if let Ok(souls) = client.get("/souls") {
                for s in souls.as_array().unwrap_or(&Vec::new()) {
                    let last = &s["last"];
                    println!(
                        "  {}  {}  last pass {}  {}",
                        s["id"].as_str().unwrap_or("?"),
                        s["path"].as_str().unwrap_or("?"),
                        s["last_sync"].as_str().unwrap_or("never"),
                        match s["last_error"].as_str() {
                            Some(e) => format!("ERROR: {e}"),
                            None if last.is_null() => String::new(),
                            None => format!(
                                "({} session(s) ingested, {} turns; agy {} conversation(s))",
                                last["sessions"], last["turns"], last["agy_sessions"]
                            ),
                        }
                    );
                }
            }
            Ok(())
        }
        Err(_) if !pids.is_empty() => {
            println!(
                "raveld is running (pid {}) but NOT answering on {} — `raveld restart`",
                pids.join(", "),
                cfg.base_url()
            );
            std::process::exit(1);
        }
        Err(_) => {
            println!(
                "raveld is not running. Any `ravel` command starts it; or run `raveld` in a terminal."
            );
            std::process::exit(1);
        }
    }
}
