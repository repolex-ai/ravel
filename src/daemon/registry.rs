//! The soul list — turns each configured repo path into (id, path).
//!
//! A soul's id is the FIRST SIX CHARACTERS of its repo's genesis commit: the
//! same id pan, git-lex, Horae and Syrinx already use for that soul. It is
//! read from `.lex/repo.yml` (git-lex writes `genesis_sha` there) or, failing
//! that, from git itself. Derived, never typed in.
//!
//! SIX CHARACTERS, EVERYWHERE (goodlux, 2026-09-18, ruled for pan; ravel
//! copies it): the id in a route, in a log line and on the command line is
//! one value. `SOUL_ID_LEN` is the one place the length is written down.
//!
//! A configured path that cannot be resolved does not stop the daemon: the
//! other souls still need their backups. It is reported by name in `/health`
//! on every call until fixed, which is the loud part.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SOUL_ID_LEN: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Soul {
    pub id: String,
    /// Canonical (symlinks resolved), because that is what Claude Code names
    /// the transcript directory after.
    pub path: PathBuf,
}

/// The full genesis SHA of a repo.
pub fn genesis_sha(repo: &Path) -> Result<String> {
    let repo_yml = repo.join(".lex").join("repo.yml");
    if let Ok(raw) = std::fs::read_to_string(&repo_yml) {
        for line in raw.lines() {
            if let Some(v) = line.trim_start().strip_prefix("genesis_sha:") {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if !v.is_empty() {
                    return Ok(v.to_string());
                }
            }
        }
    }
    let out = Command::new("git")
        .args(["rev-list", "--max-parents=0", "HEAD"])
        .current_dir(repo)
        .output()
        .context("run git rev-list")?;
    if !out.status.success() {
        return Err(anyhow!("git rev-list failed in {}", repo.display()));
    }
    let sha = String::from_utf8_lossy(&out.stdout)
        .lines()
        .last()
        .unwrap_or("")
        .trim()
        .to_string();
    if sha.is_empty() {
        return Err(anyhow!("no commits in {}", repo.display()));
    }
    Ok(sha)
}

pub fn soul_for(repo: &Path) -> Result<Soul> {
    let path = repo
        .canonicalize()
        .with_context(|| format!("soul repo {} does not exist", repo.display()))?;
    let sha = genesis_sha(&path)?;
    if sha.len() < SOUL_ID_LEN {
        return Err(anyhow!("genesis sha too short in {}", path.display()));
    }
    Ok(Soul {
        id: sha[..SOUL_ID_LEN].to_string(),
        path,
    })
}

/// Resolve every configured path. Returns the souls that resolved and one
/// line per path that did not, or that collided with another.
pub fn build(paths: &[PathBuf]) -> (Vec<Soul>, Vec<String>) {
    let mut souls = Vec::new();
    let mut problems = Vec::new();
    let mut seen: HashMap<String, PathBuf> = HashMap::new();
    for p in paths {
        match soul_for(p) {
            Ok(s) => {
                if let Some(prev) = seen.get(&s.id) {
                    problems.push(format!(
                        "souls {} and {} resolve to the same id {} — the second is skipped",
                        prev.display(),
                        s.path.display(),
                        s.id
                    ));
                    continue;
                }
                seen.insert(s.id.clone(), s.path.clone());
                souls.push(s);
            }
            Err(e) => problems.push(format!("{}: {e:#}", p.display())),
        }
    }
    (souls, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ravel-registry-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q"]);
        std::fs::write(dir.join("a"), name).unwrap();
        run(&["add", "a"]);
        run(&["commit", "-q", "-m", "genesis", "--no-gpg-sign"]);
        dir
    }

    #[test]
    fn id_is_first_six_of_genesis() {
        let dir = fresh_repo("six");
        let s = soul_for(&dir).unwrap();
        assert_eq!(s.id.len(), SOUL_ID_LEN);
        let full = genesis_sha(&dir).unwrap();
        assert!(full.starts_with(&s.id));
        assert_eq!(s.path, dir.canonicalize().unwrap());
    }

    #[test]
    fn repo_yml_genesis_wins_over_git() {
        let dir = fresh_repo("yml");
        std::fs::create_dir_all(dir.join(".lex")).unwrap();
        std::fs::write(
            dir.join(".lex/repo.yml"),
            "genesis_sha: \"abcdef0123456789\"\n",
        )
        .unwrap();
        assert_eq!(soul_for(&dir).unwrap().id, "abcdef");
    }

    #[test]
    fn missing_path_is_a_named_problem_not_a_stop() {
        let good = fresh_repo("good");
        let (souls, problems) = build(&[PathBuf::from("/nonexistent/soul"), good.clone()]);
        assert_eq!(souls.len(), 1);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("/nonexistent/soul"));
    }

    #[test]
    fn duplicate_id_skips_the_second() {
        let dir = fresh_repo("dup");
        let (souls, problems) = build(&[dir.clone(), dir.clone()]);
        assert_eq!(souls.len(), 1);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("same id"));
    }
}
