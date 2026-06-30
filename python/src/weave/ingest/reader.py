"""The ingester proper: raw JSONL -> filtered prose stream of PoseRecords.

Layered, each layer independently testable and auditable:

  1. iter_raw_lines       — parse JSONL, tolerate broken lines (byte-faithful read,
                            no mutation; the source stays canonical).
  2. extract_prose_blocks — STRUCTURAL filter: keep user/assistant `text` blocks,
                            drop tool_use / tool_result / system / attachment / etc.
  3. strip_harness_wrappers — CONTENT filter (opt-in): remove harness-injected
                            <channel>, <command>, <system-reminder>, Caveat: noise
                            that the human/assistant never authored.
  4. load                 — the public entry: 1->2 (+optionally 3), assign `seq`,
                            emit PoseRecords; supports full-load and append modes.

Nothing here imports a model or a heavy dep. The ingester is pure-Python,
hook-safe, and is the substrate every reader (general or specific) runs on.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator, Optional


# --- the record a reader consumes -------------------------------------------

# `voice` uses the signal-contract vocabulary (human | assistant), NOT the raw
# JSONL `role` (user | assistant) — because a "user" line in the transcript is
# often a tool_result, not the human. After filtering, role==user means the human
# really spoke, so we rename to the truer term at the seam.
HUMAN = "human"
ASSISTANT = "assistant"


@dataclass(frozen=True, slots=True)
class PoseRecord:
    """One prose-bearing turn, the unit every reader anchors to.

    Field names match the signal contract's anchor fields so a reader can emit
    a monolog row that joins straight back here:

        {"reader": "...", "anchor": {"seq": rec.seq}, "signal": {...}}
    """

    seq: int           # 0-based position in the FILTERED prose stream (the anchor)
    voice: str         # HUMAN | ASSISTANT
    text: str          # the prose, harness-wrappers optionally stripped
    src_uuid: str      # uuid of the source JSONL line (joins to the Raw mirror)
    turn_id: str       # parentUuid -> uuid threading id (for turn-pair grouping)
    ts: Optional[str]  # ISO timestamp from the source line, if present

    def __repr__(self) -> str:  # compact, debugging-friendly
        head = self.text[:60].replace("\n", " ")
        return f"PoseRecord(seq={self.seq} {self.voice} {head!r})"


# --- layer 1: byte-faithful raw read ----------------------------------------

def iter_raw_lines(path: str | Path) -> Iterator[dict]:
    """Yield each JSONL object. Broken lines are skipped, not fatal.

    The Raw mirror is occasionally written mid-flush; a half-written trailing
    line must not crash a tail. We never mutate the source — this only reads.
    """
    p = Path(path)
    with p.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                # tolerate a torn trailing line (append-mode tail mid-write)
                continue


# --- layer 2: structural filter (the primary noise cut) ---------------------

# Line-types that carry a conversational message. Everything else (system,
# attachment, file-history-snapshot, permission-mode, last-prompt, ai-title,
# queue-operation) is harness bookkeeping and is dropped wholesale.
_PROSE_LINE_TYPES = {"user", "assistant"}


def _blocks_of(message: dict) -> list:
    """Normalize a message's content to a list of blocks.

    Content is either a bare string (older/simple turns) or a list of typed
    blocks (text / tool_use / tool_result / image / ...).
    """
    content = message.get("content")
    if isinstance(content, str):
        return [{"type": "text", "text": content}]
    if isinstance(content, list):
        return content
    return []


def extract_prose_blocks(obj: dict) -> list[dict]:
    """STRUCTURAL filter for one raw line.

    Returns a list of normalized prose blocks (possibly empty):
        {"voice": HUMAN|ASSISTANT, "text": str, "src_uuid", "turn_id", "ts"}

    Drops: every non-user/assistant line, and within those every non-`text`
    block (tool_use, tool_result, image, thinking, ...). A "user" role maps to
    HUMAN only after the tool_result blocks are gone — by this point a remaining
    text block really is the human speaking.
    """
    if obj.get("type") not in _PROSE_LINE_TYPES:
        return []
    message = obj.get("message") or {}
    role = message.get("role")
    voice = HUMAN if role == "user" else ASSISTANT if role == "assistant" else None
    if voice is None:
        return []

    src_uuid = obj.get("uuid", "")
    turn_id = obj.get("parentUuid") or src_uuid  # thread anchor for pairing
    ts = obj.get("timestamp")

    out: list[dict] = []
    for block in _blocks_of(message):
        if not isinstance(block, dict):
            continue
        if block.get("type") != "text":
            continue  # drop tool_use / tool_result / image / thinking / ...
        text = (block.get("text") or "").strip()
        if not text:
            continue
        out.append(
            {"voice": voice, "text": text, "src_uuid": src_uuid,
             "turn_id": turn_id, "ts": ts}
        )
    return out


# --- layer 3: content filter (opt-in harness-wrapper scrub) ------------------

# Harness/MCP-injected blocks that the human/assistant never authored. These show
# up INSIDE user text and would otherwise be read as prose. Each pattern is a
# whole-block remover; what's left is the actual authored prose. Auditable: this
# pass is off by default in load(); callers opt in via strip_wrappers=True.
_WRAPPER_PATTERNS = [
    # <channel source="..."> ... </channel>  (subtext peer messages)
    re.compile(r"<channel\b[^>]*>.*?</channel>", re.DOTALL | re.IGNORECASE),
    # <system-reminder> ... </system-reminder>
    re.compile(r"<system-reminder>.*?</system-reminder>", re.DOTALL | re.IGNORECASE),
    # <command-name>...</command-name>, <command-message>, <command-args>, <local-command-*>
    re.compile(r"</?(?:local-)?command[\w-]*>", re.IGNORECASE),
    # <local-command-caveat>...</local-command-caveat>
    re.compile(r"<local-command-caveat>.*?</local-command-caveat>", re.DOTALL | re.IGNORECASE),
    # leading "Caveat: ..." preamble line the harness prepends to local-command output
    re.compile(r"^Caveat:.*$", re.MULTILINE),
]


def strip_harness_wrappers(text: str) -> str:
    """Remove harness/MCP-injected wrappers, leaving only authored prose.

    Returns the scrubbed text (may be empty if the block was *entirely* an
    injected wrapper — e.g. a turn that was nothing but an incoming peer message).
    Callers should drop empty results.
    """
    for pat in _WRAPPER_PATTERNS:
        text = pat.sub("", text)
    return text.strip()


# --- layer 4: public entry --------------------------------------------------

def load(
    path: str | Path,
    *,
    after: Optional[str] = None,
    strip_wrappers: bool = False,
) -> Iterator[PoseRecord]:
    """Full-load or append-incoming, yielding filtered PoseRecords.

    Args:
      path:           the Raw-mirror JSONL.
      after:          if given, an `src_uuid`; skip every prose block up to and
                      including that line, yield only what came after (tail/append
                      mode). If the uuid is never found, the whole file is yielded
                      rather than silently dropping it — an unknown anchor must not
                      cause silent data loss (the skipped blocks are held until the
                      anchor is confirmed; if it never arrives they are emitted).
      strip_wrappers: opt into the content filter (layer 3). When True, blocks
                      that are entirely harness-wrappers are dropped.

    `seq` is assigned over the FILTERED stream and is stable for a given file
    content + filter setting: it is the anchor readers join on. NOTE: enabling
    strip_wrappers can drop wholly-injected blocks, which shifts seq vs the
    unstripped stream — a reader-set must agree on one filter setting per corpus
    so anchors line up across readers. (Documented in the taxonomy doc.)
    """
    def _emit(seq: int, block: dict) -> Optional[PoseRecord]:
        text = block["text"]
        if strip_wrappers:
            text = strip_harness_wrappers(text)
            if not text:
                return None  # block was entirely an injected wrapper
        return PoseRecord(
            seq=seq,
            voice=block["voice"],
            text=text,
            src_uuid=block["src_uuid"],
            turn_id=block["turn_id"],
            ts=block["ts"],
        )

    seq = 0
    skipping = after is not None
    held: list[dict] = []  # blocks skipped before the anchor was confirmed
    for obj in iter_raw_lines(path):
        for block in extract_prose_blocks(obj):
            if skipping:
                # still fast-forwarding to the `after` anchor; hold the block in
                # case the anchor is never found (then we owe the whole file).
                held.append(block)
                if block["src_uuid"] == after:
                    skipping = False
                    held.clear()  # anchor confirmed — the held blocks are truly behind us
                continue
            rec = _emit(seq, block)
            if rec is not None:
                yield rec
                seq += 1

    if skipping and held:
        # the anchor was never found: emit the whole file instead of nothing,
        # so an unknown/stale anchor degrades to a full-load, never to silence.
        for block in held:
            rec = _emit(seq, block)
            if rec is not None:
                yield rec
                seq += 1
