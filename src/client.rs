//! What `ravel` and `raveld status` share: where the daemon is, whether it
//! answers, and starting it when it does not (goodlux, 2026-09-22: the
//! command line stays, and it starts raveld itself).

use crate::daemon::config::{self, DaemonConfig};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Client {
    base: String,
    http: reqwest::blocking::Client,
}

impl Client {
    pub fn new(cfg: &DaemonConfig) -> Self {
        Self {
            base: cfg.base_url(),
            http: reqwest::blocking::Client::builder()
                // A full pass over a large soul can take a while; the CLI waits.
                .timeout(Duration::from_secs(600))
                .build()
                .expect("http client"),
        }
    }

    /// `/health`, or the reason it did not answer. Short timeout: this is the
    /// question "is anyone there".
    pub fn health(&self) -> Result<Value> {
        let quick = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()?;
        let r = quick.get(format!("{}/health", self.base)).send()?;
        Ok(r.error_for_status()?.json()?)
    }

    fn finish(r: reqwest::blocking::Response) -> Result<Value> {
        let status = r.status();
        let v: Value = r.json().unwrap_or(Value::Null);
        if status.is_success() {
            Ok(v)
        } else {
            Err(anyhow!(
                "{}",
                v["error"].as_str().unwrap_or(status.as_str())
            ))
        }
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        Self::finish(self.http.get(format!("{}{path}", self.base)).send()?)
    }

    pub fn post(&self, path: &str, body: Option<Value>) -> Result<Value> {
        let req = self.http.post(format!("{}{path}", self.base));
        let req = match body {
            Some(b) => req.json(&b),
            None => req,
        };
        Self::finish(req.send()?)
    }
}

/// Every raveld process on this machine except this one.
pub fn other_daemons() -> Vec<u32> {
    let me = std::process::id();
    std::process::Command::new("pgrep")
        .args(["-x", "raveld"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.trim().parse::<u32>().ok())
                .filter(|p| *p != me)
                .collect()
        })
        .unwrap_or_default()
}

/// The raveld binary: beside this executable when installed together
/// (`cargo install` puts both in one directory), otherwise whatever is on PATH.
pub fn raveld_binary() -> PathBuf {
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            let sib = dir.join("raveld");
            if sib.is_file() {
                return sib;
            }
        }
    }
    PathBuf::from("raveld")
}

/// A client for a running daemon — started here if none answers. Detached:
/// no terminal owns it, its output goes to `~/.config/ravel/raveld.log`.
pub fn ensure_running(cfg: &DaemonConfig) -> Result<Client> {
    let client = Client::new(cfg);
    if client.health().is_ok() {
        return Ok(client);
    }
    if !other_daemons().is_empty() {
        return Err(anyhow!(
            "raveld is running (pid {}) but not answering on {} — `raveld restart`",
            other_daemons()
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            cfg.base_url()
        ));
    }
    let log = config::log_path();
    std::fs::create_dir_all(log.parent().unwrap_or(std::path::Path::new(".")))?;
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("open {}", log.display()))?;
    let err = out.try_clone()?;
    let bin = raveld_binary();
    let mut cmd = std::process::Command::new(&bin);
    cmd.stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: closing this terminal does not take it down.
        cmd.process_group(0);
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("start {}", bin.display()))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        if client.health().is_ok() {
            eprintln!(
                "started raveld (pid {}), log at {}",
                child.id(),
                log.display()
            );
            return Ok(client);
        }
    }
    Err(anyhow!(
        "started raveld (pid {}) but it did not answer on {} within 15s — see {}",
        child.id(),
        cfg.base_url(),
        log.display()
    ))
}
