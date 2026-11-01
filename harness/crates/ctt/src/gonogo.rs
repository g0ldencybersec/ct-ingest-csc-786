//! `ctt gonogo`: the M1 testbed acceptance check.
//!
//! GO requires all of the following on a quiescent testbed of size N:
//!   1. RFC 6962 STH and Static CT checkpoint report the same size and root.
//!   2. Both APIs return exactly N entries (complete).
//!   3. At every index, the two APIs give byte-identical Merkle leaves, the
//!      same submitted certificate, and the same chain fingerprints.
//!   4. Leaf hashes from EACH API recompute to the pinned root.
//!   5. Every issuer referenced by a tile is served at /issuer/<fp> with
//!      content matching its fingerprint.
//!
//! Timing and byte counts are recorded for information only; they are NOT a
//! measurement (no network control, single pass, no warm-up handling).

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use futures::{stream, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};

use ctcore::checkpoint::{parse_checkpoint, SthJson, TreeHead};
use ctcore::entry::{parse_data_tile, parse_rfc6962_entry};
use ctcore::tile::{data_tile_path, tiles_for};
use ctcore::{maybe_gunzip, sha256, Entry, EntryKind, Frontier, Hash};

use crate::http::{client, get_bytes, join};

/// What we keep per index from each API (compact digests).
#[derive(Clone, PartialEq, Eq)]
struct Digest {
    leaf: Hash,
    kind: EntryKind,
    submitted: Hash,
    chain: Vec<[u8; 32]>,
}

impl From<&Entry> for Digest {
    fn from(e: &Entry) -> Self {
        Digest {
            leaf: e.leaf_hash(),
            kind: e.kind,
            submitted: sha256(&e.submitted_cert),
            chain: e.chain_fingerprints.clone(),
        }
    }
}

#[derive(Default, Serialize)]
struct PathStats {
    entries: u64,
    requests: u64,
    bytes_on_wire: u64,
    wall_secs: f64,
    computed_root_b64: String,
}

#[derive(Default, Serialize)]
struct Mismatches {
    leaf: u64,
    kind: u64,
    submitted_cert: u64,
    chain: u64,
    first_bad_index: Option<u64>,
}

#[derive(Deserialize)]
struct GetEntries {
    entries: Vec<RawEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    leaf_input: String,
    extra_data: String,
}

