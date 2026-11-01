#!/usr/bin/env bash
# Freeze the current testbed as THE pinned testbed for all experiments.
#
# Writes:
#   data/snapshots/<name>.tgz      CompactLog data + signing keys (not committed)
#   configs/testbed-c1.json        pinned tree head and provenance (committed)
#
# The tree is only pinned if it survives a restart unchanged, which shows the
# snapshot is a consistent copy of persisted state.
#
# Usage: SLICE=data/slices/<slice> ./scripts/snapshot.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TESTBED="http://127.0.0.1:8080"
VOL_DATA="ct-testbed_compactlog-data"
VOL_KEYS="ct-testbed_compactlog-keys"
COMPACTLOG_REV="$(sed -nE 's/^ARG COMPACTLOG_REV=(.*)/\1/p' "$ROOT/testbed/compactlog/Dockerfile")"
die() { printf '\nERROR: %s\n' "$*"; exit 1; }
command -v python3 >/dev/null || die "python3 is required"

# Default slice: the most recently replayed one.
if [ -z "${SLICE:-}" ]; then
  SLICE="$(ls -t "$ROOT"/data/slices/*/replay-report.json 2>/dev/null | head -1 | xargs -n1 dirname 2>/dev/null || true)"
fi
[ -f "${SLICE:-}/manifest.json" ] || die "set SLICE to the slice directory that was replayed"
SLICE="$(cd "$SLICE" && pwd)"

wait_healthy() {
  for _ in $(seq 1 120); do
    curl -fsS "$TESTBED/health" >/dev/null 2>&1 && return 0
    sleep 1
  done
  die "testbed not healthy"
}

wait_healthy
STH_BEFORE="$(curl -fsS "$TESTBED/ct/v1/get-sth")"
CP_BEFORE="$(curl -fsS "$TESTBED/checkpoint")"
SIZE="$(printf '%s' "$STH_BEFORE" | python3 -c 'import json,sys;print(json.load(sys.stdin)["tree_size"])')"
ROOT_B64="$(printf '%s' "$STH_BEFORE" | python3 -c 'import json,sys;print(json.load(sys.stdin)["sha256_root_hash"])')"
[ "$SIZE" -gt 0 ] || die "testbed is empty"
SHORT="$(printf '%s' "$ROOT_B64" | tr '+/' '-_' | cut -c1-8)"
NAME="testbed-c1-${SIZE}-${SHORT}"
mkdir -p "$ROOT/data/snapshots" "$ROOT/configs"

echo "stopping testbed for a consistent copy (tree size $SIZE, root $ROOT_B64)"
(cd "$ROOT/testbed" && docker compose stop compactlog)
docker run --rm \
  -v "$VOL_DATA":/v/data:ro -v "$VOL_KEYS":/v/keys:ro \
  -v "$ROOT/data/snapshots":/backup \
  alpine:3.20 tar czf "/backup/$NAME.tgz" -C /v data keys
echo "starting testbed again"
(cd "$ROOT/testbed" && docker compose start compactlog)
wait_healthy

STH_AFTER="$(curl -fsS "$TESTBED/ct/v1/get-sth")"
python3 - "$STH_BEFORE" "$STH_AFTER" <<'PY' || die "tree changed across restart; not pinning"
import json, sys
a, b = (json.loads(x) for x in sys.argv[1:3])
assert (a["tree_size"], a["sha256_root_hash"]) == (b["tree_size"], b["sha256_root_hash"]), (a, b)
PY

IMAGE_ID="$(docker image inspect ct-testbed/compactlog:1ceab9f --format '{{.Id}}' 2>/dev/null || echo unknown)"
GONOGO="$(ls -t "$ROOT"/runs/gonogo*.json "$ROOT"/runs/m1-gonogo*.json 2>/dev/null | head -1 || true)"

python3 - "$ROOT" "$NAME" "$STH_BEFORE" "$CP_BEFORE" "$SLICE" "$COMPACTLOG_REV" "$IMAGE_ID" "$GONOGO" <<'PY'
import datetime, hashlib, json, os, sys
root, name, sth, cp, slice_dir, rev, image_id, gonogo = sys.argv[1:9]

def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()

snap = os.path.join(root, "data", "snapshots", name + ".tgz")
manifest_path = os.path.join(slice_dir, "manifest.json")
manifest = json.load(open(manifest_path))
sth = json.loads(sth)
gonogo_rel = os.path.relpath(gonogo, root) if gonogo else None
verdict = json.load(open(gonogo))["verdict"] if gonogo else None
pinned = {
    "testbed": "C1 (CompactLog)",
    "pinned_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "tree_size": sth["tree_size"],
    "root_hash_b64": sth["sha256_root_hash"],
    "sth": sth,
    "checkpoint": cp,
    "snapshot": {
        "file": os.path.relpath(snap, root),
        "bytes": os.path.getsize(snap),
        "sha256": sha256_file(snap),
        "contains": ["compactlog data volume", "log signing keys"],
    },
    "compactlog": {"commit": rev, "docker_image_id": image_id},
    "slice": {
        "dir": os.path.relpath(slice_dir, root),
        "manifest_sha256": sha256_file(manifest_path),
        "source_origin": manifest.get("source_origin"),
        "source_tree_size": manifest.get("source_tree_size"),
        "entries": manifest.get("entries"),
        "sampling": manifest.get("sampling"),
        "x509_entries": manifest.get("x509_entries"),
        "precert_entries": manifest.get("precert_entries"),
    },
    "gonogo_report": gonogo_rel,
    "gonogo_verdict": verdict,
}
out = os.path.join(root, "configs", "testbed-c1.json")
json.dump(pinned, open(out, "w"), indent=2)
print(f"pinned: size {pinned['tree_size']}, root {pinned['root_hash_b64']}")
print(f"snapshot: {pinned['snapshot']['file']} ({pinned['snapshot']['bytes']/1e9:.2f} GB)")
print(f"config:   configs/testbed-c1.json")
PY
