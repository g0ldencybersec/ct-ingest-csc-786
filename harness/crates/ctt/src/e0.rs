//! `ctt e0`: offline encoding analysis (sizes only, no timing).
//!
//! For each reference block, reads the same entries through every
//! representation and breaks the bytes down, so the C1 byte gap can be split
//! into an ENCODING part (base64 + JSON framing, full chains repeated per
//! entry vs. fingerprints + one copy per issuer) and a COMPRESSION part
//! (gzip applied to each).
//!
//! Representations per block:
//!   A  legacy JSON, identity (as served)
//!   A' legacy JSON, gzip as served by the log (actual Content-Encoding recorded)
//!   A6 legacy JSON, gzip level 6 applied locally per response (reference)
//!   B  legacy binary: base64-decoded leaf_input + extra_data (no JSON, no base64)
//!   B6 legacy binary, gzip level 6 per 1,000-entry response
//!   C  tiles, decompressed, plus each distinct issuer once
//!   C' tiles as served (gzip) plus issuers as served
//!
//! Output holds byte counts only.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use futures::{stream, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};

use ctcore::entry::{parse_data_tile, parse_rfc6962_entry};
use ctcore::tile::{data_tile_path, TILE_WIDTH};
use ctcore::{sha256, EntryKind, Frontier};

use crate::bench::Blocks;
use crate::http::{client, get_measured, join, strip_gzip};

#[derive(Deserialize)]
struct GetEntries {
    entries: Vec<RawEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    leaf_input: String,
    extra_data: String,
}

#[derive(Default, Serialize, Clone)]
pub struct Sizes {
    pub entries: u64,
    pub x509: u64,
    pub precert: u64,
    // Legacy JSON (A) and its breakdown.
    pub legacy_requests: u64,
    pub legacy_json_bytes: u64,
    pub legacy_b64_chars: u64,
    pub legacy_json_framing_bytes: u64,
    pub legacy_raw_bytes: u64,
    pub legacy_base64_overhead_bytes: u64,
    pub legacy_leaf_input_bytes: u64,
    pub legacy_extra_precert_bytes: u64,
    pub legacy_extra_chain_bytes: u64,
    pub legacy_chain_certs: u64,
    // Compressed legacy.
    pub legacy_gzip_served_bytes: u64,
    pub legacy_gzip_local6_bytes: u64,
    pub legacy_binary_gzip6_bytes: u64,
    pub legacy_gzip_content_encodings: BTreeMap<String, u64>,
    // Tiles (C) and breakdown.
    pub tile_requests: u64,
    pub tile_served_bytes: u64,
    pub tile_decoded_bytes: u64,
    pub tile_timestamped_entry_bytes: u64,
    pub tile_precert_bytes: u64,
    pub tile_fingerprint_bytes: u64,
    pub tile_content_encodings: BTreeMap<String, u64>,
    pub tile_gzip_layers: BTreeMap<u8, u64>,
    pub issuers: u64,
    pub issuer_served_bytes: u64,
    pub issuer_der_bytes: u64,
}

impl Sizes {
    fn add(&mut self, o: &Sizes) {
        macro_rules! s { ($($f:ident),*) => { $( self.$f += o.$f; )* } }
        s!(entries, x509, precert, legacy_requests, legacy_json_bytes, legacy_b64_chars,
           legacy_json_framing_bytes, legacy_raw_bytes, legacy_base64_overhead_bytes,
           legacy_leaf_input_bytes, legacy_extra_precert_bytes, legacy_extra_chain_bytes,
           legacy_chain_certs, legacy_gzip_served_bytes, legacy_gzip_local6_bytes,
           legacy_binary_gzip6_bytes, tile_requests, tile_served_bytes, tile_decoded_bytes,
           tile_timestamped_entry_bytes, tile_precert_bytes, tile_fingerprint_bytes,
           issuer_served_bytes, issuer_der_bytes);
        // Issuers are per block (fresh monitor); summing counts is meaningful
        // as "issuer fetches if each block were read independently".
        self.issuers += o.issuers;
        for (k, v) in &o.legacy_gzip_content_encodings { *self.legacy_gzip_content_encodings.entry(k.clone()).or_default() += v; }
        for (k, v) in &o.tile_content_encodings { *self.tile_content_encodings.entry(k.clone()).or_default() += v; }
        for (k, v) in &o.tile_gzip_layers { *self.tile_gzip_layers.entry(*k).or_default() += v; }
    }
}

fn gzip6(data: &[u8]) -> Result<u64> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    enc.write_all(data)?;
    Ok(enc.finish()?.len() as u64)
}

