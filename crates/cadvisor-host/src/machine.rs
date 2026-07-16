//! Machine info assembly from /proc and /sys (Linux only).
//!
//! Produces a `v1::MachineInfo` shaped like cadvisor v0.49.2's. The captured
//! `crates/cadvisor-model/fixtures/v1_machine.json` from dev.g8.lo is the
//! oracle this implementation is checked against (`examples/dump.rs`).

use std::path::Path;

use cadvisor_model::GoTime;
use cadvisor_model::v1;

use crate::HostError;
use crate::fs::FsService;
use crate::parse;

fn read_trim(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path.as_ref()).ok().map(|s| s.trim().to_string())
}

fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read_trim(path)?.parse().ok()
}

/// Expands a cpulist string like "0-3,8,10-11" into ids.
fn parse_cpulist(list: &str) -> Vec<i64> {
    let mut out = Vec::new();
    for part in list.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.parse::<i64>(), b.parse::<i64>()) {
                out.extend(a..=b);
            }
        } else if let Ok(v) = part.parse() {
            out.push(v);
        }
    }
    out
}

/// Cache size strings look like "32K", "1280K", "24576K".
fn parse_cache_size(s: &str) -> u64 {
    let s = s.trim();
    let (num, mult) = match s.as_bytes().last() {
        Some(b'K') => (&s[..s.len() - 1], 1024),
        Some(b'M') => (&s[..s.len() - 1], 1024 * 1024),
        Some(b'G') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    num.parse::<u64>().unwrap_or(0) * mult
}

fn machine_id(candidates: &[&str]) -> String {
    candidates.iter().find_map(read_trim).unwrap_or_default()
}

fn hugepages_from(dir: &Path) -> Vec<v1::HugePagesInfo> {
    let mut out: Vec<(String, v1::HugePagesInfo)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        // hugepages-2048kB
        let Some(size) = name.strip_prefix("hugepages-").and_then(|s| s.strip_suffix("kB")) else {
            continue;
        };
        let page_size: u64 = size.parse().unwrap_or(0);
        let num_pages = read_u64(e.path().join("nr_hugepages")).unwrap_or(0);
        out.push((name, v1::HugePagesInfo { page_size, num_pages }));
    }
    // cadvisor keeps directory order, which is lexical by entry name
    // ("hugepages-1048576kB" sorts before "hugepages-2048kB").
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.into_iter().map(|(_, h)| h).collect()
}

fn per_cpu_topology(cpu_id: i64) -> Option<(i64, i64)> {
    let base = format!("/sys/devices/system/cpu/cpu{cpu_id}/topology");
    let core_id = read_u64(format!("{base}/core_id"))? as i64;
    let socket_id = read_u64(format!("{base}/physical_package_id"))? as i64;
    Some((core_id, socket_id))
}

fn cpu_caches(cpu_id: i64) -> Vec<(v1::Cache, Vec<i64>)> {
    let mut out = Vec::new();
    let base = format!("/sys/devices/system/cpu/cpu{cpu_id}/cache");
    let Ok(entries) = std::fs::read_dir(&base) else { return out };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("index") {
            continue;
        }
        let p = e.path();
        let level = read_u64(p.join("level")).unwrap_or(0) as i64;
        let cache_type = match read_trim(p.join("type")).as_deref() {
            Some("Data") => "Data",
            Some("Instruction") => "Instruction",
            Some("Unified") => "Unified",
            _ => continue,
        };
        let size = parse_cache_size(&read_trim(p.join("size")).unwrap_or_default());
        let id = read_u64(p.join("id")).unwrap_or(0) as i64;
        let shared = parse_cpulist(&read_trim(p.join("shared_cpu_list")).unwrap_or_default());
        out.push((
            v1::Cache { id, size, cache_type: cache_type.to_string(), level },
            shared,
        ));
    }
    out
}

