//! `ctt fetch`: download a bounded slice of a public Static CT log, once.
//!
//! Layout written to `--out`:
//!   checkpoint.txt         source checkpoint at fetch time (verbatim)
//!   tiles/<index>.bin      raw data tile bytes as received (usually gzip)
//!   issuers/<hex>.der      issuer certificates, verified against fingerprint
//!   manifest.json          provenance: source, sizes, per-file SHA-256
//!
//! Two sampling modes:
//!   - prefix (default): the first N entries of the log.
//!   - seeded (`--sample-seed S`): N/256 whole tiles chosen uniformly at
//!     random across the log with a recorded seed, so the slice is not
//!     clustered in time and anyone can reselect the same tiles.
//!
//! Re-running is safe: files already on disk are kept, not re-downloaded.
//! Request rate is kept low on purpose (shared public infrastructure).

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use futures::{stream, StreamExt, TryStreamExt};
use serde::Serialize;

use ctcore::checkpoint::parse_checkpoint;
use ctcore::tile::{data_tile_path, tiles_for, TILE_WIDTH};
use ctcore::{entry::parse_data_tile, maybe_gunzip, sha256};

use crate::http::{client, get_bytes, join};

#[derive(Serialize)]
struct TileRecord {
    index: u64,
    width: u64,
    bytes: usize,
    sha256: String,
}

#[derive(Serialize)]
struct Sampling {
    mode: &'static str,
    seed: Option<u64>,
    /// Full tiles in the source log that were eligible for selection.
    population_tiles: u64,
    algorithm: &'static str,
}

#[derive(Serialize)]
struct Manifest {
    source_log_url: String,
    source_origin: String,
    source_tree_size: u64,
    source_root_hash_b64: String,
    fetched_at: String,
    entries: u64,
    sampling: Sampling,
    x509_entries: u64,
    precert_entries: u64,
    tiles: Vec<TileRecord>,
    issuers: Vec<String>,
    note: &'static str,
}

