"""Regenerate contract/observed.json from a journal: every (source, kind) pair
with the envelope and payload field names present in at least 5% of its
events. Names only, never values.

    python3 scripts/observe_kinds.py ~/.vascular/spool/events [--until 20261007] > contract/observed.json
"""
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

ABOUT = ("Every (source, kind) pair seen in the real journal up to {date}, with the "
         "envelope and payload field names present in at least 5% of its events. Names only, "
         "never values. Input for catalog.json; regenerate it from a journal rather than "
         "editing it by hand (scripts/observe_kinds.py).")
NOT_ENVELOPE = {"ts", "source", "kind", "payload"}
TEST_SOURCES = {"test", "probe"}  # leaked by test runs, not the stack


def main(journal: str, until: str | None = None) -> dict:
    seen = Counter()
    env, pay = defaultdict(Counter), defaultdict(Counter)
    last = ""
    for day in sorted(Path(journal).glob("*.ndjson")):
        if until and day.stem > until:
            continue
        last = day.stem
        for line in day.open(encoding="utf-8", errors="replace"):
            try:
                e = json.loads(line)
                key = (e["source"], e["kind"])
            except Exception:
                continue
            if key[0] in TEST_SOURCES:
                continue
            seen[key] += 1
            env[key].update(k for k in e if k not in NOT_ENVELOPE)
            if isinstance(e.get("payload"), dict):
                pay[key].update(e["payload"].keys())
    common = lambda c, n: sorted(k for k, m in c.items() if m * 20 >= n)  # noqa: E731
    kinds = [{"source": s, "kind": k, "envelope_fields": common(env[(s, k)], n),
              "payload_fields": common(pay[(s, k)], n)} for (s, k), n in sorted(seen.items())]
    return {"about": ABOUT.format(date=f"{last[:4]}-{last[4:6]}-{last[6:]}"), "kinds": kinds}


if __name__ == "__main__":
    args = sys.argv[1:]
    until = args[args.index("--until") + 1] if "--until" in args else None
    print(json.dumps(main(args[0], until), indent=2))
