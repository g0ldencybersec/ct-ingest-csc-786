#!/usr/bin/env bash
# Delete local CT data and testbed state.
#
# Implements the data-retention commitment: certificate data is held only on
# the experiment machine for the duration of the study and deleted afterward.
#
# Never touches configs/ or runs/ (committed code, configuration, and
# aggregate results).
#
# Usage:
#   ./scripts/cleanup.sh                 show what exists (deletes nothing)
#   ./scripts/cleanup.sh --slices        delete downloaded slices (data/slices)
#   ./scripts/cleanup.sh --snapshots     delete testbed snapshots (data/snapshots)
#   ./scripts/cleanup.sh --testbed       remove testbed container + volumes
#   ./scripts/cleanup.sh --build         remove Rust build output (harness/target)
#   ./scripts/cleanup.sh --all           all of the above + the testbed image
#   add --yes to skip the confirmation prompt
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DATA="$ROOT/data"

die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

# Refuse to run anywhere that does not look like this project.
[ -f "$ROOT/README.md" ] && [ -d "$ROOT/testbed" ] && [ -d "$ROOT/harness" ] \
  || die "unexpected project layout at $ROOT; refusing to delete anything"

SLICES=0 SNAPSHOTS=0 TESTBED=0 BUILD=0 IMAGE=0 YES=0
[ $# -eq 0 ] && DRY=1 || DRY=0
for arg in "$@"; do
  case "$arg" in
    --slices) SLICES=1 ;;
    --snapshots) SNAPSHOTS=1 ;;
    --testbed) TESTBED=1 ;;
    --build) BUILD=1 ;;
    --all) SLICES=1 SNAPSHOTS=1 TESTBED=1 BUILD=1 IMAGE=1 ;;
    --yes) YES=1 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) die "unknown option: $arg (see --help)" ;;
  esac
done

size_of() { [ -e "$1" ] && du -sh "$1" 2>/dev/null | awk '{print $1}' || echo "-"; }
have_docker() { command -v docker >/dev/null && docker info >/dev/null 2>&1; }
volumes() { have_docker && docker volume ls -q --filter name='^ct-testbed_' || true; }

echo "Local state in $ROOT:"
printf '  %-28s %s\n' "data/slices"     "$(size_of "$DATA/slices")"
if [ -d "$DATA/slices" ]; then
  for s in "$DATA"/slices/*/; do
    [ -d "$s" ] && printf '    %-26s %s\n' "$(basename "$s")" "$(size_of "$s")"
  done
fi
printf '  %-28s %s\n' "data/snapshots"  "$(size_of "$DATA/snapshots")"
printf '  %-28s %s\n' "harness/target"  "$(size_of "$ROOT/harness/target")"
if have_docker; then
  printf '  %-28s %s\n' "testbed volumes" "$(volumes | tr '\n' ' ')"
  printf '  %-28s %s\n' "testbed image" \
    "$(docker image ls -q ct-testbed/compactlog 2>/dev/null | head -1 || true)"
else
  printf '  %-28s %s\n' "docker" "not reachable (testbed state not shown)"
fi

if [ "$DRY" = 1 ]; then
  echo
  echo "Nothing deleted. Choose what to remove: --slices --snapshots --testbed --build, or --all."
  exit 0
fi

echo
echo "Will delete:"
[ "$SLICES" = 1 ]    && echo "  - data/slices (downloaded certificates, tiles, issuers)"
[ "$SNAPSHOTS" = 1 ] && echo "  - data/snapshots (testbed archives, including log signing keys)"
[ "$TESTBED" = 1 ]   && echo "  - testbed container and volumes (the loaded log)"
[ "$IMAGE" = 1 ]     && echo "  - testbed Docker image"
[ "$BUILD" = 1 ]     && echo "  - harness/target (Rust build output)"
echo "Kept: configs/, runs/, source code."

if [ "$YES" != 1 ]; then
  printf '\nType "delete" to continue: '
  read -r answer
  [ "$answer" = "delete" ] || { echo "Cancelled."; exit 1; }
fi

if [ "$TESTBED" = 1 ] || [ "$IMAGE" = 1 ]; then
  if have_docker; then
    RMI=()
    [ "$IMAGE" = 1 ] && RMI=(--rmi local)
    (cd "$ROOT/testbed" && docker compose down -v ${RMI[@]+"${RMI[@]}"})
  else
    echo "warning: docker not reachable; testbed volumes NOT removed"
  fi
fi
[ "$SLICES" = 1 ]    && rm -rf "$DATA/slices"    && echo "removed data/slices"
[ "$SNAPSHOTS" = 1 ] && rm -rf "$DATA/snapshots" && echo "removed data/snapshots"
[ "$BUILD" = 1 ]     && rm -rf "$ROOT/harness/target" && echo "removed harness/target"
rmdir "$DATA" 2>/dev/null || true

echo
echo "Done."
if [ "$IMAGE" = 1 ]; then
  echo "Note: Docker's build cache may still hold intermediate build layers."
  echo "      Inspect with 'docker system df'; 'docker builder prune' clears it for ALL projects."
fi
