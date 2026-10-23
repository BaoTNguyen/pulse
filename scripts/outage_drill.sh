#!/usr/bin/env bash
# Outage drill on a scratch pulse: own VASCULAR_HOME (keep it short, the
# socket path must fit SUN_LEN), socket and spool, and the pulse_test database.
# The collector loses its database mid-run, 20 events are emitted, and they
# must land exactly once when it returns.
#   scripts/outage_drill.sh /tmp/pd
set -u
S=$1; mkdir -p $S/spool/events
export VASCULAR_HOME=$S PULSE_DATABASE_URL="host=/var/run/postgresql dbname=pulse_test"
PG="host=/var/run/postgresql dbname=postgres"; T="$PULSE_DATABASE_URL"
restore() { psql "$PG" -qAtc "ALTER DATABASE pulse_test ALLOW_CONNECTIONS true"; }
trap 'restore; kill $COL 2>/dev/null' EXIT
psql "$T" -qAtc "truncate events, checkpoints, quarantine, contexts" 2>/dev/null
~/.cargo/bin/pulse collect --interval 1 >$S/collect.out 2>&1 & COL=$!
sleep 2
emit() { (cd ~/Coding/Projects/pulse/emitters/python && python3 - "$S" "$1" "$2" <<'PY'
import sys, pulse_emit
from pathlib import Path
s = Path(sys.argv[1])
for i in range(int(sys.argv[3])):
    pulse_emit.emit("probe", "drill.outage", s / "spool" / "events", s / "state" / "pulse" / "contexts",
                    payload={"mark": sys.argv[2], "i": i})
PY
); }
emit warm 5; sleep 3
echo "warm rows: $(psql "$T" -Atc "select count(*) from events where payload->>'mark'='warm'")"

psql "$PG" -qAtc "ALTER DATABASE pulse_test ALLOW_CONNECTIONS false"
psql "$PG" -qAtc "select count(pg_terminate_backend(pid)) from pg_stat_activity where datname='pulse_test'" | sed 's/^/outage: terminated /'
emit outage 20; sleep 5
echo "spool lines during outage: $(cat $S/spool/events/*.ndjson | grep -c '"outage"')"
echo "health during outage: $(timeout 5 ~/.cargo/bin/pulse health 2>&1 | head -c 200)"
echo "collector still running: $(kill -0 $COL 2>/dev/null && echo yes || echo no)"

restore; t0=$(date +%s.%N)
for i in $(seq 1 60); do
  n=$(psql "$T" -Atc "select count(*)||'|'||count(distinct id) from events where payload->>'mark'='outage'" 2>/dev/null)
  [ "$n" = "20|20" ] && break; sleep 0.5
done
echo "after restore: rows|distinct = $n in $(echo "$(date +%s.%N) - $t0" | bc)s"
echo "health after: $(~/.cargo/bin/pulse health)"
grep -i "error\|fail\|outage\|retry" $S/collect.out $S/log/pulse/collector.log 2>/dev/null | tail -3
