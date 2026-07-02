//! Weave SOUL-ADAPTER — the one thin layer that knows what a "soul" is.
//!
//! The engine (`project`, `annotate`, `graph`) is partition-key-agnostic: it
//! takes an opaque `partition` string and never learns it means "soul". THIS
//! module is where the soul-anchor is minted, so the engine stays clean and the
//! federation seam lives in exactly one place.
//!
//! ## The federation contract (locked w/ w4r3z, Day 38 — soul repo memory
//! `2026-06-29-weave-pool-federation-anchor-contract`)
//! Weave and Pool never co-store; they cross-join on anchors stamped IDENTICALLY
//! on both sides. The soul anchor is the load-bearing one:
//!
//!   Pool  Moment subject = `urn:soul:<genesis_sha>:Copia/Moment/<file>`
//!   Weave Turn   subject = `urn:soul:<genesis_sha>:Weave/Turn/<event_id>`
//!
//! The `<genesis_sha>` MUST be the SAME string on both sides, resolved through
//! the SAME mechanism — else two soul-id spaces LOOK joinable but silently
//! aren't (w4r3z gotcha #3, the federation-critical one).
//!
//! ## Same mechanism, verified against live Pool source
//! Pool derives the sha in `pool/src/soul_pool.rs::read_soul_genesis_sha`: it
//! reads `<soul_repo>/.lex/identity.yml` and takes the `genesis_sha:` line. NOT
//! `git rev-list --max-parents=0` — the sha is a value `git lex sync` PINS into
//! identity.yml (write-once), so recomputing from git history would diverge the
//! day a soul's history is ever rewritten. We mirror Pool's file+parse EXACTLY
//! (same path, same field, same validation: non-empty, ≤40 chars, all hex).
//!
//! This code is deliberately DUPLICATED from Pool, not shared: each engine
//! depends on no sibling's code and federates only by the wire-shape. Mirroring
//! the ~15 lines is the price of sovereignty; a shared crate would couple them.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

/// Read a soul repo's pinned genesis SHA from `.lex/identity.yml`.
///
/// Mirrors `pool/src/soul_pool.rs::read_soul_genesis_sha` byte-for-byte in
/// behaviour so Weave and Pool resolve the SAME soul to the SAME sha. Do NOT
/// "improve" this to compute the sha from git — the pinned file is canonical.
pub fn read_genesis_sha(soul_repo: &Path) -> Result<String> {
    let identity = soul_repo.join(".lex").join("identity.yml");
    let body = std::fs::read_to_string(&identity)
        .with_context(|| format!("read soul identity {}", identity.display()))?;
    for line in body.lines() {
        if let Some(rest) = line.trim().strip_prefix("genesis_sha:") {
            let sha = rest.trim();
            // Identical validation to Pool: non-empty, ≤40 hex chars.
            if !sha.is_empty() && sha.len() <= 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
                return Ok(sha.to_string());
            }
        }
    }
    Err(anyhow!(
        "no valid `genesis_sha:` line in {} — run `git lex sync` to create it",
        identity.display()
    ))
}

/// The Weave partition prefix for a soul: `urn:soul:<sha>:Weave/Turn/`.
///
/// Appended with an `event_id` by the engine's `event_iri`, this yields the
/// federation-shaped Turn subject `urn:soul:<sha>:Weave/Turn/<event_id>` that
/// shares the `urn:soul:<sha>:` prefix with Pool's Moment subjects.
pub fn turn_partition(genesis_sha: &str) -> String {
    format!("urn:soul:{genesis_sha}:Weave/Turn/")
}

/// Resolve a soul repo path straight to its Weave Turn partition string —
/// the one call an adapter/binary makes to replace a demo partition.
pub fn soul_partition(soul_repo: &Path) -> Result<String> {
    Ok(turn_partition(&read_genesis_sha(soul_repo)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// The partition is contract-shaped: `urn:soul:<sha>:Weave/Turn/`, and it
    /// shares the `urn:soul:<sha>:` prefix a Pool Moment subject carries — the
    /// whole point of the federation seam.
    #[test]
    fn partition_is_federation_shaped_and_shares_pool_prefix() {
        let sha = "9bdf2afa2a49bfac1b6d6ff7f5f3ab34f04ed44b";
        let part = turn_partition(sha);
        assert_eq!(part, "urn:soul:9bdf2afa2a49bfac1b6d6ff7f5f3ab34f04ed44b:Weave/Turn/");
        // A Pool Moment subject for the same soul:
        let pool_moment = format!("urn:soul:{sha}:Copia/Moment/render-123.json");
        let common = format!("urn:soul:{sha}:");
        assert!(part.starts_with(&common), "Weave turn must carry the soul prefix");
        assert!(pool_moment.starts_with(&common), "Pool moment carries the same prefix");
        // and the event-id appends cleanly to a full turn subject
        let turn_subject = format!("{part}0f8a1c2d-uuid");
        assert!(turn_subject.starts_with(&common));
    }

    /// Reads the pinned sha from a real `.lex/identity.yml`, mirroring Pool.
    #[test]
    fn reads_pinned_genesis_from_identity_yml() {
        let dir = std::env::temp_dir().join("weave-soul-test-ok");
        let lex = dir.join(".lex");
        fs::create_dir_all(&lex).unwrap();
        fs::write(
            lex.join("identity.yml"),
            "# WRITE ONCE. NEVER MODIFY.\ngenesis_sha: 9bdf2afa2a49bfac1b6d6ff7f5f3ab34f04ed44b\n",
        )
        .unwrap();
        let sha = read_genesis_sha(&dir).unwrap();
        assert_eq!(sha, "9bdf2afa2a49bfac1b6d6ff7f5f3ab34f04ed44b");
        fs::remove_dir_all(&dir).ok();
    }

    /// A non-hex / empty genesis line is REFUSED, not silently accepted — a
    /// bad sha would forge a partition that joins nothing.
    #[test]
    fn rejects_missing_or_malformed_genesis() {
        let dir = std::env::temp_dir().join("weave-soul-test-bad");
        let lex = dir.join(".lex");
        fs::create_dir_all(&lex).unwrap();
        fs::write(lex.join("identity.yml"), "genesis_sha: not-a-sha!!\n").unwrap();
        assert!(read_genesis_sha(&dir).is_err(), "malformed sha must be refused");
        fs::write(lex.join("identity.yml"), "# no genesis line here\n").unwrap();
        assert!(read_genesis_sha(&dir).is_err(), "missing genesis line must be refused");
        fs::remove_dir_all(&dir).ok();
    }
}
