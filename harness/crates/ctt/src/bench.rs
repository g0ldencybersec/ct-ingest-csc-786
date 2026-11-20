//! `ctt bench`: one measured C1 trial, and `ctt blocks`: its reference.
//!
//! A trial reads one aligned block of 2^k entries (default 2^17 = 131,072 =
//! 512 full tiles) through ONE read path ("arm"), decodes every entry with
//! the shared core, and verifies the block's Merkle subtree root against the
//! reference in `configs/testbed-c1-blocks.json`. Because the block is
//! aligned and a power of two, its root is an internal node of the pinned
//! tree, so a trial that returns wrong or incomplete data cannot pass.
//!
//! Arms:
//!   legacy-gzip      RFC 6962 get-entries, `Accept-Encoding: gzip` (primary)
//!   legacy-identity  RFC 6962 get-entries, `Accept-Encoding: identity` (diagnostic)
//!   tiles            Static CT data tiles + issuers, `Accept-Encoding: gzip` (primary)
//!
//! One trial per process, so peak RSS and CPU time belong to that trial.
//! Writes a per-request event log (JSONL) and a summary (JSON). Neither
//! contains certificate bytes, hostnames, or issuer fingerprints.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use futures::{stream, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};

use ctcore::checkpoint::SthJson;
use ctcore::entry::{parse_data_tile, parse_rfc6962_entry};
use ctcore::tile::{data_tile_path, tiles_for, TILE_WIDTH};
use ctcore::{sha256, EntryKind, Frontier, Hash};

use crate::http::{client, get_bytes, get_measured, join, strip_gzip, Event};
use crate::sys;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Arm {
    LegacyGzip,
    LegacyIdentity,
    Tiles,
}

impl Arm {
    pub fn name(self) -> &'static str {
        match self {
            Arm::LegacyGzip => "legacy-gzip",
            Arm::LegacyIdentity => "legacy-identity",
            Arm::Tiles => "tiles",
        }
    }
    fn accept_encoding(self) -> &'static str {
        match self {
            Arm::LegacyIdentity => "identity",
            _ => "gzip",
        }
    }
}

// ---------------------------------------------------------------------------
// Reference blocks
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub struct BlockRef {
    pub block: u64,
    pub first: u64,
    pub entries: u64,
    pub root_b64: String,
    pub x509: u64,
    pub precert: u64,
    pub distinct_issuers: u64,
}

#[derive(Serialize, Deserialize)]
pub struct Blocks {
    pub tree_size: u64,
    pub root_hash_b64: String,
    pub block_log2: u32,
    pub blocks: Vec<BlockRef>,
    pub computed_at: String,
    pub note: String,
}

