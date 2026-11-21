#!/usr/bin/env bash
# C1 trial campaign. Runs ON THE CLIENT DROPLET as root.
#
#   SERVER=10.x.x.x ./scripts/run-c1.sh                 new campaign
#   SERVER=10.x.x.x RUN=runs/c1-YYYYMMDD-HHMM ./scripts/run-c1.sh   resume
#
# Design: a "unit" is (replicate, network, cache state). Every unit reads ONE
# randomly chosen reference block through all arms in a random order, so arms
# are compared in pairs on identical data under identical conditions.
# Units run in a seeded random order (no time-of-day drift onto one condition).
#
#   ARMS    legacy-gzip tiles legacy-identity   (gzip vs tiles is primary)
#   NETS    lan wan                             (scripts/netem.sh profiles)
#   CACHES  cold warm
#     cold: log stopped, OS page cache dropped, restarted, read throttle waited out
#     warm: same arm and block read once (unrecorded) just before the trial
#   REPS    replicates per (net, cache) cell, default 10
#
# Resumable: finished, verified trials are skipped. Writes to $RUN/:
#   plan.tsv, meta.json, trials.jsonl (one line per attempt), crashes.jsonl,
#   events/, primes/
#
# If the log crashes, the crash is recorded, the log restarted and checked
# against the pin, and the trial retried (MAX_ATTEMPTS, default 2).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
: "${SERVER:?set SERVER to the log server private IP}"
SSH_TARGET="${SSH_TARGET:-root@$SERVER}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/ct_server}"
LOG="http://$SERVER:8080"
REPS="${REPS:-10}"
SEED="${SEED:-786}"
CONC="${CONC:-4}"
ARMS="${ARMS:-legacy-gzip tiles legacy-identity}"
NETS="${NETS:-lan wan}"
CACHES="${CACHES:-cold warm}"
BLOCKS_FILE="$ROOT/configs/testbed-c1-blocks.json"
CTT="$ROOT/harness/target/release/ctt"
RUN="${RUN:-$ROOT/runs/c1-$(date -u +%Y%m%d-%H%M)}"
mkdir -p "$RUN/events" "$RUN/primes"

die() { printf '\nERROR: %s\n' "$*" >&2; exit 1; }
rctl() { ssh -n -i "$SSH_KEY" -o BatchMode=yes -o StrictHostKeyChecking=accept-new "$SSH_TARGET" "/opt/ct/scripts/server-ctl.sh $*"; }
net() { SERVER="$SERVER" "$ROOT/scripts/netem.sh" "$@"; }
trap 'net lan || true' EXIT

[ -x "$CTT" ] || die "build first: (cd harness && cargo build --release)"
[ -f "$BLOCKS_FILE" ] || die "missing $BLOCKS_FILE; run: $CTT blocks --log $LOG --out $BLOCKS_FILE"
[ "$(id -u)" = 0 ] || die "run as root (netem needs it)"

net lan
if ! rctl snap | grep -q '"running":true'; then
  echo "log is down at start; restarting it"
  rctl recover >/dev/null || die "could not restart the log"
fi
rctl check >/dev/null || die "server check failed (log down or not the pinned tree)"

# ---- plan (only created once per run directory) -----------------------------
if [ ! -f "$RUN/plan.tsv" ]; then
  python3 - "$RUN/plan.tsv" "$BLOCKS_FILE" "$SEED" "$REPS" "$ARMS" "$NETS" "$CACHES" <<'PY'
import json, random, sys
out, bf, seed, reps = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
arms, nets, caches = sys.argv[5].split(), sys.argv[6].split(), sys.argv[7].split()
nblocks = len(json.load(open(bf))["blocks"])
rng = random.Random(seed)
units = [(r, n, c) for r in range(1, reps + 1) for n in nets for c in caches]
rng.shuffle(units)
with open(out, "w") as f:
    f.write("trial_id\tunit\trep\tnet\tcache\tarm\tblock\tposition\n")
    for u, (r, n, c) in enumerate(units, 1):
        block = rng.randrange(nblocks)
        order = arms[:]; rng.shuffle(order)
        for pos, a in enumerate(order, 1):
            f.write(f"u{u:03d}-{a}\tu{u:03d}\t{r}\t{n}\t{c}\t{a}\t{block}\t{pos}\n")
