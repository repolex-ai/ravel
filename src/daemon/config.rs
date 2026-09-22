//! raveld's configuration: `~/.config/ravel/config.yml`, one per machine.
//!
//! ```yaml
//! port: 7881
//! interval_secs: 30
//! souls:
//!   - ~/repos/7R1PL3F0RC3/spaceGOAT
//!   - ~/repos/7R1PL3F0RC3/W4R3Z
//! ```
//!
//! A missing file is not an error: the daemon starts with no souls and says
//! so in `/health` by name. "No config file" and "a config with zero souls"
//! must never be the same silent state.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Ruled 2026-09-22 (goodlux): 7881 for now.
pub const DEFAULT_PORT: u16 = 7881;
/// How often the daemon looks for new or grown transcripts. A pass over an
/// unchanged soul costs about a second (the ingest manifest skips everything
/// it has seen), so this can be short.
pub const DEFAULT_INTERVAL_SECS: u64 = 30;

#[derive(Debug, Clone, Deserialize, Default)]
struct Raw {
    port: Option<u16>,
    interval_secs: Option<u64>,
    #[serde(default)]
    souls: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Where the file is (or would be).
    pub path: PathBuf,
    /// Whether it was there.
    pub present: bool,
    pub port: u16,
    pub interval_secs: u64,
    /// Soul repo paths, `~` expanded, in file order.
    pub souls: Vec<PathBuf>,
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".config").join("ravel"))
        .unwrap_or_else(|| PathBuf::from(".config/ravel"))
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.yml")
}

/// Where a detached raveld writes what it would have said to a terminal.
pub fn log_path() -> PathBuf {
    config_dir().join("raveld.log")
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        if p == "~" {
            return PathBuf::from(home);
        }
        if let Some(rest) = p.strip_prefix("~/") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(p)
}

impl DaemonConfig {
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let (raw, present) = match std::fs::read_to_string(path) {
            Ok(s) if s.trim().is_empty() => (Raw::default(), true),
            Ok(s) => (
                serde_yaml::from_str::<Raw>(&s)
                    .with_context(|| format!("parse {}", path.display()))?,
                true,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Raw::default(), false),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        Ok(Self {
            path: path.to_path_buf(),
            present,
            port: raw.port.unwrap_or(DEFAULT_PORT),
            interval_secs: raw.interval_secs.unwrap_or(DEFAULT_INTERVAL_SECS),
            souls: raw.souls.iter().map(|s| expand_tilde(s)).collect(),
        })
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_defaults_and_says_so() {
        let cfg = DaemonConfig::load_from(Path::new("/nonexistent/ravel/config.yml")).unwrap();
        assert!(!cfg.present);
        assert_eq!(cfg.port, DEFAULT_PORT);
        assert_eq!(cfg.interval_secs, DEFAULT_INTERVAL_SECS);
        assert!(cfg.souls.is_empty());
    }

    #[test]
    fn file_parses_and_expands_tilde() {
        let dir = std::env::temp_dir().join("ravel-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.yml");
        std::fs::write(&p, "port: 9999\nsouls:\n  - ~/repos/x\n  - /abs/y\n").unwrap();
        let cfg = DaemonConfig::load_from(&p).unwrap();
        assert!(cfg.present);
        assert_eq!(cfg.port, 9999);
        assert_eq!(cfg.interval_secs, DEFAULT_INTERVAL_SECS);
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(cfg.souls[0], home.join("repos/x"));
        assert_eq!(cfg.souls[1], PathBuf::from("/abs/y"));
    }

    #[test]
    fn empty_file_is_present_with_defaults() {
        let dir = std::env::temp_dir().join("ravel-config-test-empty");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.yml");
        std::fs::write(&p, "\n").unwrap();
        let cfg = DaemonConfig::load_from(&p).unwrap();
        assert!(cfg.present);
        assert!(cfg.souls.is_empty());
    }
}