pub async fn run(
    log_url: &str,
    entries: u64,
    out: &Path,
    concurrency: usize,
    contact: Option<&str>,
    sample_seed: Option<u64>,
) -> Result<()> {
    if contact.is_none() {
        eprintln!(
            "warning: no --contact given; set one (or CTT_CONTACT) so the log operator can reach you"
        );
    }
    let http = client(contact)?;
    std::fs::create_dir_all(out.join("tiles"))?;
    std::fs::create_dir_all(out.join("issuers"))?;

    // 1. Source checkpoint.
    let cp_text = String::from_utf8(get_bytes(&http, &join(log_url, "checkpoint")).await?)?;
    let cp = parse_checkpoint(&cp_text)?;
    std::fs::write(out.join("checkpoint.txt"), &cp_text)?;
    eprintln!(
        "source {} has {} entries",
        cp.origin, cp.head.tree_size
    );

    // 2. Choose the data tiles (whole tiles only).
    let full_tiles = cp.head.tree_size / TILE_WIDTH;
    let want_tiles = entries.div_ceil(TILE_WIDTH);
    let (plan, sampling): (Vec<(u64, Option<u64>)>, Sampling) = match sample_seed {
        None => {
            let size = (want_tiles * TILE_WIDTH).min(cp.head.tree_size);
            (
                tiles_for(size),
                Sampling {
                    mode: "prefix",
                    seed: None,
                    population_tiles: full_tiles,
                    algorithm: "first N entries",
                },
            )
        }
        Some(seed) => {
            if want_tiles > full_tiles {
                bail!("asked for {want_tiles} tiles but the log has only {full_tiles} full tiles");
            }
            let chosen = ctcore::sample::sample_indices(full_tiles, want_tiles, seed);
            (
                chosen.into_iter().map(|i| (i, None)).collect(),
                Sampling {
                    mode: "seeded-uniform-tiles",
                    seed: Some(seed),
                    population_tiles: full_tiles,
                    algorithm: "SplitMix64 + Floyd sampling over full tile indices (ctcore::sample)",
                },
            )
        }
    };
    if plan.is_empty() {
        bail!("source log is empty");
    }
    let size: u64 = plan.iter().map(|(_, w)| w.unwrap_or(TILE_WIDTH)).sum();
    let total_tiles = plan.len();
    eprintln!(
        "fetching {total_tiles} data tiles ({size} entries, {} mode)",
        sampling.mode
    );
    let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let records: Vec<TileRecord> = stream::iter(plan)
        .map(|(index, width)| {
            let http = http.clone();
            let url = join(log_url, &data_tile_path(index, width));
            let path = out.join("tiles").join(format!("{index:09}.bin"));
            let progress = progress.clone();
            async move {
                let raw = if path.exists() {
                    std::fs::read(&path)?
                } else {
                    let raw = get_bytes(&http, &url).await?;
                    std::fs::write(&path, &raw)?;
                    raw
                };
                let n = parse_data_tile(&maybe_gunzip(&raw)?)
                    .with_context(|| format!("parsing tile {index}"))?
                    .len() as u64;
                let expect = width.unwrap_or(TILE_WIDTH);
                if n != expect {
                    bail!("tile {index}: expected {expect} entries, found {n}");
                }
                let k = progress.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                #[allow(clippy::manual_is_multiple_of)]
                if k % 200 == 0 || k == total_tiles {
                    eprintln!("  {k}/{total_tiles} tiles");
                }
                Ok::<_, anyhow::Error>(TileRecord {
                    index,
                    width: n,
                    bytes: raw.len(),
                    sha256: hex::encode(sha256(&raw)),
                })
            }
        })
        .buffered(concurrency)
        .try_collect()
        .await?;

    // 3. Collect issuer fingerprints and entry counts.
    let mut issuers = BTreeSet::new();
    let (mut x509, mut precert) = (0u64, 0u64);
    for r in &records {
        let raw = std::fs::read(out.join("tiles").join(format!("{:09}.bin", r.index)))?;
        for e in parse_data_tile(&maybe_gunzip(&raw)?)? {
            match e.kind {
                ctcore::EntryKind::X509 => x509 += 1,
                ctcore::EntryKind::Precert => precert += 1,
            }
            issuers.extend(e.chain_fingerprints.iter().map(hex::encode));
        }
    }
    eprintln!("fetching {} distinct issuer certificates", issuers.len());

    // 4. Issuers, verified against their fingerprints.
    stream::iter(issuers.iter().cloned())
        .map(|fp| {
            let http = http.clone();
            let url = join(log_url, &format!("issuer/{fp}"));
            let path = out.join("issuers").join(format!("{fp}.der"));
            async move {
                if !path.exists() {
                    let der = get_bytes(&http, &url).await?;
                    if hex::encode(sha256(&der)) != fp {
                        bail!("issuer {fp}: content does not match fingerprint");
                    }
                    std::fs::write(&path, &der)?;
                }
                Ok::<_, anyhow::Error>(())
            }
        })
        .buffer_unordered(concurrency)
        .try_collect::<Vec<()>>()
        .await?;

    let manifest = Manifest {
        source_log_url: log_url.to_string(),
        source_origin: cp.origin.clone(),
        source_tree_size: cp.head.tree_size,
        source_root_hash_b64: cp_text.lines().nth(2).unwrap_or_default().to_string(),
        fetched_at: chrono::Utc::now().to_rfc3339(),
        entries: size,
        sampling,
        x509_entries: x509,
        precert_entries: precert,
        tiles: records,
        issuers: issuers.into_iter().collect(),
        note: "Tiles are stored exactly as received, named by their index in the source log. \
               The slice is not proven against the source root (only the testbed tree is verified).",
    };
    std::fs::write(
        out.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    eprintln!(
        "done: {} entries ({} x509, {} precert), {} issuers -> {}",
        manifest.entries,
        x509,
        precert,
        manifest.issuers.len(),
        out.display()
    );
    Ok(())
}