PY
  SERVER_INFO="$(rctl info)"
  python3 - "$RUN/meta.json" "$SERVER_INFO" <<PY
import json, sys, subprocess, platform, os
m = {
  "experiment": "C1", "seed": $SEED, "reps": $REPS, "concurrency": $CONC,
  "arms": "$ARMS".split(), "nets": "$NETS".split(), "caches": "$CACHES".split(),
  "wan_profile": {"delay_each_way": os.environ.get("WAN_DELAY", "25ms"), "rate_each_way": os.environ.get("WAN_RATE", "50mbit")},
  "server": json.loads(sys.argv[2]),
  "client": {"kernel": platform.release(), "nproc": os.cpu_count(),
             "mem_kib": int(open("/proc/meminfo").read().split()[1]),
             "ctt_version": subprocess.run(["$CTT", "--version"], capture_output=True, text=True).stdout.strip(),
             "git_commit": subprocess.run(["git", "-C", "$ROOT", "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()},
  "blocks_file": "configs/testbed-c1-blocks.json",
}
json.dump(m, open(sys.argv[1], "w"), indent=2)
PY
fi

TOTAL=$(($(wc -l < "$RUN/plan.tsv") - 1))
echo "run: $RUN  ($TOTAL trials)"

done_ok() { [ -f "$RUN/events/$1.summary.json" ] && grep -q '"verified": true' "$RUN/events/$1.summary.json"; }

# ---- crash handling ----------------------------------------------------------
# If the log dies (e.g. OOM-killed), record it in crashes.jsonl, restart it,
# check it still serves the pinned tree, and retry the trial (MAX_ATTEMPTS).
MAX_ATTEMPTS="${MAX_ATTEMPTS:-2}"
log_up() { rctl snap 2>/dev/null | grep -q '"running":true'; }
recover() {  # $1 trial id, $2 phase, $3 attempt
  net lan
  local snapj info
  snapj="$(rctl snap 2>/dev/null || echo '{}')"
  echo "  !! log is down during $2 of $1; restarting"
  info="$(rctl recover)" || die "log could not be recovered after crash in $1"
  python3 - "$RUN/crashes.jsonl" "$1" "$2" "$3" "$snapj" "$info" <<'PY'
import json, sys, datetime
out, tid, phase, att, snap, info = sys.argv[1:7]
def j(x):
    try: return json.loads(x)
    except Exception: return x
rec = {"t": datetime.datetime.now(datetime.timezone.utc).isoformat(), "trial_id": tid,
       "phase": phase, "attempt": int(att), "snap_at_detection": j(snap), "recover": j(info)}
open(out, "a").write(json.dumps(rec) + "\n")
PY
}

n=0
# Read the plan on fd 3 so nothing in the loop (ssh, ctt, ping) can consume it.
while IFS=$'\t' read -r -u 3 tid unit rep netp cache arm block pos; do
  n=$((n + 1))
  if done_ok "$tid"; then continue; fi

  attempt=0
  while :; do
    attempt=$((attempt + 1))
    printf '\n[%d/%d] %s  net=%s cache=%s block=%s%s\n' "$n" "$TOTAL" "$tid" "$netp" "$cache" "$block" \
      "$([ $attempt -gt 1 ] && echo "  (attempt $attempt)")"

    net lan
    if ! log_up; then recover "$tid" "pre-trial" "$attempt"; fi
    if [ "$cache" = cold ]; then
      rctl cold >/dev/null || { recover "$tid" "cold-reset" "$attempt"; [ $attempt -lt "$MAX_ATTEMPTS" ] && continue; echo "  giving up on $tid"; break; }
    else
      rctl ready >/dev/null || { recover "$tid" "ready" "$attempt"; [ $attempt -lt "$MAX_ATTEMPTS" ] && continue; echo "  giving up on $tid"; break; }
      if ! "$CTT" bench --log "$LOG" --arm "$arm" --block "$block" --blocks "$BLOCKS_FILE" \
          --concurrency "$CONC" --trial-id "$tid-prime" \
          --events /dev/null --summary "$RUN/primes/$tid.json" 2>/dev/null; then
        if log_up; then die "priming read failed for $tid with the log up (see $RUN/primes/$tid.json)"; fi
        recover "$tid" "prime" "$attempt"
        [ $attempt -lt "$MAX_ATTEMPTS" ] && continue
        echo "  giving up on $tid after $attempt attempts"; break
      fi
      rctl idle >/dev/null
    fi

    net "$netp"
    RTT="$(net rtt || echo null)"
    BEFORE="$(rctl snap)"
    rm -f "$RUN/events/$tid.summary.json"
    set +e
    "$CTT" bench --log "$LOG" --arm "$arm" --block "$block" --blocks "$BLOCKS_FILE" \
      --concurrency "$CONC" --trial-id "$tid" \
      --events "$RUN/events/$tid.jsonl" --summary "$RUN/events/$tid.summary.json" \
      --meta "unit=$unit" --meta "rep=$rep" --meta "net=$netp" --meta "cache=$cache" --meta "position=$pos" \
      --meta "attempt=$attempt"
    rc=$?
    set -e
    AFTER="$(rctl snap || echo '{}')"
    net lan

    python3 - "$RUN/events/$tid.summary.json" "$BEFORE" "$AFTER" "${RTT:-null}" "$RUN/trials.jsonl" "$rc" "$tid" "$attempt" <<'PY'
import json, sys
p, b, a, rtt, out, rc, tid, att = sys.argv[1:9]
try:
    s = json.load(open(p))
except Exception:
    s = {"trial_id": tid, "verified": False, "failures": [f"ctt exited {rc} without a summary"]}
def j(x):
    try: return json.loads(x)
    except Exception: return {}
b, a = j(b), j(a)
def g(d, *ks):
    for k in ks:
        d = d.get(k) if isinstance(d, dict) else None
    return d
dt = (g(a, "proc_stat", "total") or 0) - (g(b, "proc_stat", "total") or 0)
cb, ca = g(b, "cpu_usage_usec"), g(a, "cpu_usage_usec")
s["server"] = {
    "cpu_secs": (ca - cb) / 1e6 if None not in (ca, cb) else None,
    "steal_frac": (g(a, "proc_stat", "steal") - g(b, "proc_stat", "steal")) / dt if dt > 0 else None,
    "busy_frac": 1 - (g(a, "proc_stat", "idle") - g(b, "proc_stat", "idle")) / dt if dt > 0 else None,
    "mem_bytes_before": g(b, "mem_bytes"),
    "mem_bytes_after": g(a, "mem_bytes"),
    "running_after": g(a, "running"),
    "restarted_during_trial": g(a, "running") is not True or g(a, "started_at") != g(b, "started_at"),
    "uptime_secs_before": g(b, "uptime_secs"),
}
s["rtt_ms_measured"] = None if rtt in ("", "null") else float(rtt)
s["exit_code"] = int(rc)
s["attempt"] = int(att)
with open(out, "a") as f:
    f.write(json.dumps(s) + "\n")
PY

    if [ "$rc" = 0 ]; then break; fi
    if ! log_up; then
      recover "$tid" "trial" "$attempt"
      [ $attempt -lt "$MAX_ATTEMPTS" ] && continue
    fi
    echo "  trial $tid failed (exit $rc) after $attempt attempt(s); moving on"
    break
  done
done 3< <(tail -n +2 "$RUN/plan.tsv")

echo
echo "campaign finished: $RUN"
echo "commit results:  git add $RUN && git commit -m 'C1 results' && git push"
