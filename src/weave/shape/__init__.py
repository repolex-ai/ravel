"""Shape providers — the pluggable seam the wave engine rides.

The wave operation ("how much did this token deform the running structure") is the
same over any shape. So the engine never knows whether it's riding an
embedding-trajectory, an AMR graph, or (later) an SLG graph — it only knows the
ShapeProvider protocol below. Which shape makes the meaning-wave most legible is an
empirical calibration arm, NOT an assumption baked into the engine.

Three arms, none blocking on the others:
  - embedding-trajectory : no external dependency, the baseline shape (build first)
  - amr-graph            : public Python parser (amrlib/SPRING), simplify-then-wave
  - slg-graph            : drop-in later if/when a text->logic-graph transform exists

Skeleton-only (Day 26): the protocol is declared so v0 is built against an
interface, not a concrete shape. No arm is implemented yet.
"""

from __future__ import annotations

from typing import Protocol, runtime_checkable


@runtime_checkable
class ShapeProvider(Protocol):
    """A running structure that grows and deforms as tokens arrive.

    The contract is deliberately minimal: ingest a token, and report how much it
    deformed the structure. Everything the wave engine needs rides these two ops.
    The provider OWNS its internal representation (a trajectory, a graph, ...); the
    engine never inspects it directly except via `snapshot` (post-v0).
    """

    def push(self, token: str, *, seq: int) -> "Deformation":
        """Fold one token into the running shape; return how it deformed it.

        `seq` is the global prose-stream position (matches the signal contract's
        `seq`). The returned Deformation is what the wave math reads to produce the
        per-word signal legs.
        """
        ...

    def snapshot(self, seq: int) -> object:
        """Return a representation of the shape AS OF `seq` (post-v0 affordance).

        v0 does not call this, but the provider keeps its shape-state snapshot-able
        from day one so the deform-widget (shape_at) is a thin wrapper, not a
        refactor. See docs/2026_06_13_SIGNAL_CONTRACT.md (cold path II).
        """
        ...


class Deformation:
    """How much one token deformed the running shape.

    Raw, pre-signal: the wave math turns these into the contract's named legs
    (returnStrength / novelty / turbulence). Kept separate from the signal so the
    same Deformation can feed different leg formulations during calibration.

    Skeleton — fields are placeholders to be set by the math, not final.
    """

    __slots__ = ("seq",)

    def __init__(self, seq: int) -> None:
        self.seq = seq
