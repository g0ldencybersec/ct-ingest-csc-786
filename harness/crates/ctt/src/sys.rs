//! Client-side resource accounting for a trial: CPU time, peak memory, and
//! (on Linux VMs) CPU steal from /proc/stat.

use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Rusage {
    pub user_secs: f64,
    pub sys_secs: f64,
    /// Peak resident set size in KiB.
    pub max_rss_kib: u64,
}

#[allow(clippy::unnecessary_cast)]
pub fn rusage() -> Rusage {
    #[cfg(unix)]
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut ru) != 0 {
            return Rusage::default();
        }
        let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        // Linux reports KiB; macOS reports bytes.
        #[cfg(target_os = "macos")]
        let rss = ru.ru_maxrss as u64 / 1024;
        #[cfg(not(target_os = "macos"))]
        let rss = ru.ru_maxrss as u64;
        Rusage {
            user_secs: tv(ru.ru_utime),
            sys_secs: tv(ru.ru_stime),
            max_rss_kib: rss,
        }
    }
    #[cfg(not(unix))]
    Rusage::default()
}

/// Aggregate CPU tick counters from the first line of /proc/stat.
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuTicks {
    pub total: u64,
    pub idle: u64,
    pub steal: u64,
}

pub fn cpu_ticks() -> Option<CpuTicks> {
    let s = std::fs::read_to_string("/proc/stat").ok()?;
    let line = s.lines().next()?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|x| x.parse().ok())
        .collect();
    if v.len() < 8 {
        return None;
    }
    // user nice system idle iowait irq softirq steal [guest guest_nice]
    // guest time is already included in user/nice, so sum the first 8.
    Some(CpuTicks {
        total: v[..8].iter().sum(),
        idle: v[3] + v[4],
        steal: v[7],
    })
}

/// Steal and busy fractions of all CPUs between two samples.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CpuShare {
    pub steal_frac: f64,
    pub busy_frac: f64,
}

pub fn cpu_share(a: CpuTicks, b: CpuTicks) -> Option<CpuShare> {
    let dt = b.total.checked_sub(a.total)?;
    if dt == 0 {
        return None;
    }
    let steal = b.steal.saturating_sub(a.steal) as f64 / dt as f64;
    let idle = b.idle.saturating_sub(a.idle) as f64 / dt as f64;
    Some(CpuShare {
        steal_frac: steal,
        busy_frac: 1.0 - idle,
    })
}
