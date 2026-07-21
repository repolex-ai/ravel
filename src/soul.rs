//! Ravel SOUL-ADAPTER — the one thin layer that knows what a "soul" is.
//!
//! The engine (`project`, `annotate`, `graph`) is partition-key-agnostic: it
//! takes an opaque `partition` string and never learns what it means. THIS
//! module is where the partition is chosen, so that choice lives in exactly
//! one place.
//!
//! ## Soul scoping is BY STORE, not by subject (git-lex Day-50 ruling)
//! Subjects carry NO soul identity. One soul repo owns one ravel store; whose
//! store you query IS the soul scope, and the per-soul query distributor
//! federates stores in place — it never co-mingles two souls' graphs. The old
//! partition (`urn:soul:<genesis_sha>:Ravel/Turn/`) baked the soul into every
//! subject via an opaque `urn:` — the exact pattern git-lex eradicated
//! stack-wide in favor of resolvable https IRIs (see git-lex
//! `src/nquad.rs` "no soul identity in the subject"). Turn subjects are now:
//!
//!   `https://repolex.ai/ravel/Turn/<event_id>`
//!
//! following git-lex's machinery shape (`…/git-lex/SpoEvent/<id>`,
//! `…/git-lex/NamedGraph/<name>`): instance data under the product path,
//! vocabulary under `…/ontology/ravel#` — never mixed.
//!
//! ## The federation join (contract w/ Pool, re-based on this ruling)
//! Ravel and Pool never co-store; the cross-store join no longer rides a
//! shared subject prefix. It rides (a) store scoping — the distributor asks
//! one soul's ravel store and the same soul's Pool store — plus (b) the TIME
//! anchor (`xsd:dateTime`, RFC-3339 lexical form, identical on both sides)
//! and (c) explicit prov edges naming Turn IRIs. The genesis-sha resolution
//! this module used to carry (mirroring `pool/src/soul_pool.rs`) left with
//! the urn — soul identity lives in `.lex/identity.yml` and souls.toml,
//! consulted by whoever ROUTES to a store, never stamped inside one.

/// The Turn partition prefix, identical for every soul: appended with an
/// `event_id` by the engine's `event_iri`, it yields the Turn subject
/// `https://repolex.ai/ravel/Turn/<event_id>`.
pub const TURN_PARTITION: &str = "https://repolex.ai/ravel/Turn/";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RAVEL_NS;

    /// The partition is a resolvable https IRI prefix with NO soul identity
    /// and NO `urn:` — soul scoping is the store, not the subject.
    #[test]
    fn partition_is_https_and_carries_no_soul_identity() {
        assert!(TURN_PARTITION.starts_with("https://repolex.ai/ravel/"));
        assert!(!TURN_PARTITION.contains("urn:"));
        // and the event-id appends cleanly to a full turn subject
        let turn_subject = format!("{TURN_PARTITION}0f8a1c2d-uuid");
        assert_eq!(turn_subject, "https://repolex.ai/ravel/Turn/0f8a1c2d-uuid");
    }

    /// Instance IRIs live under the product path, never inside the ontology
    /// namespace — vocabulary and data must not share a prefix.
    #[test]
    fn partition_is_not_inside_the_ontology_namespace() {
        assert!(!TURN_PARTITION.starts_with(RAVEL_NS));
    }
}
