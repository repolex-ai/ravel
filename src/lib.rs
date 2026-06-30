//! Weave — transcript-graph engine (Rust core).
//!
//! Day-39 spike scope: prove the loop end-to-end on real data —
//!   transcript JSONL → project to RDF 1.2 → load into oxigraph →
//!   one SPARQL CONSTRUCT detector mints an annotation as a triple term →
//!   round-trip it back out.
//!
//! Engine/adapter line (locked w/ w4r3z): the ENGINE knows `Event`/`Turn` and a
//! generic `partition` string. It does NOT know what a "soul" is. The
//! claude-code ADAPTER (`adapter` mod) parses the dialect and supplies the
//! partition. The soul-anchor `urn:soul:<sha>:` is just what an adapter chooses
//! to pass as `partition` — the engine is invariant under it.

pub mod adapter;
pub mod project;

/// A generic transcript event — the engine's unit. Dialect-agnostic.
#[derive(Debug, Clone)]
pub struct Event {
    /// Stable id (the idempotency key — w4r3z: key on this, not file mtime).
    pub event_id: String,
    /// Parent event id (the reply-to spine). None at a root / compaction break.
    pub parent_id: Option<String>,
    /// "user" | "assistant" | other dialect role; the engine treats it opaquely.
    pub role: String,
    /// RFC-3339 timestamp (federation anchor lexical form — matches Pool).
    pub timestamp: Option<String>,
    /// The text content of this event, if any (concatenated text blocks).
    pub text: Option<String>,
}

/// The engine namespace for the weave ontology.
pub const WEAVE_NS: &str = "https://repolex.ai/ontology/weave#";