pub async fn run(log: &str, out: &Path, concurrency: usize) -> Result<bool> {
    let http = client(None)?;

    // 0. Wait for the log to be up (it may be restarting or still compacting).
    wait_healthy(&http, log, Duration::from_secs(600)).await?;

    // 1. Quiescent, agreeing tree heads.
    let (sth_head, cp_text) = stable_heads(&http, log).await?;
    let n = sth_head.tree_size;
    eprintln!("pinned tree: size {n}, root {}", B64.encode(sth_head.root_hash));
    if n == 0 {
        bail!("testbed log is empty; run `ctt replay` first");
    }

    // 2a. RFC 6962 path.
    let t0 = Instant::now();
    let mut rfc = PathStats::default();
    let ranges: Vec<(u64, u64)> = (0..n).step_by(1000).map(|s| (s, (s + 1000).min(n))).collect();
    let total_ranges = ranges.len();
    let rfc_progress = Progress::new("rfc6962", total_ranges, 1000);
    let chunks: Vec<(Vec<Digest>, u64, u64)> = stream::iter(ranges)
        .map(|(start, end)| {
            let p = rfc_progress.clone();
            let http = http.clone();
            async move {
                let r = fetch_rfc_range(&http, log, start, end).await;
                p.tick();
                r
            }
        })
        .buffered(concurrency)
        .try_collect()
        .await?;
    let mut rfc_digests = Vec::with_capacity(n as usize);
    for (d, reqs, bytes) in chunks {
        rfc.requests += reqs;
        rfc.bytes_on_wire += bytes;
        rfc_digests.extend(d);
    }
    rfc.wall_secs = t0.elapsed().as_secs_f64();
    rfc.entries = rfc_digests.len() as u64;
    eprintln!("rfc6962: {} entries in {:.1}s", rfc.entries, rfc.wall_secs);

    // 2b. Static CT path.
    let t0 = Instant::now();
    let mut tiled = PathStats::default();
    let tile_plan = tiles_for(n);
    let tile_progress = Progress::new("static-ct", tile_plan.len(), 256);
    let tiles: Vec<(Vec<Digest>, u64)> = stream::iter(tile_plan)
        .map(|(index, width)| {
            let http = http.clone();
            let url = join(log, &data_tile_path(index, width));
            let p = tile_progress.clone();
            async move {
                let raw = get_bytes(&http, &url).await?;
                let entries = parse_data_tile(&maybe_gunzip(&raw)?)?;
                p.tick();
                Ok::<_, anyhow::Error>((entries.iter().map(Digest::from).collect(), raw.len() as u64))
            }
        })
        .buffered(concurrency)
        .try_collect()
        .await?;
    let mut tile_digests = Vec::with_capacity(n as usize);
    for (d, bytes) in tiles {
        tiled.requests += 1;
        tiled.bytes_on_wire += bytes;
        tile_digests.extend(d);
    }
    // Issuers are part of the tiled read path's cost and completeness.
    let fps: BTreeSet<[u8; 32]> = tile_digests.iter().flat_map(|d| d.chain.iter().copied()).collect();
    let issuer_results: Vec<(bool, u64)> = stream::iter(fps.iter().copied())
        .map(|fp| {
            let http = http.clone();
            let url = join(log, &format!("issuer/{}", hex::encode(fp)));
            async move {
                let der = get_bytes(&http, &url).await?;
                Ok::<_, anyhow::Error>((sha256(&der) == fp, der.len() as u64))
            }
        })
        .buffer_unordered(concurrency)
        .try_collect()
        .await?;
    let bad_issuers = issuer_results.iter().filter(|(ok, _)| !ok).count();
    tiled.requests += issuer_results.len() as u64;
    tiled.bytes_on_wire += issuer_results.iter().map(|(_, b)| b).sum::<u64>();
    tiled.wall_secs = t0.elapsed().as_secs_f64();
    tiled.entries = tile_digests.len() as u64;
    eprintln!("static-ct: {} entries, {} issuers in {:.1}s", tiled.entries, fps.len(), tiled.wall_secs);

    // 3. Per-index comparison.
    let mut mm = Mismatches::default();
    for (i, (a, b)) in rfc_digests.iter().zip(tile_digests.iter()).enumerate() {
        let mut bad = false;
        if a.leaf != b.leaf { mm.leaf += 1; bad = true; }
        if a.kind != b.kind { mm.kind += 1; bad = true; }
        if a.submitted != b.submitted { mm.submitted_cert += 1; bad = true; }
        if a.chain != b.chain { mm.chain += 1; bad = true; }
        if bad && mm.first_bad_index.is_none() {
            mm.first_bad_index = Some(i as u64);
        }
    }

    // 4. Recompute roots independently from each path.
    let root_of = |ds: &[Digest]| {
        let mut f = Frontier::new();
        ds.iter().for_each(|d| f.push(d.leaf));
        f.root()
    };
    let rfc_root = root_of(&rfc_digests);
    let tile_root = root_of(&tile_digests);
    rfc.computed_root_b64 = B64.encode(rfc_root);
    tiled.computed_root_b64 = B64.encode(tile_root);

    // 5. Verdict.
    let mut failures = Vec::new();
    if rfc.entries != n { failures.push(format!("rfc6962 returned {} of {n} entries", rfc.entries)); }
    if tiled.entries != n { failures.push(format!("static-ct returned {} of {n} entries", tiled.entries)); }
    if mm.leaf + mm.kind + mm.submitted_cert + mm.chain > 0 { failures.push("per-index mismatches between APIs".into()); }
    if rfc_root != sth_head.root_hash { failures.push("rfc6962 leaves do not recompute to pinned root".into()); }
    if tile_root != sth_head.root_hash { failures.push("static-ct leaves do not recompute to pinned root".into()); }
    if bad_issuers > 0 { failures.push(format!("{bad_issuers} issuers do not match their fingerprint")); }
    let go = failures.is_empty();

    let report = serde_json::json!({
        "verdict": if go { "GO" } else { "NO-GO" },
        "failures": failures,
        "log": log,
        "checked_at": chrono::Utc::now().to_rfc3339(),
        "pinned": {
            "tree_size": n,
            "root_hash_b64": B64.encode(sth_head.root_hash),
            "checkpoint": cp_text,
        },
        "mismatches": mm,
        "distinct_issuers": fps.len(),
        "rfc6962": rfc,
        "static_ct": tiled,
        "informational_note": "requests/bytes/wall_secs are a single uncontrolled pass; not experimental results",
    });
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(out, serde_json::to_vec_pretty(&report)?)?;

    eprintln!();
    if go {
        eprintln!("GO: both APIs serve identical data for all {n} entries and recompute the pinned root.");
    } else {
        eprintln!("NO-GO:");
        for f in report["failures"].as_array().unwrap() {
            eprintln!("  - {}", f.as_str().unwrap());
        }
    }
    eprintln!("report: {}", out.display());
    Ok(go)
}

