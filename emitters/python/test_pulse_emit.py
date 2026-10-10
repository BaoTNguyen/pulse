"""Tests for pulse_emit. Run: python3 -m unittest discover emitters/python"""
import json
import multiprocessing
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import pulse_emit  # noqa: E402

ENVELOPE = json.loads((Path(__file__).parents[2] / "contract" / "envelope.json").read_text())
TYPES = {"string": str, "integer": int, "number": (int, float), "object": dict,
         "uuid": str, "timestamp": str, "name": str, "kind": str, "origin": str}


def _lines(journal: Path) -> list[dict]:
    return [json.loads(line) for f in sorted(journal.glob("*.ndjson"))
            for line in f.read_text().splitlines()]


def _writer(journal: str, n: int) -> None:
    for i in range(n):
        pulse_emit.emit("heart", "test.burst", Path(journal), payload={"i": i, "pad": "x" * 9000})


class TestPulseEmit(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.journal = Path(self.tmp.name) / "events"
        self.state = Path(self.tmp.name) / "contexts"
        pulse_emit._announced.clear()
        for var in ("PULSE_PROJECT", "PLEXUS_GOAL_ID", "PULSE_TRACE_ID", "PULSE_PARENT_SPAN"):
            os.environ.pop(var, None)

    def tearDown(self):
        self.tmp.cleanup()

    def test_every_line_fits_the_envelope(self):
        os.environ["PULSE_TRACE_ID"] = "trace-1"
        pulse_emit.emit("heart", "episode.started", self.journal, self.state,
                        episode_id="ep-1", origin="agent", payload={"agent": "claude"})
        for ev in _lines(self.journal):
            for field, typ in ENVELOPE["required"].items():
                self.assertIsInstance(ev[field], TYPES[typ], field)
            for field, value in ev.items():
                typ = ENVELOPE["optional"].get(field) or ENVELOPE["required"].get(field)
                self.assertIsNotNone(typ, f"{field} is not in envelope.json")
                self.assertIsInstance(value, TYPES[typ], field)
        self.assertEqual(_lines(self.journal)[-1]["trace_id"], "trace-1")

    def test_context_is_announced_once_across_processes(self):
        pulse_emit.emit("heart", "a.b", self.journal, self.state)
        pulse_emit._announced.clear()  # as if a new process started
        pulse_emit.emit("heart", "a.c", self.journal, self.state)
        kinds = [e["kind"] for e in _lines(self.journal)]
        self.assertEqual(kinds, ["context.started", "a.b", "a.c"])
        started = _lines(self.journal)[0]
        self.assertEqual(started["context_id"], pulse_emit.context_id(started["payload"]))

    def test_ids_are_unique_and_seq_increases(self):
        for _ in range(5):
            pulse_emit.emit("heart", "a.b", self.journal)
        evs = [e for e in _lines(self.journal) if e["kind"] == "a.b"]
        self.assertEqual(len({e["id"] for e in evs}), 5)
        seqs = [e["emitter_seq"] for e in evs]
        self.assertEqual(seqs, sorted(seqs))

    def test_caller_fields_win_and_none_is_absent(self):
        os.environ["PULSE_PROJECT"] = "fromenv"
        pulse_emit.emit("heart", "a.b", self.journal, project="explicit", task_id=None)
        ev = _lines(self.journal)[-1]
        self.assertEqual(ev["project"], "explicit")
        self.assertNotIn("task_id", ev)

    def test_concurrent_long_lines_stay_whole(self):
        procs = [multiprocessing.Process(target=_writer, args=(str(self.journal), 40)) for _ in range(4)]
        for p in procs:
            p.start()
        for p in procs:
            p.join()
        evs = [e for e in _lines(self.journal) if e["kind"] == "test.burst"]  # json.loads raises on a torn line
        self.assertEqual(len(evs), 160)

    def test_emit_never_raises(self):
        pulse_emit.emit("heart", "a.b", Path("/proc/nonexistent/events"))


if __name__ == "__main__":
    unittest.main()
