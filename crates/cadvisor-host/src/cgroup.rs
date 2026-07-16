//! cgroup-v2 tree reader: per-container spec and stats.
//!
//! Maps cgroup files to `cadvisor_model::v1` types with cadvisor v0.49.2
//! semantics (see crate docs). All reads are relative to a cgroup path like
//! `/machine.slice/libpod-<id>.scope` under the unified mountpoint.

use std::path::PathBuf;
use std::time::SystemTime;

use cadvisor_model::GoTime;
use cadvisor_model::v1;

use crate::HostError;
use crate::parse;

pub const UNIFIED_MOUNTPOINT: &str = "/sys/fs/cgroup";

/// Spec-relevant facts read from a cgroup directory.
#[derive(Debug, Default, Clone)]
pub struct CgroupSpec {
    pub creation_time: Option<SystemTime>,
    pub has_cpu: bool,
    pub cpu_shares: u64,
    pub cpu_quota: Option<u64>,
    pub cpu_period: u64,
    pub cpu_mask: String,
    pub has_memory: bool,
    pub memory_limit: u64,
    pub swap_limit: u64,
    pub memory_reservation: u64,
    pub has_processes: bool,
    pub pids_limit: u64,
    pub has_disk_io: bool,
}

#[derive(Debug, Clone)]
pub struct CgroupReader {
    root: PathBuf,
}

impl Default for CgroupReader {
    fn default() -> Self {
        Self::new(UNIFIED_MOUNTPOINT)
    }
}

