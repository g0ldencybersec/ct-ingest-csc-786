#!/usr/bin/env bash
# Server-side controls for C1 trials. Runs ON THE LOG SERVER (as root);
# run-c1.sh on the client calls it over SSH. Prints one JSON object.
#
#   server-ctl.sh info     static facts about the server (record once per run)
#   server-ctl.sh check    log healthy and serving exactly the pinned tree
#   server-ctl.sh snap     counters: container CPU, /proc/stat, start time
#   server-ctl.sh idle     wait until CompactLog is idle (<5% of one core)
#   server-ctl.sh cold     stop log, drop OS page cache, start, wait out the
#                          30 s read throttle, check pin, wait idle
#   server-ctl.sh ready    like check, and also waits until the log has been
#                          up for longer than the read-throttle window
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOG="http://127.0.0.1:8080"
PIN="$ROOT/configs/testbed-c1.json"
THROTTLE_SECS=35   # CompactLog limits reads for 30 s after start; 5 s margin

die() { printf '{"ok":false,"error":"%s"}\n' "$*"; exit 1; }
cid() { (cd "$ROOT/testbed" && docker compose ps -q compactlog); }

cgroup_dir() {
  local id; id="$(docker inspect -f '{{.Id}}' "$(cid)")"
  for d in "/sys/fs/cgroup/system.slice/docker-$id.scope" "/sys/fs/cgroup/docker/$id"; do
    [ -f "$d/cpu.stat" ] && { echo "$d"; return; }
  done
  echo ""
}

cpu_usec() {
  local d; d="$(cgroup_dir)"
  [ -n "$d" ] && awk '/^usage_usec/{print $2}' "$d/cpu.stat" || echo null
}

wait_health() {
  for _ in $(seq 1 600); do
    curl -fsS "$LOG/health" >/dev/null 2>&1 && return 0
    sleep 1
  done
  die "log not healthy after 600 s"
}

check_pin() {
  python3 - "$PIN" "$(curl -fsS "$LOG/ct/v1/get-sth")" <<'PY' || die "log is not serving the pinned tree"
import json, sys
pin = json.load(open(sys.argv[1])); sth = json.loads(sys.argv[2])
assert sth["tree_size"] == pin["tree_size"] and sth["sha256_root_hash"] == pin["root_hash_b64"]
PY
}

uptime_secs() {
  python3 - "$(docker inspect -f '{{.State.StartedAt}}' "$(cid)")" <<'PY'
import sys, datetime
s = sys.argv[1].rstrip("Z")
s = s[:26] if "." in s else s   # trim nanoseconds to microseconds
t = datetime.datetime.fromisoformat(s).replace(tzinfo=datetime.timezone.utc)
print(int((datetime.datetime.now(datetime.timezone.utc) - t).total_seconds()))
PY
}

wait_idle() {
  local a b t0=$SECONDS
  while :; do
    a="$(cpu_usec)"; sleep 3; b="$(cpu_usec)"
    [ "$a" = null ] && return 0
    # < 5% of one core over 3 s = < 150,000 usec
    [ $((b - a)) -lt 150000 ] && return 0
    [ $((SECONDS - t0)) -gt 180 ] && { echo "warning: not idle after 180 s" >&2; return 0; }
  done
}

snap() {
  local d mem="null"; d="$(cgroup_dir)"
  [ -n "$d" ] && [ -f "$d/memory.current" ] && mem="$(cat "$d/memory.current")"
  read -r _ u n s i w q sq st _ < /proc/stat
  printf '{"ok":true,"t":%s,"started_at":"%s","uptime_secs":%s,"cpu_usage_usec":%s,"mem_bytes":%s,"proc_stat":{"total":%s,"idle":%s,"steal":%s},"loadavg":"%s"}\n' \
    "$(date +%s.%N)" "$(docker inspect -f '{{.State.StartedAt}}' "$(cid)")" "$(uptime_secs)" \
    "$(cpu_usec)" "$mem" "$((u+n+s+i+w+q+sq+st))" "$((i+w))" "$st" "$(cut -d' ' -f1-3 /proc/loadavg)"
}

case "${1:-}" in
  info)
    printf '{"ok":true,"hostname":"%s","kernel":"%s","nproc":%s,"mem_kib":%s,"docker":"%s","image":"%s","cache_mb":%s,"compose_file":"%s","git_commit":"%s"}\n' \
      "$(hostname)" "$(uname -r)" "$(nproc)" "$(awk '/MemTotal/{print $2}' /proc/meminfo)" \
      "$(docker version -f '{{.Server.Version}}')" "$(docker inspect -f '{{.Image}}' "$(cid)")" \
      "$(awk -F= '/memory_block_cache_capacity_mb/{gsub(/ /,"",$2);print $2}' "$ROOT/testbed/compactlog/Config.server.toml")" \
      "$(grep -h COMPOSE_FILE "$ROOT/testbed/.env" 2>/dev/null | cut -d= -f2)" "$(git -C "$ROOT" rev-parse --short HEAD)"
    ;;
  check) wait_health; check_pin; echo '{"ok":true}' ;;
  snap) snap ;;
  idle) wait_idle; echo '{"ok":true}' ;;
  ready)
    wait_health; check_pin
    up="$(uptime_secs)"; [ "$up" -lt "$THROTTLE_SECS" ] && sleep $((THROTTLE_SECS - up))
    echo '{"ok":true}'
    ;;
  cold)
    (cd "$ROOT/testbed" && docker compose stop compactlog >/dev/null 2>&1)
    sync; echo 3 > /proc/sys/vm/drop_caches
    (cd "$ROOT/testbed" && docker compose start compactlog >/dev/null 2>&1)
    wait_health; sleep "$THROTTLE_SECS"; check_pin; wait_idle
    echo '{"ok":true}'
    ;;
  *) sed -n '2,14p' "$0"; exit 2 ;;
esac
