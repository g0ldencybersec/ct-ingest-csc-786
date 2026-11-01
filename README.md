# ct-ingest-csc-786

Measuring the client-side cost of ingesting Certificate Transparency logs
through the legacy RFC 6962 API versus the Static CT (tiled) API, on identical
log data. CSC 786 practical project.

## Layout

```
testbed/     CompactLog testbed (condition C1), Docker
harness/     Rust workspace
  crates/ctcore   shared decode / parse / Merkle-verify core (both APIs)
  crates/ctt      testbed tools: fetch, replay, gonogo
configs/     testbed pin and experiment configs (committed)
runs/        aggregate reports and event logs (committed)
data/        slices and snapshots (LOCAL ONLY, never committed)
```

## Data policy

No CT-derived data is committed to this repository: no certificates, tiles,
issuer certificates, slice manifests, or testbed snapshots. They live only in
`data/` on the machine running the experiments, enforced by `.gitignore`
(which also blocks certificate, tile, and archive file types anywhere).

What *is* committed is code, configuration, and aggregate results:

- `configs/testbed-c1.json`: tree size, root hash, signed tree head, snapshot
  SHA-256, CompactLog version, source log name, sampling seed, and entry
  counts. No certificate content.
- `runs/`: go/no-go and experiment reports (counts, sizes, timings, hashes).
  Event logs record indices, byte counts, and timings per request, never
  certificate bytes or hostnames.

Anyone can rebuild an equivalent slice from the source log URL and seed, and
check it against the snapshot hash recorded in the pin.

Local data is deleted at the end of the study (or between runs) with:

```sh
./scripts/cleanup.sh             # show what exists; deletes nothing
./scripts/cleanup.sh --all       # slices, snapshots, testbed volumes + image, build output
```

It asks for confirmation and never touches `configs/` or `runs/`.

## Milestone M1: testbed go/no-go

Goal: a private CompactLog instance, loaded with a slice of real public
CT data, that serves **identical** data through both APIs.

Requirements: Docker (OrbStack works), Rust toolchain.

### 1. Start the testbed

```sh
cd testbed
docker compose up -d --build     # first build takes several minutes
docker compose ps                # wait for "healthy"
curl -s http://127.0.0.1:8080/ct/v1/get-sth
```

The log listens on `127.0.0.1:8080` only. Certificate validation is disabled
(see `testbed/compactlog/Config.toml` for why).

### 2. Build the tools

```sh
cd harness
cargo test            # unit tests, including Merkle root vs. reference
cargo build --release
```

`scripts/setup-testbed.sh` also does this.

### 3–5. Fetch, replay, go/no-go (one script)

```sh
CTT_CONTACT="you@example.edu" RESET=1 ./scripts/setup-testbed.sh
```

Defaults: 1,000,000 entries (3,907 whole tiles) sampled uniformly at random
across Let's Encrypt Sycamore 2026h1 with seed 786. Sampling uses an
in-tree SplitMix64 + Floyd implementation (`ctcore::sample`), so the same seed
selects the same tiles forever; the seed is recorded in `manifest.json`.
`RESET=1` wipes the testbed first so slices are never mixed. Re-running skips
finished steps.

The testbed assigns its own timestamps and ordering, so its tree is a new
tree, not a copy of the source tree. Ground truth for every experiment is the
testbed's own signed tree head, pinned in the next step.

GO requires: STH and checkpoint agree; both APIs return every entry; at every
index the Merkle leaf, submitted certificate, and chain fingerprints are
identical; leaves from each API recompute to the pinned root; every issuer
matches its fingerprint.

### 6. Pin the testbed

```sh
./scripts/snapshot.sh     # freeze: data/snapshots/*.tgz + configs/testbed-c1.json
./scripts/restore.sh      # later: restore and verify against the pin
```

`snapshot.sh` stops the log, archives its data and signing keys, restarts it,
and only pins the tree if it is unchanged after the restart.
`configs/testbed-c1.json` (committed; no certificate content) records the pinned size and root, the
snapshot's SHA-256, the CompactLog commit and image, and the slice provenance.

If CompactLog cannot pass, the fallback is a local Sunlight log with the
Sunglasses RFC 6962 proxy in front of it (still identical data).

## Testbed behaviors that affect measurement

Found by reading CompactLog at the pinned commit (`1ceab9f`):

- **Read warm-up throttle:** reads are limited to 5 concurrent for the first
  30 s after startup. Trials must start after that window.
- **Response compression:** every response, including RFC 6962 JSON, is
  compressed according to the client's `Accept-Encoding`. The harness must set
  and record this header explicitly. (`ctt` sends none, so JSON is identity.)
- **Data tiles are generated on demand** from the database and gzipped, not
  served as static files. C1 results describe CompactLog's implementation,
  not CDN-style static serving.
- **`get-entries` returns at most 1000 entries** per request.
- **Writes are batched** (~500 ms), and resubmitted certificates are
  de-duplicated by hash.
