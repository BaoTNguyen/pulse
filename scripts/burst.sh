#!/usr/bin/env bash
# Burst on a scratch pulse (see outage_drill.sh): the real peak is 54
# events/minute, so 100x is 5,400 realistic-size events emitted as fast as one
# emitter can; the collector is timed until every row has landed.
#   scripts/burst.sh /tmp/pd [N]
set -u
S=$1; N=${2:-5400}
export VASCULAR_HOME=$S PULSE_DATABASE_URL="host=/var/run/postgresql dbname=pulse_test"
T="$PULSE_DATABASE_URL"
trap 'kill $COL 2>/dev/null' EXIT
psql "$T" -qAtc "truncate events, checkpoints, quarantine, contexts"
rm -rf $S/spool $S/state; mkdir -p $S/spool/events
~/.cargo/bin/pulse collect --interval 2 >$S/collect.out 2>&1 & COL=$!
sleep 2
t0=$(date +%s.%N)
(cd ~/Coding/Projects/pulse/emitters/python && python3 - "$S" "$N" <<'PY'
import sys, time, pulse_emit
from pathlib import Path
s, n = Path(sys.argv[1]), int(sys.argv[2])
t = time.perf_counter()
for i in range(n):
    pulse_emit.emit("probe", "drill.burst", s / "spool" / "events", s / "state" / "pulse" / "contexts",
                    run_id="r-%d" % (i % 50), payload={"i": i, "note": "x" * 300})
print(f"emitted {n} in {time.perf_counter() - t:.2f}s ({n / (time.perf_counter() - t):.0f}/s)")
PY
)
peak=0
while :; do
  h=$(~/.cargo/bin/pulse health 2>/dev/null)
  lag=$(echo "$h" | python3 -c "import json,sys; print(json.load(sys.stdin)['lag_bytes'])" 2>/dev/null || echo 0)
  [ "$lag" -gt "$peak" ] && peak=$lag
  rows=$(psql "$T" -Atc "select count(*) from events where kind='drill.burst'")
  [ "$rows" -ge "$N" ] && break
  sleep 0.2
done
dt=$(echo "$(date +%s.%N) - $t0" | bc)
echo "ingested $rows rows (distinct ids: $(psql "$T" -Atc "select count(distinct id) from events where kind='drill.burst'")) in ${dt}s from first emit; peak lag ${peak} bytes"
echo "spool: $(du -sh $S/spool/events | cut -f1); table: $(psql "$T" -Atc "select pg_size_pretty(pg_total_relation_size('events'))")"
q() { /usr/bin/time -f "%es" ~/.cargo/bin/pulse events --run r-7 --limit 500 2>&1 >/dev/null | tail -1; }
echo "query --run r-7 (108 rows) took: $(q)"
