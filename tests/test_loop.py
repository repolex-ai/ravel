"""End-to-end loop test: ingest -> emojikey reader -> monolog.

Proves the reader-set loop the way the recall hook proved corpus-is-the-store:
prose goes in, a greppable signal lands in the monolog, joinable back to source
by anchor. No model, no GPU.
"""

from __future__ import annotations

import json

from weave.ingest import load
from weave.monolog import Monolog
from weave.readers import EmojikeyReader
from weave.readers.emojikey import _KEY


# --- the regex itself -------------------------------------------------------

def test_key_matches_canonical_shape():
    t = "wrap up 🐐 [ME|🧠🎨8∠45]~[CONTENT|💻🧩9∠15]~[YOU|🎓🌱8∠35] done"
    m = _KEY.search(t)
    assert m is not None
    assert m.group("me") == "🧠🎨8∠45"
    assert m.group("content") == "💻🧩9∠15"
    assert m.group("you") == "🎓🌱8∠35"


def test_key_case_insensitive_and_minimal():
    t = "[me|🐐]~[content|⚙️🌊]~[you|🤝]"
    m = _KEY.search(t)
    assert m is not None
    assert m.group("me") == "🐐"


def test_key_ignores_ordinary_brackets():
    assert _KEY.search("see file.py:[12] and the [TODO] list") is None
    # right labels, wrong order -> not a key
    assert _KEY.search("[YOU|a]~[CONTENT|b]~[ME|c]") is None


def test_multiple_keys_in_one_text():
    t = "[ME|a]~[CONTENT|b]~[YOU|c] ... later ... [ME|x]~[CONTENT|y]~[YOU|z]"
    assert len(list(_KEY.finditer(t))) == 2


# --- the full loop ----------------------------------------------------------

def _make_transcript(tmp_path, lines):
    p = tmp_path / "session.jsonl"
    p.write_text("\n".join(json.dumps(o) for o in lines) + "\n", encoding="utf-8")
    return p


def test_ingest_read_monolog_roundtrip(tmp_path):
    transcript = _make_transcript(tmp_path, [
        {"type": "user", "uuid": "u1",
         "message": {"role": "user", "content": "how's it going?"}},
        {"type": "assistant", "uuid": "a1", "parentUuid": "u1",
         "message": {"role": "assistant", "content": [
             {"type": "text",
              "text": "going great! [ME|🧠8]~[CONTENT|💻9]~[YOU|🎓8] shipping now"},
             {"type": "tool_use", "name": "Bash", "input": {}},
         ]}},
        {"type": "assistant", "uuid": "a2", "parentUuid": "u1",
         "message": {"role": "assistant", "content": "no key in this turn"}},
    ])

    # ingest -> read -> monolog
    records = list(load(transcript))
    reader = EmojikeyReader()
    mono = Monolog.for_transcript(transcript)
    for row in reader.read(records):
        mono.append(row)
    written = mono.flush()

    assert written == 1  # exactly one key, in the assistant text block

    # the monolog file exists alongside the transcript, joinable by anchor
    rows = list(mono.read())
    assert len(rows) == 1
    row = rows[0]
    assert row.reader == "emojikey"
    assert row.signal["me"] == "🧠8"
    assert row.signal["voice"] == "assistant"

    # anchor joins back to the SOURCE: the seq points at the real prose record
    anchored = records[row.anchor["seq"]]
    assert "[ME|🧠8]" in anchored.text
    assert anchored.src_uuid == row.anchor["src_uuid"] == "a1"


def test_monolog_path_is_sidecar(tmp_path):
    transcript = _make_transcript(tmp_path, [
        {"type": "user", "uuid": "u1",
         "message": {"role": "user", "content": "hi"}},
    ])
    mono = Monolog.for_transcript(transcript)
    assert mono.path.name == "session.monolog.jsonl"
    assert mono.path.parent == transcript.parent  # parallel to the source


def test_monolog_excludes_render_back_edge(tmp_path):
    """A reader filters reader=='render' so it never reads the viz echo."""
    transcript = _make_transcript(tmp_path, [
        {"type": "user", "uuid": "u1",
         "message": {"role": "user", "content": "hi"}},
    ])
    mono = Monolog.for_transcript(transcript)
    mono.emit("emojikey", {"seq": 0}, {"raw": "[ME|a]~[CONTENT|b]~[YOU|c]"})
    mono.emit("render", {"seq": 0}, {"shown": True})
    mono.flush()

    # default read drops the render row (loop-safety)
    assert len(list(mono.read())) == 1
    # but it's on disk if you ask for it explicitly
    assert len(list(mono.read(exclude_render=False))) == 2