/// Prints progress about every 5% (and at most every 10 s) with an ETA.
#[derive(Clone)]
struct Progress {
    inner: std::sync::Arc<ProgressInner>,
}

struct ProgressInner {
    label: &'static str,
    total: usize,
    per_unit: u64,
    done: std::sync::atomic::AtomicUsize,
    start: Instant,
    last_print: std::sync::Mutex<Instant>,
}

impl Progress {
    fn new(label: &'static str, total: usize, per_unit: u64) -> Self {
        Progress {
            inner: std::sync::Arc::new(ProgressInner {
                label,
                total,
                per_unit,
                done: std::sync::atomic::AtomicUsize::new(0),
                start: Instant::now(),
                last_print: std::sync::Mutex::new(Instant::now()),
            }),
        }
    }

    fn tick(&self) {
        let p = &self.inner;
        let done = p.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let step = (p.total / 20).max(1);
        let mut last = p.last_print.lock().unwrap();
        #[allow(clippy::manual_is_multiple_of)]
        let due = done % step == 0 || last.elapsed() >= Duration::from_secs(10) || done == p.total;
        if !due {
            return;
        }
        *last = Instant::now();
        let secs = p.start.elapsed().as_secs_f64();
        let rate = done as f64 / secs.max(1e-9);
        let eta = (p.total - done) as f64 / rate.max(1e-9);
        eprintln!(
            "  {}: {done}/{} requests ({:.0}%), ~{:.0} entries/s, ETA {:.0}s",
            p.label,
            p.total,
            100.0 * done as f64 / p.total as f64,
            rate * p.per_unit as f64,
            eta
        );
    }
}

/// Poll `/health` until the log answers, up to `limit`.
async fn wait_healthy(http: &reqwest::Client, log: &str, limit: Duration) -> Result<()> {
    let start = Instant::now();
    let url = join(log, "health");
    let mut warned = false;
    loop {
        if let Ok(r) = http.get(&url).send().await {
            if r.status().is_success() {
                return Ok(());
            }
        }
        if start.elapsed() > limit {
            bail!("log at {log} not healthy after {:?}", limit);
        }
        if !warned {
            eprintln!("waiting for {log} to become healthy...");
            warned = true;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Read STH and checkpoint until they agree (the log may still be publishing
/// a batch). Returns the pinned head and the checkpoint text.
async fn stable_heads(http: &reqwest::Client, log: &str) -> Result<(TreeHead, String)> {
    for _ in 0..30 {
        let sth: SthJson = serde_json::from_slice(&get_bytes(http, &join(log, "ct/v1/get-sth")).await?)?;
        let cp_text = String::from_utf8(get_bytes(http, &join(log, "checkpoint")).await?)?;
        let cp = parse_checkpoint(&cp_text)?;
        let head = sth.head()?;
        if head == cp.head {
            return Ok((head, cp_text));
        }
        eprintln!(
            "waiting: STH size {} vs checkpoint size {}",
            head.tree_size, cp.head.tree_size
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bail!("STH and checkpoint never agreed; is something still writing to the log?")
}

/// Fetch `[start, end)` via get-entries, following server-side truncation.
async fn fetch_rfc_range(
    http: &reqwest::Client,
    log: &str,
    start: u64,
    end: u64,
) -> Result<(Vec<Digest>, u64, u64)> {
    let mut out = Vec::with_capacity((end - start) as usize);
    let (mut reqs, mut bytes) = (0u64, 0u64);
    let mut next = start;
    while next < end {
        let url = join(log, &format!("ct/v1/get-entries?start={next}&end={}", end - 1));
        let body = get_bytes(http, &url).await?;
        reqs += 1;
        bytes += body.len() as u64;
        let parsed: GetEntries = serde_json::from_slice(&body)?;
        if parsed.entries.is_empty() {
            bail!("get-entries returned no entries at {next}");
        }
        for e in parsed.entries {
            let leaf = B64.decode(e.leaf_input)?;
            let extra = B64.decode(e.extra_data)?;
            let (entry, _chain) = parse_rfc6962_entry(&leaf, &extra)?;
            out.push(Digest::from(&entry));
            next += 1;
        }
    }
    Ok((out, reqs, bytes))
}