/// Read the whole testbed via tiles, verify the full root against the STH,
/// and record the subtree root of every complete aligned block.
pub async fn blocks(log: &str, block_log2: u32, out: &Path, concurrency: usize) -> Result<()> {
    let http = client(None)?;
    let sth: SthJson = serde_json::from_slice(&get_bytes(&http, &join(log, "ct/v1/get-sth")).await?)?;
    let head = sth.head()?;
    let n = head.tree_size;
    let bsize = 1u64 << block_log2;
    if bsize % TILE_WIDTH != 0 {
        bail!("block size must be a multiple of {TILE_WIDTH}");
    }
    eprintln!("reading {n} entries via tiles to compute {}-entry block roots", bsize);
    let per_tile: Vec<Vec<(Hash, EntryKind, Vec<[u8; 32]>)>> = stream::iter(tiles_for(n))
        .map(|(idx, w)| {
            let http = http.clone();
            let url = join(log, &data_tile_path(idx, w));
            async move {
                let (body, _) = strip_gzip(get_bytes(&http, &url).await?)?;
                let es = parse_data_tile(&body)?;
                Ok::<_, anyhow::Error>(
                    es.into_iter()
                        .map(|e| (e.leaf_hash(), e.kind, e.chain_fingerprints))
                        .collect(),
                )
            }
        })
        .buffered(concurrency)
        .try_collect()
        .await?;

    let mut full = Frontier::new();
    let mut out_blocks = Vec::new();
    let mut cur = Frontier::new();
    let (mut x509, mut pre) = (0u64, 0u64);
    let mut fps: BTreeSet<[u8; 32]> = BTreeSet::new();
    for (leaf, kind, chain) in per_tile.into_iter().flatten() {
        full.push(leaf);
        cur.push(leaf);
        match kind {
            EntryKind::X509 => x509 += 1,
            EntryKind::Precert => pre += 1,
        }
        fps.extend(chain);
        if cur.size() == bsize {
            let b = out_blocks.len() as u64;
            out_blocks.push(BlockRef {
                block: b,
                first: b * bsize,
                entries: bsize,
                root_b64: B64.encode(cur.root()),
                x509,
                precert: pre,
                distinct_issuers: fps.len() as u64,
            });
            cur = Frontier::new();
            x509 = 0;
            pre = 0;
            fps.clear();
        }
    }
    if full.root() != head.root_hash || full.size() != n {
        bail!("tiles do not recompute to the STH root; testbed is not the pinned tree");
    }
    let doc = Blocks {
        tree_size: n,
        root_hash_b64: B64.encode(head.root_hash),
        block_log2,
        blocks: out_blocks,
        computed_at: chrono::Utc::now().to_rfc3339(),
        note: "Merkle subtree roots of aligned blocks (internal nodes of the pinned tree). No certificate content.".into(),
    };
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(out, serde_json::to_vec_pretty(&doc)?)?;
    eprintln!("full root verified; {} blocks written to {}", doc.blocks.len(), out.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// One trial
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct GetEntries {
    entries: Vec<RawEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    leaf_input: String,
    extra_data: String,
}

struct Part {
    leaves: Vec<Hash>,
    kinds: (u64, u64),
    fps: BTreeSet<[u8; 32]>,
    events: Vec<Event>,
}

pub struct BenchArgs {
    pub log: String,
    pub arm: Arm,
    pub block: u64,
    pub blocks_file: PathBuf,
    pub concurrency: usize,
    pub trial_id: String,
    pub events: PathBuf,
    pub summary: PathBuf,
    pub meta: Vec<String>,
}

pub async fn bench(a: BenchArgs) -> Result<bool> {
    let refs: Blocks = serde_json::from_slice(
        &std::fs::read(&a.blocks_file).with_context(|| format!("reading {}", a.blocks_file.display()))?,
    )?;
    let bref = refs
        .blocks
        .iter()
        .find(|b| b.block == a.block)
        .with_context(|| format!("block {} not in {}", a.block, a.blocks_file.display()))?
        .clone();
    let (first, end) = (bref.first, bref.first + bref.entries);
    let http = client(None)?;
    let ae = a.arm.accept_encoding();

    let cpu0 = sys::cpu_ticks();
    let ru0 = sys::rusage();
    let started_at = chrono::Utc::now().to_rfc3339();
    let epoch = Instant::now();

    let parts: Vec<Part> = match a.arm {
        Arm::LegacyGzip | Arm::LegacyIdentity => {
            let ranges: Vec<(u64, u64)> = (first..end).step_by(1000).map(|s| (s, (s + 1000).min(end))).collect();
            stream::iter(ranges)
                .map(|(s, e)| legacy_range(&http, &a.log, s, e, ae, epoch))
                .buffered(a.concurrency)
                .try_collect()
                .await?
        }
        Arm::Tiles => {
            let t0 = first / TILE_WIDTH;
            let t1 = end / TILE_WIDTH;
            stream::iter(t0..t1)
                .map(|t| tile(&http, &a.log, t, ae, epoch))
                .buffered(a.concurrency)
                .try_collect()
                .await?
        }
    };

    let mut f = Frontier::new();
    let (mut x509, mut pre) = (0u64, 0u64);
    let mut fps = BTreeSet::new();
    let mut events = Vec::new();
    for p in parts {
        p.leaves.iter().for_each(|l| f.push(*l));
        x509 += p.kinds.0;
        pre += p.kinds.1;
        fps.extend(p.fps);
        events.extend(p.events);
    }

    // Tiles carry only issuer fingerprints: fetch each distinct issuer once
    // (a fresh monitor has no issuer cache) and check it against its hash.
    let mut bad_issuers = 0u64;
    if a.arm == Arm::Tiles {
        let res: Vec<(bool, Event)> = stream::iter(fps.iter().copied())
            .map(|fp| {
                let http = http.clone();
                let url = join(&a.log, &format!("issuer/{}", hex::encode(fp)));
                async move {
                    let (body, mut ev) = get_measured(&http, &url, Some(ae), epoch, "issuer", None).await?;
                    let (der, layers) = strip_gzip(body)?;
                    ev.decoded_bytes = der.len() as u64;
                    ev.gzip_layers = layers;
                    Ok::<_, anyhow::Error>((sha256(&der) == fp, ev))
                }
            })
            .buffer_unordered(a.concurrency)
            .try_collect()
            .await?;
        for (ok, ev) in res {
            bad_issuers += (!ok) as u64;
            events.push(ev);
        }
    }

    let wall = epoch.elapsed().as_secs_f64();
    let ru1 = sys::rusage();
    let cpu1 = sys::cpu_ticks();

    let root = B64.encode(f.root());
    let mut failures = Vec::new();
    if f.size() != bref.entries {
        failures.push(format!("got {} of {} entries", f.size(), bref.entries));
    }
    if root != bref.root_b64 {
        failures.push("block root does not match reference".to_string());
    }
    if (x509, pre) != (bref.x509, bref.precert) {
        failures.push("entry-type counts differ from reference".to_string());
    }
    if bad_issuers > 0 {
        failures.push(format!("{bad_issuers} issuers do not match their fingerprint"));
    }
    let verified = failures.is_empty();

    // Aggregate by request kind.
    let mut by_kind: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
    for kind in ["get-entries", "tile", "issuer"] {
        let ev: Vec<&Event> = events.iter().filter(|e| e.kind == kind).collect();
        if ev.is_empty() {
            continue;
        }
        let mut enc: BTreeMap<String, u64> = BTreeMap::new();
        let mut layers: BTreeMap<u8, u64> = BTreeMap::new();
        for e in &ev {
            let k = if e.content_encoding.is_empty() { "none".to_string() } else { e.content_encoding.clone() };
            *enc.entry(k).or_default() += 1;
            *layers.entry(e.gzip_layers).or_default() += 1;
        }
        let mut ttfb: Vec<f64> = ev.iter().map(|e| e.t_headers_ms - e.t_start_ms).collect();
        ttfb.sort_by(|a, b| a.partial_cmp(b).unwrap());
        by_kind.insert(
            kind,
            serde_json::json!({
                "requests": ev.len(),
                "wire_bytes": ev.iter().map(|e| e.wire_bytes).sum::<u64>(),
                "decoded_bytes": ev.iter().map(|e| e.decoded_bytes).sum::<u64>(),
                "retries": ev.iter().map(|e| (e.attempts - 1) as u64).sum::<u64>(),
                "content_encoding_received": enc,
                "gzip_layers": layers,
                "ttfb_ms_median": ttfb[ttfb.len() / 2],
            }),
        );
    }
    let meta: BTreeMap<String, String> = a
        .meta
        .iter()
        .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect();

    let summary = serde_json::json!({
        "trial_id": a.trial_id,
        "arm": a.arm.name(),
        "accept_encoding_sent": ae,
        "block": a.block,
        "first_index": first,
        "entries": f.size(),
        "x509": x509,
        "precert": pre,
        "distinct_issuers": fps.len(),
        "concurrency": a.concurrency,
        "started_at": started_at,
        "wall_secs": wall,
        "entries_per_sec": f.size() as f64 / wall,
        "requests": events.len(),
        "wire_bytes": events.iter().map(|e| e.wire_bytes).sum::<u64>(),
        "decoded_bytes": events.iter().map(|e| e.decoded_bytes).sum::<u64>(),
        "retries": events.iter().map(|e| (e.attempts - 1) as u64).sum::<u64>(),
        "by_kind": by_kind,
        "client": {
            "cpu_user_secs": ru1.user_secs - ru0.user_secs,
            "cpu_sys_secs": ru1.sys_secs - ru0.sys_secs,
            "max_rss_kib": ru1.max_rss_kib,
            "cpu_share": cpu0.zip(cpu1).and_then(|(a, b)| sys::cpu_share(a, b)),
            "nproc": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        },
        "verified": verified,
        "failures": failures,
        "block_root_b64": root,
        "harness_version": env!("CARGO_PKG_VERSION"),
        "meta": meta,
    });

    for p in [&a.events, &a.summary] {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
    }
    let mut buf = Vec::new();
    for e in &events {
        serde_json::to_writer(&mut buf, e)?;
        buf.push(b'\n');
    }
    std::fs::write(&a.events, buf)?;
    std::fs::write(&a.summary, serde_json::to_vec_pretty(&summary)?)?;

    eprintln!(
        "{} block {}: {} entries in {:.2}s, {} requests, {:.1} MB on wire, client CPU {:.2}s, {}",
        a.arm.name(),
        a.block,
        f.size(),
        wall,
        events.len(),
        summary["wire_bytes"].as_u64().unwrap_or(0) as f64 / 1e6,
        ru1.user_secs - ru0.user_secs + ru1.sys_secs - ru0.sys_secs,
        if verified { "VERIFIED".to_string() } else { format!("FAILED: {}", failures.join("; ")) }
    );
    Ok(verified)
}

async fn legacy_range(
    http: &reqwest::Client,
    log: &str,
    start: u64,
    end: u64,
    ae: &str,
    epoch: Instant,
) -> Result<Part> {
    let mut p = Part { leaves: Vec::with_capacity((end - start) as usize), kinds: (0, 0), fps: BTreeSet::new(), events: Vec::new() };
    let mut next = start;
    while next < end {
        let url = join(log, &format!("ct/v1/get-entries?start={next}&end={}", end - 1));
        let (body, mut ev) = get_measured(http, &url, Some(ae), epoch, "get-entries", Some(next)).await?;
        let (json, layers) = strip_gzip(body)?;
        ev.decoded_bytes = json.len() as u64;
        ev.gzip_layers = layers;
        let parsed: GetEntries = serde_json::from_slice(&json)?;
        if parsed.entries.is_empty() {
            bail!("get-entries returned no entries at {next}");
        }
        ev.entries = parsed.entries.len() as u64;
        for e in parsed.entries {
            let leaf = B64.decode(e.leaf_input)?;
            let extra = B64.decode(e.extra_data)?;
            let (entry, _chain) = parse_rfc6962_entry(&leaf, &extra)?;
            p.leaves.push(entry.leaf_hash());
            match entry.kind {
                EntryKind::X509 => p.kinds.0 += 1,
                EntryKind::Precert => p.kinds.1 += 1,
            }
            p.fps.extend(entry.chain_fingerprints);
            next += 1;
        }
        p.events.push(ev);
    }
    Ok(p)
}

async fn tile(http: &reqwest::Client, log: &str, t: u64, ae: &str, epoch: Instant) -> Result<Part> {
    let url = join(log, &data_tile_path(t, None));
    let (body, mut ev) = get_measured(http, &url, Some(ae), epoch, "tile", Some(t * TILE_WIDTH)).await?;
    let (raw, layers) = strip_gzip(body)?;
    ev.decoded_bytes = raw.len() as u64;
    ev.gzip_layers = layers;
    let entries = parse_data_tile(&raw)?;
    ev.entries = entries.len() as u64;
    let mut p = Part { leaves: Vec::with_capacity(256), kinds: (0, 0), fps: BTreeSet::new(), events: vec![ev] };
    for e in entries {
        p.leaves.push(e.leaf_hash());
        match e.kind {
            EntryKind::X509 => p.kinds.0 += 1,
            EntryKind::Precert => p.kinds.1 += 1,
        }
        p.fps.extend(e.chain_fingerprints);
    }
    Ok(p)
}
