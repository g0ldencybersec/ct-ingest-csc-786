//! `ctt`: testbed tools (milestone M1).
//!
//! - `fetch`:  download a bounded slice of a public Static CT log, once.
//! - `replay`: submit that slice into the private CompactLog testbed.
//! - `gonogo`: read the testbed through BOTH APIs and verify they serve
//!   identical data that recomputes to the published root.

mod fetch;
mod gonogo;
mod http;
mod replay;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ctt", version, about = "CT ingestion testbed tools (CSC 786)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Download the first N entries (plus issuers) of a public Static CT log.
    Fetch {
        /// Monitoring prefix, e.g. https://mon.sycamore.ct.letsencrypt.org/2026h1/
        #[arg(long)]
        log_url: String,
        /// Number of entries to fetch (rounded up to whole tiles).
        #[arg(long, default_value_t = 100_000)]
        entries: u64,
        /// Output directory for the slice.
        #[arg(long)]
        out: PathBuf,
        /// Requests in flight against the public log. Keep this low.
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
        /// Contact string sent in the User-Agent so the operator can reach you.
        #[arg(long, env = "CTT_CONTACT")]
        contact: Option<String>,
        /// Sample whole tiles uniformly across the log with this seed,
        /// instead of taking the first N entries. Recorded in the manifest.
        #[arg(long)]
        sample_seed: Option<u64>,
    },
    /// Replay a fetched slice into the testbed log via add-chain / add-pre-chain.
    Replay {
        #[arg(long)]
        slice: PathBuf,
        /// Testbed base URL.
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        log: String,
        /// Submissions in flight. CompactLog batches writes about every 500 ms,
        /// so high concurrency is needed for reasonable load times.
        #[arg(long, default_value_t = 256)]
        concurrency: usize,
        /// Only replay the first N entries of the slice.
        #[arg(long)]
        limit: Option<u64>,
    },
    /// M1 go/no-go: identical leaves, chains, and root through both APIs.
    Gonogo {
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        log: String,
        /// Where to write the JSON report.
        #[arg(long, default_value = "../runs/gonogo.json")]
        out: PathBuf,
        #[arg(long, default_value_t = 8)]
        concurrency: usize,
    },
}

/// Raise the soft open-file limit as far as allowed and return it.
///
/// macOS defaults to 256 open files per process, which concurrent HTTP
/// submissions exhaust quickly. Computed once at startup.
#[allow(clippy::unnecessary_cast)] // rlim_t differs across platforms
fn raise_fd_limit() -> u64 {
    #[cfg(unix)]
    unsafe {
        let mut rl = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) != 0 {
            return 256;
        }
        #[allow(unused_mut)]
        let mut target = rl.rlim_max;
        // macOS rejects values above OPEN_MAX (10240) even if the hard limit
        // reports "unlimited".
        #[cfg(target_os = "macos")]
        {
            target = target.min(10240);
        }
        if target > rl.rlim_cur {
            let new = libc::rlimit {
                rlim_cur: target,
                rlim_max: rl.rlim_max,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &new) == 0 {
                return target as u64;
            }
        }
        rl.rlim_cur as u64
    }
    #[cfg(not(unix))]
    {
        512
    }
}

static FD_LIMIT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// The process open-file limit after [`raise_fd_limit`].
pub fn fd_limit() -> u64 {
    *FD_LIMIT.get_or_init(raise_fd_limit)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    fd_limit();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Fetch {
            log_url,
            entries,
            out,
            concurrency,
            contact,
            sample_seed,
        } => {
            fetch::run(
                &log_url,
                entries,
                &out,
                concurrency,
                contact.as_deref(),
                sample_seed,
            )
            .await
        }
        Cmd::Replay {
            slice,
            log,
            concurrency,
            limit,
        } => replay::run(&slice, &log, concurrency, limit).await,
        Cmd::Gonogo {
            log,
            out,
            concurrency,
        } => {
            let go = gonogo::run(&log, &out, concurrency).await?;
            if !go {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}
