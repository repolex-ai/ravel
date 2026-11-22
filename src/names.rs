//! Every address ravel stores, in one place.
//!
//! An address starts with the store's root, `https://repolex.ai/{squad}/{user}/`,
//! the same root git-lex gives that soul (address ruling by goodlux,
//! 2026-11-19; named-graph spec section 12.1). ravel's own things sit under
//! `ravel/` below it:
//!
//!   `https://repolex.ai/7r1pl3f0rc3/spacegoat/ravel/Turn/<event id>`
//!   `https://repolex.ai/7r1pl3f0rc3/spacegoat/ravel/NamedGraph/<transcript id>`
//!
//! `squad` and `username` are read from the soul's `.lex/repo.yml`, which
//! git-lex writes. A repo that has neither gets the bare root
//! `https://repolex.ai/`, exactly as gitlexd reports it; a repo with one and
//! not the other is refused, because half a root is a wrong address.
//!
//! Vocabulary is not here: classes and properties stay under
//! `https://repolex.ai/ontology/ravel/` ([`crate::RAVEL_NS`]) for every soul.

use anyhow::{anyhow, bail, Result};
use std::path::Path;

/// Where every address began before the root carried a squad and a user.
/// Also the root of a repo that has no squad and no username.
pub const BARE_ROOT: &str = "https://repolex.ai/";

/// The memory graph's id. `memory-v1` until 0.10.0.
pub const MEMORY_GRAPH: &str = "optmem";
/// The id of the graph that lists the newest turns ([`crate::graph::keep_recent`]).
pub const RECENT_GRAPH: &str = "recent";
/// What the memory graph was called before; cleared wherever it is found.
pub const OLD_MEMORY_GRAPH: &str = "memory-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    root: String,
    /// `(squad, username)` when the repo has them.
    who: Option<(String, String)>,
}

