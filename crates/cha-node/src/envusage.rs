//! What each running environment uses of the node: CPU and RAM from the
//! engine's container stats (the app's and the streamer's together), and GPU
//! memory from NVML's process list, matched to containers by host pid through
//! `/proc/<pid>/cgroup`. That match needs the agent to share the host's pid
//! namespace (`pid: host`); without it NVML lists no processes and the VRAM
//! figure is left out.
//!
//! The work runs in its own task ([`Watcher`]) every few seconds, so a slow
//! engine never holds up the agent's heartbeats; the usage report just takes
//! the latest result. CPU is a rate, so a [`Memory`] keeps each container's
//! last reading; the first reading of a container has none and counts as 0.
//! The figures go to the portal in [`cha_wire::NodeUsage::by_environment`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cha_wire::EnvironmentUsage;
use futures_util::future::join_all;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::docker::{ContainerSummary, Docker};

/// How long one round of readings may take before it is dropped.
const ROUND_TIMEOUT: Duration = Duration::from_secs(5);

/// Keeps reading every environment's use and holds the latest; stops when
/// dropped (the connection to the portal ended).
pub struct Watcher {
    latest: Arc<Mutex<Vec<EnvironmentUsage>>>,
    task: JoinHandle<()>,
}

impl Watcher {
    pub fn start(docker: Docker, every: Duration) -> Self {
        let latest = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&latest);
        let task = tokio::spawn(async move {
            let mut memory = Memory::default();
            let mut ticks = tokio::time::interval(every);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticks.tick().await;
                let round = tokio::time::timeout(ROUND_TIMEOUT, collect(&docker, &mut memory));
                // A round that ran out of time keeps the figures from the last.
                if let Ok(usage) = round.await {
                    *shared.lock().expect("usage lock") = usage;
                }
            }
        });
        Self { latest, task }
    }

    /// What the last round read.
    pub fn latest(&self) -> Vec<EnvironmentUsage> {
        self.latest.lock().expect("usage lock").clone()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The label that names a container's environment (`environments.rs`).
const LABEL_ENV: &str = "sh.cha.env";

/// A container's cumulative CPU and the machine's, at its last reading.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Last {
    container_ns: f64,
    system_ns: f64,
}

/// What the last reading of each container said.
#[derive(Debug, Default)]
pub struct Memory(HashMap<String, Last>);

/// One container's reading, as far as the stats say.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Reading {
    container_ns: f64,
    system_ns: f64,
    mem: u64,
}

fn reading(stats: &Value) -> Option<Reading> {
    let cpu = &stats["cpu_stats"];
    let mem = &stats["memory_stats"];
    let usage = mem["usage"].as_u64()?;
    // What `docker stats` shows: usage without the reclaimable page cache
    // (`inactive_file` on cgroup v2, `total_inactive_file` on v1, else `cache`).
    let cache = ["inactive_file", "total_inactive_file", "cache"]
        .iter()
        .find_map(|k| mem["stats"][k].as_u64())
        .unwrap_or(0);
    Some(Reading {
        container_ns: cpu["cpu_usage"]["total_usage"].as_f64()?,
        system_ns: cpu["system_cpu_usage"].as_f64().unwrap_or(0.0),
        mem: usage.saturating_sub(cache),
    })
}

/// The share of the whole machine's CPU between two readings, in percent.
fn cpu_percent(before: Option<Last>, now: Reading) -> f64 {
    let Some(before) = before else { return 0.0 };
    let container = now.container_ns - before.container_ns;
    let system = now.system_ns - before.system_ns;
    if container <= 0.0 || system <= 0.0 {
        return 0.0;
    }
    (container / system * 100.0).min(100.0)
}

/// The container a `/proc/<pid>/cgroup` file puts a process in: the 64-digit id
/// in a `docker-<id>.scope` (systemd) or `/docker/<id>` (cgroupfs) path.
fn container_of(cgroup: &str) -> Option<String> {
    cgroup
        .lines()
        .flat_map(|line| line.rsplit(':').next().unwrap_or("").split('/'))
        .map(|part| {
            let part = part.strip_prefix("docker-").unwrap_or(part);
            part.strip_suffix(".scope").unwrap_or(part)
        })
        .find(|part| part.len() == 64 && part.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_string)
}

