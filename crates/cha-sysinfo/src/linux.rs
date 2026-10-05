//! The host's CPU and RAM, from /proc.

use std::time::Instant;

use crate::HostSample;

/// A sampler: it keeps the last counters to take deltas of.
pub struct Sampler {
    last: Option<Counters>,
}

struct Counters {
    at: Instant,
    cpu: CpuTimes,
    /// This process's user + system time, in clock ticks.
    own_ticks: u64,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// The reading now (the CPU figures are over the time since the last
    /// one, and 0 on the first), or None if /proc can't be read.
    pub fn sample(&mut self) -> Option<HostSample> {
        let at = Instant::now();
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let cpu = parse_cpu(&stat)?;
        let cores = count_cores(&stat);
        let mem = parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)?;
        let own_ticks = std::fs::read_to_string("/proc/self/stat")
            .ok()
            .and_then(|s| parse_self_ticks(&s))
            .unwrap_or(0);
        let load = std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| parse_load(&s))
            .unwrap_or_default();
        let now = Counters { at, cpu, own_ticks };
        let (cpu_pct, process_cpu) = match &self.last {
            Some(prev) => {
                let secs = at.duration_since(prev.at).as_secs_f64();
                let own = now.own_ticks.saturating_sub(prev.own_ticks) as f64
                    / clock_ticks_per_second()
                    / secs.max(1e-3)
                    * 100.0;
                (now.cpu.utilisation_since(&prev.cpu), own)
            }
            None => (0.0, 0.0),
        };
        self.last = Some(now);
        Some(HostSample {
            cpu: round1(cpu_pct),
            cores,
            load,
            mem_used: mem.total.saturating_sub(mem.available),
            mem_total: mem.total,
            process_cpu: round1(process_cpu),
        })
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn clock_ticks_per_second() -> f64 {
    // SAFETY: sysconf only reads a constant.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz > 0 { hz as f64 } else { 100.0 }
}

/// The aggregate `cpu` line of `/proc/stat`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CpuTimes {
    busy: u64,
    total: u64,
}

impl CpuTimes {
    /// The share of the time since `prev` spent not idle, in percent.
    fn utilisation_since(&self, prev: &CpuTimes) -> f64 {
        let total = self.total.saturating_sub(prev.total);
        if total == 0 {
            return 0.0;
        }
        let busy = self.busy.saturating_sub(prev.busy);
        (busy as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
    }
}

/// `cpu  user nice system idle iowait irq softirq steal guest guest_nice`:
/// guest time is already in user's, so only the first eight count; idle is
/// idle and iowait.
fn parse_cpu(stat: &str) -> Option<CpuTimes> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(|f| f.parse().ok())
        .collect::<Option<_>>()?;
    if fields.len() < 4 {
        return None;
    }
    let total: u64 = fields.iter().sum();
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    Some(CpuTimes {
        busy: total - idle,
        total,
    })
}

fn count_cores(stat: &str) -> usize {
    stat.lines()
        .filter(|l| {
            l.strip_prefix("cpu")
                .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
        })
        .count()
}

struct Memory {
    total: u64,
    available: u64,
}

/// `MemTotal` and `MemAvailable`, in bytes (the file says kB).
fn parse_meminfo(text: &str) -> Option<Memory> {
    let kb = |key: &str| -> Option<u64> {
        let rest = text.lines().find_map(|l| l.strip_prefix(key))?;
        rest.trim_start_matches(':')
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
            .map(|v| v * 1024)
    };
    let total = kb("MemTotal")?;
    // Kernels before 3.14 have no MemAvailable; free + cached is the nearest.
    let available =
        kb("MemAvailable").or_else(|| Some(kb("MemFree")? + kb("Cached").unwrap_or(0)))?;
    Some(Memory { total, available })
}

/// The first three fields of `/proc/loadavg`.
fn parse_load(text: &str) -> Option<[f64; 3]> {
    let mut fields = text.split_whitespace();
    let mut next = || fields.next()?.parse().ok();
    Some([next()?, next()?, next()?])
}

/// utime + stime of `/proc/self/stat`. The command name in field 2 may hold
/// spaces and parentheses, so the fields count from the last `)`.
fn parse_self_ticks(stat: &str) -> Option<u64> {
    let after = &stat[stat.rfind(')')? + 1..];
    // After the name: state is field 3, so utime and stime (14, 15) are the
    // 12th and 13th here.
    let mut fields = after.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime + stime)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "cpu  4705 150 1120 16250 520 0 30 0 0 0\n\
        cpu0 1100 40 300 4000 100 0 10 0 0 0\n\
        cpu1 1200 30 250 4100 140 0 5 0 0 0\n\
        intr 12345 0 0\nctxt 999\n";

    #[test]
    fn cpu_line_gives_busy_and_total() {
        let t = parse_cpu(STAT).unwrap();
        // total = 4705+150+1120+16250+520+0+30+0; idle = 16250+520.
        assert_eq!(t.total, 22775);
        assert_eq!(t.busy, 22775 - 16770);
        assert_eq!(count_cores(STAT), 2);
    }

    #[test]
    fn utilisation_is_the_busy_share_of_the_delta() {
        let a = CpuTimes {
            busy: 100,
            total: 1000,
        };
        let b = CpuTimes {
            busy: 175,
            total: 1100,
        };
        assert_eq!(b.utilisation_since(&a), 75.0);
        assert_eq!(a.utilisation_since(&a), 0.0);
    }

    #[test]
    fn meminfo_is_in_bytes_and_prefers_available() {
        let text = "MemTotal:       16384000 kB\nMemFree:         1000000 kB\n\
            MemAvailable:    8192000 kB\nCached:          2000000 kB\n";
        let m = parse_meminfo(text).unwrap();
        assert_eq!(m.total, 16_384_000 * 1024);
        assert_eq!(m.available, 8_192_000 * 1024);
        let old = "MemTotal: 1000 kB\nMemFree: 100 kB\nCached: 50 kB\n";
        assert_eq!(parse_meminfo(old).unwrap().available, 150 * 1024);
        assert!(parse_meminfo("nothing").is_none());
    }

    #[test]
    fn load_and_own_ticks() {
        assert_eq!(
            parse_load("2.10 1.50 1.00 3/500 12345\n"),
            Some([2.1, 1.5, 1.0])
        );
        assert_eq!(parse_load("2.10"), None);
        // A name with spaces and parentheses; utime 70, stime 30.
        let stat =
            "42 (cha (str) eamer) S 1 42 42 0 -1 4194560 100 0 0 0 70 30 0 0 20 0 8 0 100 1000 50";
        assert_eq!(parse_self_ticks(stat), Some(100));
    }

    /// Prints a real sample, to compare with `top` and `free` by hand.
    #[test]
    #[ignore = "needs /proc"]
    fn prints_a_sample() {
        let mut sampler = Sampler::new();
        sampler.sample().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        println!("{:#?}", sampler.sample().unwrap());
    }
}