fn slug(s: &str) -> bool {
    !s.is_empty()
        && s.split('-').all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn yml_value(raw: &str, key: &str) -> Option<String> {
    raw.lines().find_map(|line| {
        let v = line.strip_prefix(key)?.strip_prefix(':')?;
        let v = v.trim().trim_matches('"').trim_matches('\'');
        (!v.is_empty()).then(|| v.to_string())
    })
}

impl Names {
    /// For a soul with a squad and a username.
    pub fn new(squad: &str, username: &str) -> Result<Self> {
        if !slug(squad) || !slug(username) {
            bail!(
                "squad {squad:?} and username {username:?} must be lowercase letters, \
                 digits and single dashes: they become part of every address"
            );
        }
        Ok(Self {
            root: format!("{BARE_ROOT}{squad}/{username}/"),
            who: Some((squad.to_string(), username.to_string())),
        })
    }

    /// For a repo with no squad and no username; the addresses ravel has
    /// always written.
    pub fn bare() -> Self {
        Self {
            root: BARE_ROOT.to_string(),
            who: None,
        }
    }

    /// Read the soul's `.lex/repo.yml`.
    pub fn for_soul(repo: &Path) -> Result<Self> {
        let file = repo.join(".lex").join("repo.yml");
        let raw = std::fs::read_to_string(&file).unwrap_or_default();
        match (yml_value(&raw, "squad"), yml_value(&raw, "username")) {
            (Some(s), Some(u)) => {
                Self::new(&s, &u).map_err(|e| anyhow!("{}: {e}", file.display()))
            }
            (None, None) => Ok(Self::bare()),
            (s, u) => bail!(
                "{} has squad {s:?} and username {u:?}; it needs both or neither. \
                 `git lex kit-update` in that repo writes them",
                file.display()
            ),
        }
    }

    /// `https://repolex.ai/{squad}/{user}/`: what an outside query is parsed
    /// against, and what `GET /souls` reports.
    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn squad(&self) -> Option<&str> {
        self.who.as_ref().map(|w| w.0.as_str())
    }

    pub fn username(&self) -> Option<&str> {
        self.who.as_ref().map(|w| w.1.as_str())
    }

    /// `{root}ravel/`: every instance ravel names sits below this.
    pub fn base(&self) -> String {
        format!("{}ravel/", self.root)
    }

    /// `{root}ravel/Turn/`: a turn's address is this plus its event id.
    pub fn turn_partition(&self) -> String {
        format!("{}ravel/Turn/", self.root)
    }

    pub fn turn(&self, event_id: &str) -> String {
        format!("{}ravel/Turn/{event_id}", self.root)
    }

    pub fn memory(&self, id: &str) -> String {
        format!("{}ravel/Memory/{id}", self.root)
    }

    /// `{root}ravel/NamedGraph/`.
    pub fn graph_prefix(&self) -> String {
        format!("{}ravel/NamedGraph/", self.root)
    }

    /// The named graph one transcript lives in. `id` is already safe for an
    /// address; [`crate::graph::transcript_graph_iri`] makes it so.
    pub fn graph(&self, id: &str) -> String {
        format!("{}ravel/NamedGraph/{id}", self.root)
    }

    pub fn memory_graph(&self) -> String {
        self.graph(MEMORY_GRAPH)
    }

    /// The graph that lists the store's newest turns.
    pub fn recent_graph(&self) -> String {
        self.graph(RECENT_GRAPH)
    }

    /// The soul that owns this store, as git-lex names it:
    /// `{root}soul/Soul/{user}`. None for a repo with no username; such a
    /// store's graphs are described without an owner.
    pub fn owner(&self) -> Option<String> {
        self.username()
            .map(|u| format!("{}soul/Soul/{u}", self.root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_soul_with_a_squad_and_a_username_gets_the_ruled_addresses() {
        let n = Names::new("7r1pl3f0rc3", "spacegoat").unwrap();
        assert_eq!(n.root(), "https://repolex.ai/7r1pl3f0rc3/spacegoat/");
        assert_eq!(
            n.turn("abc"),
            "https://repolex.ai/7r1pl3f0rc3/spacegoat/ravel/Turn/abc"
        );
        assert_eq!(
            n.memory_graph(),
            "https://repolex.ai/7r1pl3f0rc3/spacegoat/ravel/NamedGraph/optmem"
        );
        assert_eq!(
            n.owner().as_deref(),
            Some("https://repolex.ai/7r1pl3f0rc3/spacegoat/soul/Soul/spacegoat")
        );
        // Vocabulary and data never share a prefix.
        assert!(!n.base().starts_with(crate::RAVEL_NS));
    }

    #[test]
    fn a_repo_with_neither_keeps_the_addresses_it_always_had() {
        let n = Names::bare();
        assert_eq!(n.turn("abc"), "https://repolex.ai/ravel/Turn/abc");
        assert_eq!(n.owner(), None);
    }

    #[test]
    fn repo_yml_is_read_and_half_a_root_is_refused() {
        let dir = std::env::temp_dir().join(format!("ravel-names-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".lex")).unwrap();
        let yml = dir.join(".lex").join("repo.yml");

        std::fs::write(&yml, "name: X\nsquad: 7r1pl3f0rc3\nusername: \"m4xx\"\n").unwrap();
        assert_eq!(
            Names::for_soul(&dir).unwrap().root(),
            "https://repolex.ai/7r1pl3f0rc3/m4xx/"
        );

        std::fs::write(&yml, "name: X\n").unwrap();
        assert_eq!(Names::for_soul(&dir).unwrap(), Names::bare());

        std::fs::write(&yml, "name: X\nsquad: 7r1pl3f0rc3\n").unwrap();
        assert!(Names::for_soul(&dir).is_err());

        std::fs::write(&yml, "squad: Bad Squad\nusername: m4xx\n").unwrap();
        assert!(Names::for_soul(&dir).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