/// GPU memory per container id, from NVML's process list; `None` when NVML
/// lists no processes at all (no NVIDIA GPU, or the agent can't see the host's
/// pids), in which case no container's figure can be trusted. Reads files and
/// asks the driver, so it belongs on a blocking thread.
fn vram_by_container() -> Option<HashMap<String, u64>> {
    let processes = cha_sysinfo::gpu_processes();
    if processes.is_empty() {
        return None;
    }
    let mut by_container: HashMap<String, u64> = HashMap::new();
    for p in processes {
        let Ok(cgroup) = std::fs::read_to_string(format!("/proc/{}/cgroup", p.pid)) else {
            continue; // gone since NVML listed it, or not ours to see
        };
        if let Some(container) = container_of(&cgroup) {
            *by_container.entry(container).or_insert(0) += p.vram;
        }
    }
    Some(by_container)
}

/// Every running environment's use now. A container the engine won't read is
/// left out of its environment's sum; an environment with none is left out.
pub async fn collect(docker: &Docker, memory: &mut Memory) -> Vec<EnvironmentUsage> {
    let Ok(containers) = docker.list(LABEL_ENV).await else {
        return Vec::new();
    };
    let running: Vec<&ContainerSummary> = containers
        .iter()
        .filter(|c| c.state == "running" && c.labels.contains_key(LABEL_ENV))
        .collect();
    if running.is_empty() {
        memory.0.clear();
        return Vec::new();
    }
    // The driver and /proc, off the async threads, while the engine answers.
    let vram = tokio::task::spawn_blocking(vram_by_container);
    let stats = join_all(running.iter().map(|c| docker.stats(&c.id))).await;
    let vram = vram.await.ok().flatten();

    let mut by_env: HashMap<String, EnvironmentUsage> = HashMap::new();
    let mut seen: HashMap<String, Last> = HashMap::new();
    for (c, stats) in running.into_iter().zip(stats) {
        let Some(now) = stats.ok().as_ref().and_then(reading) else {
            continue;
        };
        let before = memory.0.get(&c.id).copied();
        seen.insert(
            c.id.clone(),
            Last {
                container_ns: now.container_ns,
                system_ns: now.system_ns,
            },
        );
        let env = &c.labels[LABEL_ENV];
        let entry = by_env
            .entry(env.clone())
            .or_insert_with(|| EnvironmentUsage {
                id: env.clone(),
                ..EnvironmentUsage::default()
            });
        entry.cpu += cpu_percent(before, now);
        entry.mem += now.mem;
        if let Some(vram) = &vram {
            *entry.vram.get_or_insert(0) += vram.get(&c.id).copied().unwrap_or(0);
        }
    }
    // Containers that are gone take their last reading with them.
    memory.0 = seen;
    let mut out: Vec<_> = by_env.into_values().collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn memory_leaves_out_the_reclaimable_cache() {
        let v2 = json!({
            "cpu_stats": { "cpu_usage": { "total_usage": 10 }, "system_cpu_usage": 100 },
            "memory_stats": { "usage": 1000, "stats": { "inactive_file": 300 } },
        });
        assert_eq!(reading(&v2).unwrap().mem, 700);
        let v1 = json!({
            "cpu_stats": { "cpu_usage": { "total_usage": 10 } },
            "memory_stats": { "usage": 1000, "stats": { "total_inactive_file": 100, "cache": 900 } },
        });
        let r = reading(&v1).unwrap();
        assert_eq!((r.mem, r.system_ns), (900, 0.0));
        assert!(
            reading(&json!({})).is_none(),
            "a stopped container has no figures"
        );
    }

    #[test]
    fn cpu_is_the_share_of_the_machine_between_two_readings() {
        let last = Last {
            container_ns: 1_000.0,
            system_ns: 10_000.0,
        };
        let now = Reading {
            container_ns: 1_500.0,
            system_ns: 12_000.0,
            mem: 0,
        };
        assert_eq!(cpu_percent(Some(last), now), 25.0);
        assert_eq!(
            cpu_percent(None, now),
            0.0,
            "the first reading has nothing to compare"
        );
        let same = Reading {
            container_ns: 1_000.0,
            system_ns: 10_000.0,
            mem: 0,
        };
        assert_eq!(cpu_percent(Some(last), same), 0.0);
    }

    #[test]
    fn a_process_belongs_to_the_container_in_its_cgroup_path() {
        let id = "a".repeat(64);
        let systemd = format!("0::/system.slice/docker-{id}.scope\n");
        assert_eq!(container_of(&systemd), Some(id.clone()));
        let cgroupfs = format!("12:memory:/docker/{id}\n1:name=systemd:/docker/{id}/x\n");
        assert_eq!(container_of(&cgroupfs), Some(id.clone()));
        assert_eq!(
            container_of("0::/user.slice/user-1000.slice/session-1.scope\n"),
            None
        );
        assert_eq!(
            container_of("0::/docker-short.scope\n"),
            None,
            "not a full id"
        );
        assert_eq!(container_of(""), None);
    }
}
