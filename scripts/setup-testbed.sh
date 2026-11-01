#!/usr/bin/env bash
# Build the testbed end to end: start CompactLog, build tools, fetch a slice,
# replay it into the log, and run the go/no-go check.
# Safe to re-run: finished steps are skipped. Full log goes to runs/.
#
# Usage:  CTT_CONTACT="you@example.edu" ./scripts/setup-testbed.sh
#   RESET=1         wipe the testbed volumes first (start from an empty log)
#   ENTRIES=1000000 entries to fetch (rounded up to whole 256-entry tiles)
#   SAMPLE_SEED=786 seeded random tiles across the log; set empty for prefix
#   LOG_URL=...     Static CT monitoring prefix of the source log
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOG_URL="${LOG_URL:-https://mon.sycamore.ct.letsencrypt.org/2026h1/}"
ENTRIES="${ENTRIES:-1000000}"
SAMPLE_SEED="${SAMPLE_SEED-786}"
if [ -n "$SAMPLE_SEED" ]; then
  DEFAULT_SLICE="sycamore-2026h1-$((ENTRIES / 1000))k-s$SAMPLE_SEED"
else
  DEFAULT_SLICE="sycamore-2026h1-$((ENTRIES / 1000))k-prefix"
fi
SLICE="${SLICE:-$ROOT/data/slices/$DEFAULT_SLICE}"
TESTBED="http://127.0.0.1:8080"
STAMP="$(date +%Y%m%d-%H%M%S)"
mkdir -p "$ROOT/runs"
exec > >(tee "$ROOT/runs/setup-testbed-$STAMP.log") 2>&1

step() { printf '\n==> %s\n' "$*"; }
die()  { printf '\nERROR: %s\n' "$*"; exit 1; }
tree_size() { curl -fsS "$TESTBED/ct/v1/get-sth" | sed -E 's/.*"tree_size":([0-9]+).*/\1/'; }

step "Preflight"
command -v docker >/dev/null || die "docker not found (is OrbStack running?)"
command -v cargo  >/dev/null || die "cargo not found"
docker info >/dev/null 2>&1   || die "docker daemon not reachable (start OrbStack)"
[ -n "${CTT_CONTACT:-}" ]     || die "set CTT_CONTACT to a contact email for the User-Agent"
echo "docker $(docker --version | awk '{print $3}') | $(cargo --version) | $(uname -sm)"
echo "slice: $SLICE"

if [ "${RESET:-0}" = "1" ]; then
  step "RESET=1: removing testbed and its volumes"
  (cd "$ROOT/testbed" && docker compose down -v)
fi

step "Start CompactLog testbed (first build takes several minutes)"
(cd "$ROOT/testbed" && docker compose up -d --build)
printf "waiting for health"
for i in $(seq 1 120); do
  if curl -fsS "$TESTBED/health" >/dev/null 2>&1; then echo " ok"; break; fi
  printf "."; sleep 2
  [ "$i" = 120 ] && die "testbed never became healthy; run: cd testbed && docker compose logs"
done
echo "testbed tree size: $(tree_size)"

step "Build tools"
(cd "$ROOT/harness" && cargo test -q && cargo build --release -q)
CTT="$ROOT/harness/target/release/ctt"

step "Fetch slice ($ENTRIES entries from $LOG_URL)"
if [ -f "$SLICE/manifest.json" ]; then
  echo "already fetched: $SLICE"
else
  SEED_ARGS=()
  [ -n "$SAMPLE_SEED" ] && SEED_ARGS=(--sample-seed "$SAMPLE_SEED")
  "$CTT" fetch --log-url "$LOG_URL" --entries "$ENTRIES" --out "$SLICE" ${SEED_ARGS[@]+"${SEED_ARGS[@]}"}
fi

step "Replay into testbed"
SIZE="$(tree_size)"
REPORTED="$(sed -nE 's/.*"testbed_tree_size": ([0-9]+).*/\1/p' "$SLICE/replay-report.json" 2>/dev/null || true)"
if [ -n "$REPORTED" ] && [ "$SIZE" = "$REPORTED" ]; then
  echo "already replayed (testbed tree size $SIZE)"
else
  "$CTT" replay --slice "$SLICE" --log "$TESTBED"
fi

step "Go/no-go"
set +e
"$CTT" gonogo --log "$TESTBED" --out "$ROOT/runs/gonogo-$STAMP.json"
RC=$?
set -e
echo
echo "log:    runs/setup-testbed-$STAMP.log"
echo "report: runs/gonogo-$STAMP.json"
[ "$RC" = 0 ] && echo "next:   ./scripts/snapshot.sh   (freeze this tree as the pinned testbed)"
exit $RC
