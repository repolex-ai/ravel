"""Cadence reader tests: structural facts + relational rhythm, no model.

Mirrors test_loop.py's discipline — unit-test the cheap helpers in isolation, then
prove the full ingest -> cadence -> monolog loop and that anchors join back to
source. Cadence's twist over emojikey is the RELATIONAL facts (latency, voice
switch), so those get their own targeted cases.
"""

from __future__ import annotations

import json

from weave.ingest import load
from weave.monolog import Monolog
from weave.readers import CadenceReader
from weave.readers.cadence import (
    _code_block_ratio,
    _parse_ts,
    _question_density,
)


# --- the cheap helpers in isolation -----------------------------------------

def test_question_density_none_some_all():
    assert _question_density("a flat statement.") == 0.0
    assert _question_density("really? yes.") == 0.5  # 1 question / 2 sentences
    assert _question_density("why? how? when?") == 1.0  # all questions
    assert _question_density("") == 0.0


def test_question_density_unterminated_fragment_has_denominator():
    # no sentence terminator -> denominator floors at 1, not divide-by-zero
    assert _question_density("is this a question") == 0.0
    assert _question_density("is this a question?") == 1.0


def test_code_block_ratio_fenced_and_inline():
    assert _code_block_ratio("just talking, no code here.") == 0.0
    # a pure fenced block is ~all code
    only_code = "```\nx = 1\n```"
    assert _code_block_ratio(only_code) == 1.0
    # mixed: inline span is a fraction of the whole
    mixed = "use the `load()` function please"  # 6 of 31 chars in backticks
    r = _code_block_ratio(mixed)
    assert 0.0 < r < 1.0
    assert _code_block_ratio("") == 0.0


def test_code_block_ratio_no_double_count_inline_inside_fence():
    # backticks inside a fenced block must not be counted twice (would exceed 1.0
    # without the fenced-removed remainder pass; clamp + remainder both guard it)
    t = "```\nuse `inner` here\n```"
    assert _code_block_ratio(t) == 1.0


def test_parse_ts_handles_z_and_absence():
    assert _parse_ts(None) is None
    assert _parse_ts("") is None
    assert _parse_ts("not a date") is None
    dt = _parse_ts("2026-06-21T03:48:22.421Z")
    assert dt is not None
    assert dt.year == 2026 and dt.minute == 48


# --- the full loop ----------------------------------------------------------

def _make_transcript(tmp_path, lines):
    p = tmp_path / "session.jsonl"
    p.write_text("\n".join(json.dumps(o) for o in lines) + "\n", encoding="utf-8")
    return p


def test_cadence_emits_one_row_per_record(tmp_path):
    transcript = _make_transcript(tmp_path, [
        {"type": "user", "uuid": "u1", "timestamp": "2026-06-21T03:00:00Z",
         "message": {"role": "user", "content": "how do I run this?"}},
        {"type": "assistant", "uuid": "a1", "parentUuid": "u1",
         "timestamp": "2026-06-21T03:00:10Z",
         "message": {"role": "assistant", "content": [
             {"type": "text", "text": "run `pytest` from the repo root."},
             {"type": "tool_use", "name": "Bash", "input": {}},  # dropped by ingest
         ]}},
    ])

    records = list(load(transcript))
    reader = CadenceReader()
    mono = Monolog.for_transcript(transcript)
    for row in reader.read(records):
        mono.append(row)
    written = mono.flush()

    # one cadence row per PROSE record (the tool_use block was filtered at ingest)
    assert written == len(records) == 2

    rows = list(mono.read())
    assert all(r.reader == "cadence" for r in rows)

    # first row: the human question
    human = rows[0].signal
    assert human["voice"] == "human"
    assert human["words"] == 5  # "how do I run this?"
    assert human["question_density"] == 1.0  # single sentence, is a question
    assert human["code_block_ratio"] == 0.0
    assert human["latency_s"] is None        # no previous record
    assert human["voice_switch"] is False    # nothing to switch from

    # second row: the assistant answer with an inline code span
    asst = rows[1].signal
    assert asst["voice"] == "assistant"
    assert asst["code_block_ratio"] > 0.0    # `pytest` backtick span
    assert asst["latency_s"] == 10.0         # 10s after the human turn
    assert asst["voice_switch"] is True      # human -> assistant


def test_cadence_anchor_joins_back_to_source(tmp_path):
    transcript = _make_transcript(tmp_path, [
        {"type": "user", "uuid": "u1",
         "message": {"role": "user", "content": "first turn"}},
    ])
    records = list(load(transcript))
    reader = CadenceReader()
    rows = list(reader.read(records))
    assert len(rows) == 1
    # the anchor seq points at the real prose record; src_uuid matches the source
    assert records[rows[0].anchor["seq"]].text == "first turn"
    assert rows[0].anchor["src_uuid"] == "u1"


def test_cadence_latency_none_when_timestamp_missing(tmp_path):
    # a record with no timestamp -> latency_s is None (honest absence, not 0.0),
    # and the NEXT record's latency carries from the last KNOWN ts, not the gap
    transcript = _make_transcript(tmp_path, [
        {"type": "user", "uuid": "u1", "timestamp": "2026-06-21T03:00:00Z",
         "message": {"role": "user", "content": "one"}},
        {"type": "assistant", "uuid": "a1", "parentUuid": "u1",  # NO timestamp
         "message": {"role": "assistant", "content": "two"}},
        {"type": "user", "uuid": "u2", "timestamp": "2026-06-21T03:00:30Z",
         "message": {"role": "user", "content": "three"}},
    ])
    records = list(load(transcript))
    rows = list(CadenceReader().read(records))
    assert rows[0].signal["latency_s"] is None        # first record
    assert rows[1].signal["latency_s"] is None         # this record has no ts
    # third record: carried from u1's ts (last known), 30s — a single missing
    # stamp doesn't poison the chain into None forever
    assert rows[2].signal["latency_s"] == 30.0


def test_cadence_voice_switch_tracks_runs(tmp_path):
    # two assistant turns in a row -> the second is NOT a voice switch
    transcript = _make_transcript(tmp_path, [
        {"type": "assistant", "uuid": "a1",
         "message": {"role": "assistant", "content": "part one"}},
        {"type": "assistant", "uuid": "a2",
         "message": {"role": "assistant", "content": "part two"}},
        {"type": "user", "uuid": "u1",
         "message": {"role": "user", "content": "ok"}},
    ])
    records = list(load(transcript))
    rows = list(CadenceReader().read(records))
    assert rows[0].signal["voice_switch"] is False  # first record
    assert rows[1].signal["voice_switch"] is False  # assistant -> assistant (a run)
    assert rows[2].signal["voice_switch"] is True   # assistant -> human