fn topology() -> (Vec<v1::Node>, i64) {
    let mut nodes = Vec::new();
    let mut num_cpus: i64 = 0;

    let node_dir = Path::new("/sys/devices/system/node");
    let mut node_ids: Vec<i64> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(node_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(id) = name.strip_prefix("node").and_then(|s| s.parse().ok()) {
                node_ids.push(id);
            }
        }
    }
    node_ids.sort();

    for node_id in node_ids {
        let base = format!("/sys/devices/system/node/node{node_id}");
        let mut node = v1::Node {
            id: node_id,
            memory: 0,
            hugepages: hugepages_from(Path::new(&format!("{base}/hugepages"))),
            cores: Vec::new(),
            caches: Vec::new(),
            distances: parse_cpulist(&read_trim(format!("{base}/distance")).unwrap_or_default())
                .into_iter()
                .map(|v| v as u64)
                .collect(),
            ..Default::default()
        };
        // node meminfo: "Node 0 MemTotal:       16342752 kB"
        if let Some(mi) = read_trim(format!("{base}/meminfo")) {
            for line in mi.lines() {
                if line.contains("MemTotal:") {
                    let kb: u64 = line
                        .split_whitespace()
                        .rev()
                        .nth(1)
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    node.memory = kb * 1024;
                }
            }
        }

        let cpus = parse_cpulist(&read_trim(format!("{base}/cpulist")).unwrap_or_default());
        num_cpus += cpus.len() as i64;
        for &cpu in &cpus {
            let Some((core_id, socket_id)) = per_cpu_topology(cpu) else { continue };
            let idx = match node.cores.iter().position(|c| c.id == core_id) {
                Some(i) => i,
                None => {
                    node.cores.push(v1::Core {
                        id: core_id,
                        socket_id,
                        uncore_caches: None,
                        ..Default::default()
                    });
                    node.cores.len() - 1
                }
            };
            node.cores[idx].threads.push(cpu);
            node.cores[idx].threads.sort();

            for (cache, shared) in cpu_caches(cpu) {
                if cache.level < 3 {
                    // Per-core cache: attach once (first thread wins).
                    let core = &mut node.cores[idx];
                    if !core.caches.iter().any(|c| c.level == cache.level && c.cache_type == cache.cache_type) {
                        core.caches.push(cache);
                    }
                } else if shared.len() as i64 >= cpus.len() as i64 {
                    // Shared by every cpu in the node -> node-level cache.
                    if !node.caches.iter().any(|c| c.level == cache.level && c.cache_type == cache.cache_type) {
                        node.caches.push(cache);
                    }
                } else {
                    // L3 shared by a subset -> still node-level in cadvisor's
                    // fallback path; dedupe by cache id.
                    if !node.caches.iter().any(|c| c.id == cache.id && c.level == cache.level) {
                        node.caches.push(cache);
                    }
                }
            }
        }
        node.cores.sort_by_key(|c| c.id);
        // Cache order matches sysfs index order: level ascending, Data before
        // Instruction within a level.
        for core in &mut node.cores {
            core.caches.sort_by(|a, b| (a.level, &a.cache_type).cmp(&(b.level, &b.cache_type)));
        }
        nodes.push(node);
    }
    (nodes, num_cpus)
}

fn disk_map() -> std::collections::BTreeMap<String, v1::DiskInfo> {
    let mut out = std::collections::BTreeMap::new();
    let Ok(entries) = std::fs::read_dir("/sys/block") else { return out };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        // cadvisor's block-device filter (verified against real v0.49.2
        // output: includes zram, excludes sr/loop/ram).
        if ["loop", "ram", "sr", "fd"].iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let p = e.path();
        let Some(dev) = read_trim(p.join("dev")) else { continue };
        let Some((major, minor)) = dev.split_once(':') else { continue };
        let sectors = read_u64(p.join("size")).unwrap_or(0);
        let scheduler = read_trim(p.join("queue/scheduler"))
            .and_then(|s| {
                s.split_whitespace()
                    .find(|w| w.starts_with('['))
                    .map(|w| w.trim_matches(['[', ']']).to_string())
            })
            .unwrap_or_else(|| "none".to_string());
        // Keyed by "major:minor", matching real cadvisor output.
        out.insert(
            dev.clone(),
            v1::DiskInfo {
                name,
                major: major.parse().unwrap_or(0),
                minor: minor.parse().unwrap_or(0),
                size: sectors * 512,
                scheduler,
            },
        );
    }
    out
}

fn network_devices() -> Vec<v1::NetInfo> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else { return out };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        // cadvisor filters by name prefix, NOT by virtual-ness: real v0.49.2
        // output includes bridges (podman0) but never lo/veth/docker.
        if ["lo", "veth", "docker"].iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let p = Path::new("/sys/class/net").join(&name);
        out.push(v1::NetInfo {
            mac_address: read_trim(p.join("address")).unwrap_or_default(),
            speed: read_trim(p.join("speed")).and_then(|s| s.parse().ok()).unwrap_or(0),
            mtu: read_trim(p.join("mtu")).and_then(|s| s.parse().ok()).unwrap_or(0),
            name,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Assembles MachineInfo the way cadvisor v0.49.2 does.
pub fn machine_info(fs: &FsService, timestamp: GoTime) -> Result<v1::MachineInfo, HostError> {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo")
        .map_err(|e| HostError::io("/proc/cpuinfo", e))?;
    let cpu = parse::parse_cpuinfo(&cpuinfo);
    let meminfo = std::fs::read_to_string("/proc/meminfo")
        .map_err(|e| HostError::io("/proc/meminfo", e))?;
    let mem = parse::parse_meminfo(&meminfo);

    let cpu_frequency = read_u64("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")
        .unwrap_or(cpu.khz_fallback);

    let (nodes, num_cores) = topology();

    Ok(v1::MachineInfo {
        timestamp,
        cpu_vendor_id: cpu.vendor_id,
        num_cores,
        num_physical_cores: cpu.physical_cores,
        num_sockets: cpu.sockets,
        cpu_frequency,
        memory_capacity: mem.total_bytes,
        swap_capacity: mem.swap_bytes,
        memory_by_type: Default::default(),
        nvm_info: Default::default(),
        huge_pages: hugepages_from(Path::new("/sys/kernel/mm/hugepages")),
        machine_id: machine_id(&["/etc/machine-id", "/var/lib/dbus/machine-id"]),
        system_uuid: machine_id(&["/sys/class/dmi/id/product_uuid"]),
        boot_id: machine_id(&["/proc/sys/kernel/random/boot_id"]),
        filesystems: fs.machine_filesystems(),
        disk_map: disk_map(),
        network_devices: network_devices(),
        topology: nodes,
        cloud_provider: v1::UNKNOWN_PROVIDER.to_string(),
        instance_type: v1::UNKNOWN_INSTANCE.to_string(),
        instance_id: v1::UNNAMED_INSTANCE.to_string(),
    })
}