impl CgroupReader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        CgroupReader { root: root.into() }
    }

    fn dir(&self, cgroup: &str) -> PathBuf {
        let rel = cgroup.trim_start_matches('/');
        self.root.join(rel)
    }

    pub fn exists(&self, cgroup: &str) -> bool {
        self.dir(cgroup).is_dir()
    }

    fn read(&self, cgroup: &str, file: &str) -> Result<String, HostError> {
        let path = self.dir(cgroup).join(file);
        std::fs::read_to_string(&path).map_err(|e| HostError::io(path, e))
    }

    /// Reads a file, treating "not found" as None (missing controller files
    /// are routine, e.g. on the root cgroup).
    fn read_opt(&self, cgroup: &str, file: &str) -> Result<Option<String>, HostError> {
        match self.read(cgroup, file) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn read_u64_or_max(&self, cgroup: &str, file: &str) -> Result<Option<u64>, HostError> {
        Ok(self.read_opt(cgroup, file)?.map(|s| parse::parse_u64_or_max(&s)))
    }

    /// Direct child cgroup names (absolute cgroup paths).
    pub fn list_children(&self, cgroup: &str, out: &mut Vec<String>) -> Result<(), HostError> {
        let dir = self.dir(cgroup);
        let entries = std::fs::read_dir(&dir).map_err(|e| HostError::io(&dir, e))?;
        let prefix = if cgroup == "/" { String::new() } else { cgroup.to_string() };
        for entry in entries {
            let entry = entry.map_err(|e| HostError::io(&dir, e))?;
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                out.push(format!("{prefix}/{}", entry.file_name().to_string_lossy()));
            }
        }
        Ok(())
    }

    /// PIDs in this cgroup (not recursive).
    pub fn procs(&self, cgroup: &str) -> Result<Vec<u32>, HostError> {
        let content = self.read(cgroup, "cgroup.procs")?;
        Ok(content.lines().filter_map(|l| l.trim().parse().ok()).collect())
    }

    pub fn read_spec(&self, cgroup: &str) -> Result<CgroupSpec, HostError> {
        let mut spec = CgroupSpec::default();
        let dir = self.dir(cgroup);
        spec.creation_time = std::fs::metadata(&dir)
            .map_err(|e| HostError::io(&dir, e))?
            .modified()
            .ok();

        // On the unified hierarchy, upstream reports has_cpu/has_processes/
        // has_diskio as true for EVERY cgroup (the unified path always
        // exists); only has_memory is gated, on the memory controller being
        // enabled (memory.max present) — verified against captured v0.49.2
        // /api/v2.0/spec output including controller-less child cgroups.
        spec.has_cpu = true;
        spec.has_memory = self.dir(cgroup).join("memory.max").exists() || cgroup == "/";
        spec.has_processes = true;
        spec.has_disk_io = true;

        if let Some(weight) = self.read_u64_or_max(cgroup, "cpu.weight")? {
            spec.cpu_shares = parse::cpu_weight_to_shares(weight);
        }
        if let Some(max) = self.read_opt(cgroup, "cpu.max")? {
            let (quota, period) = parse::parse_cpu_max(&max);
            spec.cpu_quota = quota;
            spec.cpu_period = period;
        }
        if let Some(mask) = self.read_opt(cgroup, "cpuset.cpus.effective")? {
            spec.cpu_mask = mask.trim().to_string();
        }
        spec.memory_limit = self.read_u64_or_max(cgroup, "memory.max")?.unwrap_or(0);
        spec.swap_limit = self.read_u64_or_max(cgroup, "memory.swap.max")?.unwrap_or(0);
        spec.memory_reservation = self.read_u64_or_max(cgroup, "memory.low")?.unwrap_or(0);
        spec.pids_limit = self.read_u64_or_max(cgroup, "pids.max")?.unwrap_or(0);
        Ok(spec)
    }

    /// Reads one stats sample. `network` is left zeroed — it needs an init
    /// PID and is filled by the caller when one is known.
    pub fn read_stats(&self, cgroup: &str, timestamp: GoTime) -> Result<v1::ContainerStats, HostError> {
        let mut s = v1::ContainerStats { timestamp, ..Default::default() };

        if let Some(content) = self.read_opt(cgroup, "cpu.stat")? {
            let c = parse::parse_cpu_stat(&content);
            s.cpu.usage.total = c.usage_usec * 1000;
            s.cpu.usage.user = c.user_usec * 1000;
            s.cpu.usage.system = c.system_usec * 1000;
            s.cpu.cfs.periods = c.nr_periods;
            s.cpu.cfs.throttled_periods = c.nr_throttled;
            s.cpu.cfs.throttled_time = c.throttled_usec * 1000;
        }

        if let Some(current) = self.read_u64_or_max(cgroup, "memory.current")? {
            s.memory.usage = current;
            s.memory.max_usage = self.read_u64_or_max(cgroup, "memory.peak")?.unwrap_or(0);
            s.memory.swap = self.read_u64_or_max(cgroup, "memory.swap.current")?.unwrap_or(0);
            if let Some(stat) = self.read_opt(cgroup, "memory.stat")? {
                let m = parse::parse_memory_stat(&stat);
                s.memory.cache = m.file;
                s.memory.rss = m.anon;
                s.memory.mapped_file = m.file_mapped;
                // The metric everyone alerts on: usage minus inactive file
                // cache, clamped at zero.
                s.memory.working_set = current.saturating_sub(m.inactive_file);
                s.memory.container_data.pgfault = m.pgfault;
                s.memory.container_data.pgmajfault = m.pgmajfault;
                s.memory.hierarchical_data.pgfault = m.pgfault;
                s.memory.hierarchical_data.pgmajfault = m.pgmajfault;
            } else {
                s.memory.working_set = current;
            }
        }

        // memory.events oom_kill backs container_oom_events_total and the
        // oom/oomKill event stream.
        if let Some(events) = self.read_opt(cgroup, "memory.events")? {
            for (k, v) in events.lines().filter_map(|l| l.split_once(' ')) {
                if k == "oom_kill" {
                    s.oom_events = v.trim().parse().unwrap_or(0);
                }
            }
        }

        if let Some(io) = self.read_opt(cgroup, "io.stat")? {
            for dev in parse::parse_io_stat(&io) {
                let entry = |op_r: u64, op_w: u64| v1::PerDiskStats {
                    device: String::new(),
                    major: dev.major,
                    minor: dev.minor,
                    stats: [("Read".to_string(), op_r), ("Write".to_string(), op_w)]
                        .into_iter()
                        .collect(),
                };
                s.disk_io.io_service_bytes.push(entry(dev.rbytes, dev.wbytes));
                s.disk_io.io_serviced.push(entry(dev.rios, dev.wios));
            }
        }
        Ok(s)
    }
}
