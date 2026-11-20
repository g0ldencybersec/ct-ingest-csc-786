#!/usr/bin/env bash
# Provision the C1 CLIENT droplet. Run as root on a fresh Ubuntu 24.04 droplet
# in the SAME region and VPC as the log server:
#
#   curl -fsSL https://raw.githubusercontent.com/g0ldencybersec/ct-ingest-csc-786/main/scripts/client-setup.sh | bash
#   (or: git clone the repo to /opt/ct and run ./scripts/client-setup.sh)
#
# Installs pinned tooling, builds the harness, checks that netem works, and
# creates the SSH key the client uses to call scripts/server-ctl.sh on the
# server. Prints the public key to add to the server.
set -euo pipefail

REPO="${REPO:-https://github.com/g0ldencybersec/ct-ingest-csc-786}"
REF="${REF:-main}"
RUST_VERSION="${RUST_VERSION:-1.89.0}"   # same toolchain as the testbed image
DIR=/opt/ct

[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 1; }
export DEBIAN_FRONTEND=noninteractive

echo "==> packages"
apt-get update -qq
apt-get install -y -qq build-essential pkg-config git curl iproute2 iputils-ping python3 >/dev/null
# sch_netem and ifb live in linux-modules-extra on Ubuntu cloud kernels.
apt-get install -y -qq "linux-modules-extra-$(uname -r)" >/dev/null 2>&1 || true
modprobe sch_netem && modprobe ifb numifbs=1 || { echo "netem/ifb kernel modules unavailable" >&2; exit 1; }

echo "==> rust $RUST_VERSION"
if ! command -v cargo >/dev/null; then
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain "$RUST_VERSION"
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
rustup default "$RUST_VERSION" >/dev/null

echo "==> repo $REF"
[ -d "$DIR/.git" ] || git clone -q "$REPO" "$DIR"
git -C "$DIR" fetch -q origin && git -C "$DIR" checkout -q "$REF" && git -C "$DIR" pull -q --ff-only || true
echo "commit: $(git -C "$DIR" rev-parse --short HEAD)"

echo "==> build"
(cd "$DIR/harness" && cargo build --release --locked -q)
"$DIR/harness/target/release/ctt" --version

echo "==> ssh key for server control"
[ -f /root/.ssh/ct_server ] || ssh-keygen -q -t ed25519 -N "" -C "ct-client" -f /root/.ssh/ct_server

cat <<EOF

Client ready. Last step, on the LOG SERVER (as root), authorize this key:

  echo '$(cat /root/.ssh/ct_server.pub)' >> /root/.ssh/authorized_keys

Then, back here:
  cd $DIR && export SERVER=<server private IP>
  ./scripts/netem.sh show          # route, qdiscs, RTT
EOF