struct LegacyChunk {
    s: Sizes,
    leaves: Vec<ctcore::Hash>,
}

async fn legacy_chunk(http: &reqwest::Client, log: &str, start: u64, end: u64) -> Result<LegacyChunk> {
    let epoch = Instant::now();
    let mut s = Sizes::default();
    let mut leaves = Vec::new();
    let mut next = start;
    while next < end {
        // Identity.
        let url = join(log, &format!("ct/v1/get-entries?start={next}&end={}", end - 1));
        let (body, _) = get_measured(http, &url, Some("identity"), epoch, "get-entries", Some(next)).await?;
        let (json, _) = strip_gzip(body)?;
        let parsed: GetEntries = serde_json::from_slice(&json)?;
        let got = parsed.entries.len() as u64;
        if got == 0 {
            bail!("get-entries returned no entries at {next}");
        }
        s.legacy_requests += 1;
        s.legacy_json_bytes += json.len() as u64;
        s.legacy_gzip_local6_bytes += gzip6(&json)?;
        let mut binary = Vec::new();
        for e in &parsed.entries {
            s.legacy_b64_chars += (e.leaf_input.len() + e.extra_data.len()) as u64;
            let leaf = B64.decode(&e.leaf_input)?;
            let extra = B64.decode(&e.extra_data)?;
            let (entry, chain) = parse_rfc6962_entry(&leaf, &extra)?;
            s.entries += 1;
            match entry.kind {
                EntryKind::X509 => s.x509 += 1,
                EntryKind::Precert => {
                    s.precert += 1;
                    s.legacy_extra_precert_bytes += entry.submitted_cert.len() as u64 + 3;
                }
            }
            s.legacy_leaf_input_bytes += leaf.len() as u64;
            s.legacy_extra_chain_bytes += 3 + chain.iter().map(|c| c.len() as u64 + 3).sum::<u64>();
            s.legacy_chain_certs += chain.len() as u64;
            s.legacy_raw_bytes += (leaf.len() + extra.len()) as u64;
            binary.extend_from_slice(&leaf);
            binary.extend_from_slice(&extra);
            leaves.push(entry.leaf_hash());
        }
        s.legacy_binary_gzip6_bytes += gzip6(&binary)?;

        // Same range, gzip as served.
        let url = join(log, &format!("ct/v1/get-entries?start={next}&end={}", next + got - 1));
        let (body, ev) = get_measured(http, &url, Some("gzip"), epoch, "get-entries", Some(next)).await?;
        s.legacy_gzip_served_bytes += body.len() as u64;
        let k = if ev.content_encoding.is_empty() { "none".into() } else { ev.content_encoding.clone() };
        *s.legacy_gzip_content_encodings.entry(k).or_default() += 1;
        let (json2, _) = strip_gzip(body)?;
        if json2 != json {
            bail!("gzip and identity responses differ at {next}");
        }
        next += got;
    }
    s.legacy_base64_overhead_bytes = s.legacy_b64_chars - s.legacy_raw_bytes;
    s.legacy_json_framing_bytes = s.legacy_json_bytes - s.legacy_b64_chars;
    Ok(LegacyChunk { s, leaves })
}

