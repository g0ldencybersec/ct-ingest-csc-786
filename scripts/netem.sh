#!/usr/bin/env bash
# Network profiles for C1, applied ON THE CLIENT to the interface that
# reaches the log server (the VPC interface). Both directions are shaped:
# egress directly, ingress via an ifb device.
#
#   SERVER=10.x.x.x ./scripts/netem.sh lan    no emulation (VPC as-is)
#   SERVER=10.x.x.x ./scripts/netem.sh wan    WAN_DELAY each way + WAN_RATE each way
#   SERVER=10.x.x.x ./scripts/netem.sh show   current qdiscs and measured RTT
#
# Defaults: 25 ms each way (50 ms RTT), 50 Mbit/s each way: a constrained
# single-operator monitor on a home/small-office link.
set -euo pipefail
: "${SERVER:?set SERVER to the log server private IP}"
WAN_DELAY="${WAN_DELAY:-25ms}"
WAN_RATE="${WAN_RATE:-50mbit}"
WAN_LIMIT="${WAN_LIMIT:-10000}"   # packets; large enough not to drop at 50 Mbit x 50 ms

DEV="$(ip -o route get "$SERVER" | sed -n 's/.* dev \([^ ]*\).*/\1/p')"
[ -n "$DEV" ] || { echo "no route to $SERVER" >&2; exit 1; }

clear_all() {
  tc qdisc del dev "$DEV" root 2>/dev/null || true
  tc qdisc del dev "$DEV" ingress 2>/dev/null || true
  ip link show ifb0 >/dev/null 2>&1 && tc qdisc del dev ifb0 root 2>/dev/null || true
}

rtt_ms() { ping -c 5 -i 0.2 -q "$SERVER" | awk -F/ '/rtt|round-trip/{print $5}'; }

case "${1:-}" in
  lan) clear_all ;;
  wan)
    clear_all
    modprobe ifb numifbs=1 2>/dev/null || true
    ip link show ifb0 >/dev/null 2>&1 || ip link add ifb0 type ifb
    ip link set ifb0 up
    tc qdisc add dev "$DEV" root netem delay "$WAN_DELAY" rate "$WAN_RATE" limit "$WAN_LIMIT"
    tc qdisc add dev "$DEV" handle ffff: ingress
    tc filter add dev "$DEV" parent ffff: protocol all matchall action mirred egress redirect dev ifb0
    tc qdisc add dev ifb0 root netem delay "$WAN_DELAY" rate "$WAN_RATE" limit "$WAN_LIMIT"
    ;;
  show)
    echo "dev=$DEV"; tc qdisc show dev "$DEV"; ip link show ifb0 >/dev/null 2>&1 && tc qdisc show dev ifb0
    echo "rtt_ms=$(rtt_ms)"
    ;;
  rtt) rtt_ms ;;
  *) sed -n '2,12p' "$0"; exit 2 ;;
esac
