//! `ctt replay`: submit a fetched slice into the private testbed log.
//!
//! The testbed assigns its own timestamps and order, so its tree is NOT the
//! source tree. That is expected: ground truth for every experiment is the
//! testbed's own signed tree head, pinned after loading.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use futures::{stream, StreamExt, TryStreamExt};
use serde::Serialize;

use ctcore::checkpoint::SthJson;
use ctcore::{entry::parse_data_tile, maybe_gunzip, EntryKind};

use crate::http::{client, join};

#[derive(Default, Serialize)]
struct Tally {
    submitted: u64,
    accepted: u64,
    rejected: u64,
    failed: u64,
    missing_issuer: u64,
    sample_errors: Vec<String>,
}

pub async fn run(slice: &Path, log: &str, concurrency: usize, limit: Option<u64>) -> Result<()> {
    let http = client(None)?;

    let before: SthJson = serde_json::from_slice(
        &crate::http::get_bytes(&http, &join(log, "ct/v1/get-sth")).await?,
    )?;
    if before.tree_size > 0 {
        eprintln!(
            "warning: testbed already holds {} entries. Re-submitted certificates are \
             de-duplicated, so resuming an interrupted replay is fine, but entries from a \
             DIFFERENT slice would be mixed in. To start clean: \
             (cd testbed && docker compose down -v && docker compose up -d)",
            before.tree_size
        );
    }

    let mut issuers: HashMap<[u8; 32], Vec<u8>> = HashMap::new();
    for f in std::fs::read_dir(slice.join("issuers")).context("reading issuers/")? {
        let p = f?.path();
        let der = std::fs::read(&p)?;
        issuers.insert(ctcore::sha256(&der), der);
    }
    let issuers = Arc::new(issuers);

    let mut tiles: Vec<PathBuf> = std::fs::read_dir(slice.join("tiles"))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    tiles.sort();

    let limit = limit.unwrap_or(u64::MAX);
    let tally = Arc::new(Mutex::new(Tally::default()));
    let done = Arc::new(AtomicU64::new(0));
    let start = Instant::now();

    // Each in-flight submission holds a socket. Keep headroom below the
    // process file-descriptor limit for tile reads and everything else.
    let fd_limit = crate::fd_limit();
    let max_conc = (fd_limit.saturating_sub(64) as usize).max(1);
    let concurrency = if concurrency > max_conc {
        eprintln!(
            "note: lowering concurrency {concurrency} -> {max_conc} (open-file limit is {fd_limit})"
        );
        max_conc
    } else {
        concurrency
    };

    // A tile that cannot be read is a hard error: silently skipping it would
    // leave the testbed with an incomplete, unrepresentative slice.
    let entries = stream::iter(tiles)
        .map(|p| -> Result<Vec<ctcore::Entry>> {
            let raw = std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?;
            parse_data_tile(&maybe_gunzip(&raw)?)
                .with_context(|| format!("parsing {}", p.display()))
        })
        .flat_map(|r| match r {
            Ok(v) => stream::iter(v.into_iter().map(Ok)).boxed(),
            Err(e) => stream::once(async move { Err(e) }).boxed(),
        })
        .take(limit as usize);

    entries
        .map(|item: Result<ctcore::Entry>| {
            let http = http.clone();
            let issuers = issuers.clone();
            let tally = tally.clone();
            let done = done.clone();
            let log = log.to_string();
            async move {
                let e = item?;
                let mut chain = vec![B64.encode(&e.submitted_cert)];
                for fp in &e.chain_fingerprints {
                    match issuers.get(fp) {
                        Some(der) => chain.push(B64.encode(der)),
                        None => {
                            let mut t = tally.lock().unwrap();
                            t.missing_issuer += 1;
                            return Ok(());
                        }
                    }
                }
                let endpoint = match e.kind {
                    EntryKind::X509 => "ct/v1/add-chain",
                    EntryKind::Precert => "ct/v1/add-pre-chain",
                };
                let body = serde_json::json!({ "chain": chain });
                let outcome = submit(&http, &join(&log, endpoint), &body).await;
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                #[allow(clippy::manual_is_multiple_of)] // keep compatible with older toolchains
                if n % 5000 == 0 {
                    let rate = n as f64 / start.elapsed().as_secs_f64();
                    eprintln!("  {n} submitted ({rate:.0}/s)");
                }
                let mut t = tally.lock().unwrap();
                t.submitted += 1;
                match outcome {
                    Outcome::Accepted => t.accepted += 1,
                    Outcome::Rejected(msg) => {
                        t.rejected += 1;
                        if t.sample_errors.len() < 20 {
                            t.sample_errors.push(msg);
                        }
                    }
                    Outcome::Failed(msg) => {
                        t.failed += 1;
                        if t.sample_errors.len() < 20 {
                            t.sample_errors.push(msg);
                        }
                    }
                }
                Ok::<(), anyhow::Error>(())
            }
        })
        .buffer_unordered(concurrency)
        .try_collect::<Vec<()>>()
        .await?;

    // Let the final batch publish, then record the resulting tree head.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let sth: SthJson = serde_json::from_slice(
        &crate::http::get_bytes(&http, &join(log, "ct/v1/get-sth")).await?,
    )?;

    let t = tally.lock().unwrap();
    let report = serde_json::json!({
        "slice": slice.display().to_string(),
        "log": log,
        "elapsed_secs": start.elapsed().as_secs_f64(),
        "tally": &*t,
        "testbed_tree_size": sth.tree_size,
        "testbed_root_hash_b64": sth.sha256_root_hash,
        "finished_at": chrono::Utc::now().to_rfc3339(),
    });
    std::fs::write(
        slice.join("replay-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    eprintln!(
        "replay done in {:.0}s: {} submitted, {} accepted, {} rejected, {} failed, {} missing issuer",
        start.elapsed().as_secs_f64(),
        t.submitted,
        t.accepted,
        t.rejected,
        t.failed,
        t.missing_issuer
    );
    eprintln!("testbed tree size is now {}", sth.tree_size);
    if t.rejected > 0 || t.failed > 0 {
        eprintln!("see replay-report.json for sample errors");
    }
    Ok(())
}

enum Outcome {
    Accepted,
    Rejected(String),
    Failed(String),
}

async fn submit(http: &reqwest::Client, url: &str, body: &serde_json::Value) -> Outcome {
    let mut delay = Duration::from_millis(250);
    for attempt in 1..=6 {
        match http.post(url).json(body).send().await {
            Ok(r) if r.status().is_success() => return Outcome::Accepted,
            Ok(r) if r.status().is_client_error() && r.status().as_u16() != 429 => {
                let s = r.status();
                let msg = r.text().await.unwrap_or_default();
                return Outcome::Rejected(format!("{s}: {}", msg.trim()));
            }
            Ok(r) if attempt == 6 => return Outcome::Failed(format!("{}", r.status())),
            Err(e) if attempt == 6 => return Outcome::Failed(e.to_string()),
            _ => {}
        }
        tokio::time::sleep(delay).await;
        delay *= 2;
    }
    unreachable!()
}