pub async fn run(log: &str, blocks_file: &Path, which: Option<Vec<u64>>, out: &PathBuf, concurrency: usize) -> Result<()> {
    let refs: Blocks = serde_json::from_slice(
        &std::fs::read(blocks_file).with_context(|| format!("reading {}", blocks_file.display()))?,
    )?;
    let http = client(None)?;
    let mut per_block = Vec::new();
    let mut total = Sizes::default();
    for b in &refs.blocks {
        if let Some(w) = &which {
            if !w.contains(&b.block) {
                continue;
            }
        }
        let (first, end) = (b.first, b.first + b.entries);
        eprintln!("e0: block {} [{first}, {end})", b.block);

        // Legacy.
        let ranges: Vec<(u64, u64)> = (first..end).step_by(1000).map(|s| (s, (s + 1000).min(end))).collect();
        let chunks: Vec<LegacyChunk> = stream::iter(ranges)
            .map(|(s, e)| legacy_chunk(&http, log, s, e))
            .buffered(concurrency)
            .try_collect()
            .await?;
        let mut s = Sizes::default();
        let mut fl = Frontier::new();
        for c in &chunks {
            s.add(&c.s);
            c.leaves.iter().for_each(|l| fl.push(*l));
        }
        s.legacy_base64_overhead_bytes = chunks.iter().map(|c| c.s.legacy_base64_overhead_bytes).sum();
        s.legacy_json_framing_bytes = chunks.iter().map(|c| c.s.legacy_json_framing_bytes).sum();
        // Entry counts came from legacy; reset before tiles add theirs.
        let (n_legacy, x_legacy, p_legacy) = (s.entries, s.x509, s.precert);

        // Tiles.
        let tiles: Vec<(Vec<u8>, crate::http::Event)> = stream::iter((first / TILE_WIDTH)..(end / TILE_WIDTH))
            .map(|t| {
                let http = http.clone();
                let url = join(log, &data_tile_path(t, None));
                async move { get_measured(&http, &url, Some("gzip"), Instant::now(), "tile", Some(t * TILE_WIDTH)).await }
            })
            .buffered(concurrency)
            .try_collect()
            .await?;
        let mut ft = Frontier::new();
        let mut fps = BTreeSet::new();
        let (mut nt, mut xt, mut pt) = (0u64, 0u64, 0u64);
        for (body, ev) in tiles {
            s.tile_requests += 1;
            s.tile_served_bytes += body.len() as u64;
            let k = if ev.content_encoding.is_empty() { "none".into() } else { ev.content_encoding.clone() };
            *s.tile_content_encodings.entry(k).or_default() += 1;
            let (raw, layers) = strip_gzip(body)?;
            *s.tile_gzip_layers.entry(layers).or_default() += 1;
            s.tile_decoded_bytes += raw.len() as u64;
            for e in parse_data_tile(&raw)? {
                nt += 1;
                s.tile_timestamped_entry_bytes += e.merkle_leaf.len() as u64 - 2;
                match e.kind {
                    EntryKind::X509 => xt += 1,
                    EntryKind::Precert => {
                        pt += 1;
                        s.tile_precert_bytes += e.submitted_cert.len() as u64 + 3;
                    }
                }
                s.tile_fingerprint_bytes += 2 + 32 * e.chain_fingerprints.len() as u64;
                ft.push(e.leaf_hash());
                fps.extend(e.chain_fingerprints);
            }
        }
        let issuers: Vec<(Vec<u8>, [u8; 32])> = stream::iter(fps.iter().copied())
            .map(|fp| {
                let http = http.clone();
                let url = join(log, &format!("issuer/{}", hex::encode(fp)));
                async move {
                    let (b, _) = get_measured(&http, &url, Some("gzip"), Instant::now(), "issuer", None).await?;
                    Ok::<_, anyhow::Error>((b, fp))
                }
            })
            .buffer_unordered(concurrency)
            .try_collect()
            .await?;
        for (body, fp) in issuers {
            s.issuers += 1;
            s.issuer_served_bytes += body.len() as u64;
            let (der, _) = strip_gzip(body)?;
            if sha256(&der) != fp {
                bail!("issuer does not match fingerprint");
            }
            s.issuer_der_bytes += der.len() as u64;
        }

        // Both representations must be the same block of the pinned tree.
        let want = &b.root_b64;
        if B64.encode(fl.root()) != *want || B64.encode(ft.root()) != *want {
            bail!("block {} does not match the reference root", b.block);
        }
        if (n_legacy, x_legacy, p_legacy) != (nt, xt, pt) {
            bail!("block {}: legacy and tiles disagree on entry counts", b.block);
        }
        total.add(&s);
        per_block.push(serde_json::json!({ "block": b.block, "sizes": s }));
    }
    let doc = serde_json::json!({
        "experiment": "E0 offline encoding analysis (byte counts only)",
        "tree_size": refs.tree_size,
        "root_hash_b64": refs.root_hash_b64,
        "computed_at": chrono::Utc::now().to_rfc3339(),
        "harness_version": env!("CARGO_PKG_VERSION"),
        "total": total,
        "blocks": per_block,
    });
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(out, serde_json::to_vec_pretty(&doc)?)?;
    let t = &total;
    let mb = |x: u64| x as f64 / 1e6;
    eprintln!("\nE0 over {} entries:", t.entries);
    eprintln!("  legacy JSON identity   {:>10.1} MB", mb(t.legacy_json_bytes));
    eprintln!("  legacy JSON gzip       {:>10.1} MB (served)", mb(t.legacy_gzip_served_bytes));
    eprintln!("  legacy binary          {:>10.1} MB", mb(t.legacy_raw_bytes));
    eprintln!("  tiles decoded+issuers  {:>10.1} MB", mb(t.tile_decoded_bytes + t.issuer_der_bytes));
    eprintln!("  tiles served+issuers   {:>10.1} MB", mb(t.tile_served_bytes + t.issuer_served_bytes));
    eprintln!("report: {}", out.display());
    Ok(())
}
