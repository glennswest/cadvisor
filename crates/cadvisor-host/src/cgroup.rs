//! cgroup-v2 tree reader: per-container spec and stats.
//!
//! Maps cgroup files to `cadvisor_model::v1` types with cadvisor v0.49.2
//! semantics (see crate docs). All reads are relative to a cgroup path like
//! `/machine.slice/libpod-<id>.scope` under the unified mountpoint.

use std::collections::BTreeMap;
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

    /// PIDs in this cgroup and every cgroup below it (upstream's
    /// `GetAllPids`). A cgroup removed mid-walk is skipped.
    pub fn procs_recursive(&self, cgroup: &str) -> Result<Vec<u32>, HostError> {
        let mut out = self.procs(cgroup)?;
        let mut stack = Vec::new();
        self.list_children(cgroup, &mut stack)?;
        while let Some(c) = stack.pop() {
            match self.procs(&c) {
                Ok(p) => out.extend(p),
                Err(e) if e.is_not_found() => continue,
                Err(e) => return Err(e),
            }
            let _ = self.list_children(&c, &mut stack);
        }
        Ok(out)
    }

    /// Hugepage stats per page size from `hugetlb.<size>.current` and
    /// `hugetlb.<size>.events` (`max` = failcnt), as runc's fs2 reads them.
    /// cgroup v2 keeps no peak, so `max_usage` is 0 (as upstream). Empty when
    /// the hugetlb controller is not enabled for this cgroup.
    pub fn read_hugetlb(&self, cgroup: &str) -> Result<BTreeMap<String, v1::HugetlbStats>, HostError> {
        let dir = self.dir(cgroup);
        let mut out = BTreeMap::new();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(HostError::io(&dir, e)),
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(size) = name
                .to_str()
                .and_then(|n| n.strip_prefix("hugetlb."))
                .and_then(|n| n.strip_suffix(".current"))
                .filter(|s| !s.contains('.'))
            else {
                continue;
            };
            let usage = self.read_u64_or_max(cgroup, &format!("hugetlb.{size}.current"))?.unwrap_or(0);
            let failcnt = self
                .read_opt(cgroup, &format!("hugetlb.{size}.events"))?
                .map_or(0, |c| parse::parse_hugetlb_events_max(&c));
            out.insert(size.to_string(), v1::HugetlbStats { usage, max_usage: 0, failcnt });
        }
        Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cadvisor-cg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pod/c1")).unwrap();
        std::fs::create_dir_all(dir.join("pod/c2")).unwrap();
        dir
    }

    #[test]
    fn procs_recursive_takes_every_descendant() {
        let dir = tree("procs");
        std::fs::write(dir.join("pod/cgroup.procs"), "").unwrap();
        std::fs::write(dir.join("pod/c1/cgroup.procs"), "10\n11\n").unwrap();
        std::fs::write(dir.join("pod/c2/cgroup.procs"), "20\n").unwrap();
        let r = CgroupReader::new(&dir);
        let mut pids = r.procs_recursive("/pod").unwrap();
        pids.sort();
        assert_eq!(pids, vec![10, 11, 20]);
        assert_eq!(r.procs("/pod").unwrap(), Vec::<u32>::new());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hugetlb_per_page_size() {
        let dir = tree("huge");
        let c = dir.join("pod/c1");
        std::fs::write(c.join("hugetlb.2MB.current"), "4194304\n").unwrap();
        std::fs::write(c.join("hugetlb.2MB.events"), "max 3\n").unwrap();
        std::fs::write(c.join("hugetlb.2MB.max"), "max\n").unwrap();
        std::fs::write(c.join("hugetlb.1GB.current"), "0\n").unwrap();
        std::fs::write(c.join("hugetlb.1GB.events"), "max 0\n").unwrap();
        std::fs::write(c.join("hugetlb.1GB.rsvd.current"), "0\n").unwrap();
        let r = CgroupReader::new(&dir);
        let h = r.read_hugetlb("/pod/c1").unwrap();
        assert_eq!(h.keys().collect::<Vec<_>>(), ["1GB", "2MB"]);
        assert_eq!(h["2MB"], v1::HugetlbStats { usage: 4194304, max_usage: 0, failcnt: 3 });
        assert!(r.read_hugetlb("/pod/c2").unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
