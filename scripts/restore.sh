#!/usr/bin/env bash
# Restore the pinned testbed from its snapshot and verify the tree head.
# Usage: ./scripts/restore.sh            (uses configs/testbed-c1.json)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TESTBED="http://127.0.0.1:8080"
CFG="$ROOT/configs/testbed-c1.json"
die() { printf '\nERROR: %s\n' "$*"; exit 1; }
[ -f "$CFG" ] || die "no $CFG; run scripts/snapshot.sh first"

read -r SNAP WANT_SHA WANT_SIZE WANT_ROOT < <(python3 -c '
import json,sys; c=json.load(open(sys.argv[1]))
print(c["snapshot"]["file"], c["snapshot"]["sha256"], c["tree_size"], c["root_hash_b64"])' "$CFG")
[ -f "$ROOT/$SNAP" ] || die "snapshot $SNAP not found"

echo "checking snapshot integrity"
GOT_SHA="$(shasum -a 256 "$ROOT/$SNAP" | awk '{print $1}')"
[ "$GOT_SHA" = "$WANT_SHA" ] || die "snapshot sha256 mismatch"

echo "replacing testbed volumes"
(cd "$ROOT/testbed" && docker compose down -v && docker compose up --no-start)
docker run --rm \
  -v ct-testbed_compactlog-data:/v/data -v ct-testbed_compactlog-keys:/v/keys \
  -v "$(dirname "$ROOT/$SNAP")":/backup:ro \
  alpine:3.20 sh -c "tar xzf /backup/$(basename "$SNAP") -C /v"
(cd "$ROOT/testbed" && docker compose up -d)

for _ in $(seq 1 120); do curl -fsS "$TESTBED/health" >/dev/null 2>&1 && break; sleep 1; done
STH="$(curl -fsS "$TESTBED/ct/v1/get-sth")"
python3 - "$STH" "$WANT_SIZE" "$WANT_ROOT" <<'PY' || die "restored tree does not match the pinned tree head"
import json, sys
s = json.loads(sys.argv[1])
assert s["tree_size"] == int(sys.argv[2]) and s["sha256_root_hash"] == sys.argv[3], s
print(f"restored: size {s['tree_size']}, root {s['sha256_root_hash']} (matches pin)")
PY
