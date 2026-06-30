"""Weave — the semantic-wave engine.

A document is a meaning waveform; each new word is a diff that deforms the running
shape, and the deformation is the signal. The wave op is the same over any shape,
so the engine rides a pluggable shape-provider interface (see `weave.shape`).

This is skeleton-only as of Day 26 (2026-06-13). Build order: ingest →
wave-over-shape → shape arms (embedding first, then AMR). See docs/.
"""

__version__ = "0.1.0"
