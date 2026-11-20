//! What a "soul" means to ravel, in one place.
//!
//! One soul repo owns one ravel store. Whose store you ask is the soul
//! scope, and a federating reader (Syrinx) asks stores in place; two souls'
//! graphs are never mixed in one store.
//!
//! Every address in a store starts with that soul's root,
//! `https://repolex.ai/{squad}/{user}/`, the same root git-lex gives it
//! (address ruling by goodlux, 2026-11-19). [`crate::names::Names`] builds
//! every one of them. Before 0.10.0 all souls shared
//! `https://repolex.ai/ravel/…`, and before that an opaque
//! `urn:soul:<genesis>:…`; [`crate::readdress`] moves a store off the former.
//!
//! The join with another engine's store rides the shared root, the time
//! anchor (`xsd:dateTime`, RFC 3339, identical on both sides), and explicit
//! edges that name Turn addresses.
