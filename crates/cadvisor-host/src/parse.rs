//! Pure parsers for cgroup-v2, procfs, and sysfs file contents.
//!
//! Every function takes `&str` so the full parse matrix runs on any platform.
//! Numeric parse failures yield 0 for individual fields (matching cadvisor's
//! lenient readers) — a malformed kernel file should degrade, not error.

use cadvisor_model::v1::{InterfaceStats, TcpStat, UdpStat};

fn u64_field(s: &str) -> u64 {
    s.parse().unwrap_or(0)
}

/// Iterates `key value` lines of a flat-keyed cgroup file.
fn flat_keyed(content: &str) -> impl Iterator<Item = (&str, &str)> {
    content.lines().filter_map(|l| l.split_once(' '))
}

/// `cpu.stat` — µs values (converted to ns by the caller via [`CpuStatFile`]).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct CpuStatFile {
    pub usage_usec: u64,
    pub user_usec: u64,
    pub system_usec: u64,
    pub nr_periods: u64,
    pub nr_throttled: u64,
    pub throttled_usec: u64,
}

pub fn parse_cpu_stat(content: &str) -> CpuStatFile {
    let mut out = CpuStatFile::default();
    for (k, v) in flat_keyed(content) {
        let v = u64_field(v);
        match k {
            "usage_usec" => out.usage_usec = v,
            "user_usec" => out.user_usec = v,
            "system_usec" => out.system_usec = v,
            "nr_periods" => out.nr_periods = v,
            "nr_throttled" => out.nr_throttled = v,
            "throttled_usec" => out.throttled_usec = v,
            _ => {}
        }
    }
    out
}

/// The `memory.stat` keys cadvisor v0.49.2 consumes on cgroup v2.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MemoryStatFile {
    pub anon: u64,
    pub file: u64,
    pub file_mapped: u64,
    pub inactive_file: u64,
    pub pgfault: u64,
    pub pgmajfault: u64,
}

pub fn parse_memory_stat(content: &str) -> MemoryStatFile {
    let mut out = MemoryStatFile::default();
    for (k, v) in flat_keyed(content) {
        let v = u64_field(v);
        match k {
            "anon" => out.anon = v,
            "file" => out.file = v,
            "file_mapped" => out.file_mapped = v,
            "inactive_file" => out.inactive_file = v,
            "pgfault" => out.pgfault = v,
            "pgmajfault" => out.pgmajfault = v,
            _ => {}
        }
    }
    out
}

/// Single-value cgroup file; `"max"` means unlimited (u64::MAX, which is what
/// cadvisor reports for unlimited memory).
pub fn parse_u64_or_max(content: &str) -> u64 {
    let t = content.trim();
    if t == "max" { u64::MAX } else { t.parse().unwrap_or(0) }
}

/// `cpu.max`: `"$MAX $PERIOD"` where MAX is a number or `"max"` (no quota).
pub fn parse_cpu_max(content: &str) -> (Option<u64>, u64) {
    let mut it = content.split_whitespace();
    let quota = match it.next() {
        Some("max") | None => None,
        Some(v) => v.parse().ok(),
    };
    let period = it.next().map(u64_field).unwrap_or(100_000);
    (quota, period)
}

/// runc's cgroup-v2 conversion, inverted: cadvisor reports v1-style cpu shares
/// derived from `cpu.weight`.
pub fn cpu_weight_to_shares(weight: u64) -> u64 {
    if weight == 0 { 0 } else { 2 + ((weight - 1) * 262142) / 9999 }
}

/// One device row of `io.stat`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct IoDevStat {
    pub major: u64,
    pub minor: u64,
    pub rbytes: u64,
    pub wbytes: u64,
    pub rios: u64,
    pub wios: u64,
}

pub fn parse_io_stat(content: &str) -> Vec<IoDevStat> {
    let mut out = Vec::new();
    for line in content.lines() {
        let mut fields = line.split_whitespace();
        let Some(dev) = fields.next() else { continue };
        let Some((major, minor)) = dev.split_once(':') else { continue };
        let mut row = IoDevStat {
            major: u64_field(major),
            minor: u64_field(minor),
            ..Default::default()
        };
        for kv in fields {
            let Some((k, v)) = kv.split_once('=') else { continue };
            let v = u64_field(v);
            match k {
                "rbytes" => row.rbytes = v,
                "wbytes" => row.wbytes = v,
                "rios" => row.rios = v,
                "wios" => row.wios = v,
                _ => {}
            }
        }
        out.push(row);
    }
    out
}

/// Interfaces cadvisor ignores in `/proc/<pid>/net/dev`.
fn ignored_interface(name: &str) -> bool {
    ["lo", "veth", "docker", "nerdctl"]
        .iter()
        .any(|p| name.to_ascii_lowercase().starts_with(p))
}

/// `/proc/<pid>/net/dev`: 2 header lines, then
/// `iface: rx(bytes packets errs drop fifo frame compressed multicast) tx(...)`.
/// cadvisor requires exactly 17 fields and takes rx fields 1-4, tx fields 9-12.
pub fn parse_proc_net_dev(content: &str) -> Vec<InterfaceStats> {
    let mut out = Vec::new();
    for line in content.lines().skip(2) {
        let cleaned = line.replacen(':', " ", 1);
        let f: Vec<&str> = cleaned.split_whitespace().collect();
        if f.len() != 17 || ignored_interface(f[0]) {
            continue;
        }
        out.push(InterfaceStats {
            name: f[0].to_string(),
            rx_bytes: u64_field(f[1]),
            rx_packets: u64_field(f[2]),
            rx_errors: u64_field(f[3]),
            rx_dropped: u64_field(f[4]),
            tx_bytes: u64_field(f[9]),
            tx_packets: u64_field(f[10]),
            tx_errors: u64_field(f[11]),
            tx_dropped: u64_field(f[12]),
        });
    }
    out
}

/// `/proc/<pid>/net/tcp[6]`: counts connections by state (4th column, hex).
pub fn parse_tcp_states(content: &str) -> TcpStat {
    let mut out = TcpStat::default();
    for line in content.lines().skip(1) {
        let Some(st) = line.split_whitespace().nth(3) else { continue };
        match st {
            "01" => out.established += 1,
            "02" => out.syn_sent += 1,
            "03" => out.syn_recv += 1,
            "04" => out.fin_wait1 += 1,
            "05" => out.fin_wait2 += 1,
            "06" => out.time_wait += 1,
            "07" => out.close += 1,
            "08" => out.close_wait += 1,
            "09" => out.last_ack += 1,
            "0A" => out.listen += 1,
            "0B" => out.closing += 1,
            _ => {}
        }
    }
    out
}

/// `/proc/<pid>/net/udp[6]`: Listen = row count; queued from `tx:rx` (hex);
/// Dropped from the trailing drops column.
pub fn parse_udp_stats(content: &str) -> UdpStat {
    let mut out = UdpStat::default();
    for line in content.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 13 {
            continue;
        }
        out.listen += 1;
        if let Some((tx, rx)) = f[4].split_once(':') {
            out.tx_queued += u64::from_str_radix(tx, 16).unwrap_or(0);
            out.rx_queued += u64::from_str_radix(rx, 16).unwrap_or(0);
        }
        out.dropped += u64_field(f[12]);
    }
    out
}

/// `/proc/meminfo` totals, in bytes.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MemInfo {
    pub total_bytes: u64,
    pub swap_bytes: u64,
}

pub fn parse_meminfo(content: &str) -> MemInfo {
    let mut out = MemInfo::default();
    for line in content.lines() {
        let mut f = line.split_whitespace();
        match f.next() {
            Some("MemTotal:") => out.total_bytes = f.next().map(u64_field).unwrap_or(0) * 1024,
            Some("SwapTotal:") => out.swap_bytes = f.next().map(u64_field).unwrap_or(0) * 1024,
            _ => {}
        }
    }
    out
}

/// Summary of `/proc/cpuinfo`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct CpuInfoSummary {
    pub vendor_id: String,
    pub physical_cores: i64,
    pub sockets: i64,
    /// First `cpu MHz` entry ×1000 — fallback when cpufreq sysfs is absent.
    pub khz_fallback: u64,
}

pub fn parse_cpuinfo(content: &str) -> CpuInfoSummary {
    let mut out = CpuInfoSummary::default();
    let mut socket_ids = std::collections::BTreeSet::new();
    let mut cores = std::collections::BTreeSet::new();
    let mut cur_physical_id: i64 = 0;
    for line in content.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "vendor_id" if out.vendor_id.is_empty() => out.vendor_id = v.to_string(),
            "physical id" => {
                cur_physical_id = v.parse().unwrap_or(0);
                socket_ids.insert(cur_physical_id);
            }
            "core id" => {
                cores.insert((cur_physical_id, v.parse::<i64>().unwrap_or(0)));
            }
            "cpu MHz" if out.khz_fallback == 0 => {
                out.khz_fallback = (v.parse::<f64>().unwrap_or(0.0) * 1000.0) as u64;
            }
            _ => {}
        }
    }
    out.physical_cores = cores.len() as i64;
    out.sockets = socket_ids.len() as i64;
    out
}

/// cadvisor's `/proc/diskstats` device filter: `^(s|v|xv)d[a-z]+\d*$` or `^dm-\d+$`.
pub fn diskstats_device_matches(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("dm-") {
        return !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit());
    }
    let rest = name
        .strip_prefix("sd")
        .or_else(|| name.strip_prefix("vd"))
        .or_else(|| name.strip_prefix("xvd"));
    let Some(rest) = rest else { return false };
    let letters = rest.bytes().take_while(|b| b.is_ascii_lowercase()).count();
    letters >= 1 && rest.bytes().skip(letters).all(|b| b.is_ascii_digit())
}

/// One (filtered) row of `/proc/diskstats`: major minor name + 11 stat columns.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DiskStatsLine {
    pub major: u64,
    pub minor: u64,
    pub name: String,
    pub reads_completed: u64,
    pub reads_merged: u64,
    pub sectors_read: u64,
    pub read_time: u64,
    pub writes_completed: u64,
    pub writes_merged: u64,
    pub sectors_written: u64,
    pub write_time: u64,
    pub io_in_progress: u64,
    pub io_time: u64,
    pub weighted_io_time: u64,
}

pub fn parse_diskstats(content: &str) -> Vec<DiskStatsLine> {
    let mut out = Vec::new();
    for line in content.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 14 || !diskstats_device_matches(f[2]) {
            continue;
        }
        out.push(DiskStatsLine {
            major: u64_field(f[0]),
            minor: u64_field(f[1]),
            name: f[2].to_string(),
            reads_completed: u64_field(f[3]),
            reads_merged: u64_field(f[4]),
            sectors_read: u64_field(f[5]),
            read_time: u64_field(f[6]),
            writes_completed: u64_field(f[7]),
            writes_merged: u64_field(f[8]),
            sectors_written: u64_field(f[9]),
            write_time: u64_field(f[10]),
            io_in_progress: u64_field(f[11]),
            io_time: u64_field(f[12]),
            weighted_io_time: u64_field(f[13]),
        });
    }
    out
}

/// One row of `/proc/self/mountinfo` (fields per `proc(5)`).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MountInfoLine {
    pub major: u64,
    pub minor: u64,
    pub mountpoint: String,
    pub fs_type: String,
    pub source: String,
}

pub fn parse_mountinfo(content: &str) -> Vec<MountInfoLine> {
    let mut out = Vec::new();
    for line in content.lines() {
        // <id> <parent> <maj:min> <root> <mountpoint> <opts> [optional...] - <fstype> <source> <super opts>
        let Some((pre, post)) = line.split_once(" - ") else { continue };
        let pre: Vec<&str> = pre.split_whitespace().collect();
        let post: Vec<&str> = post.split_whitespace().collect();
        if pre.len() < 5 || post.len() < 2 {
            continue;
        }
        let Some((major, minor)) = pre[2].split_once(':') else { continue };
        out.push(MountInfoLine {
            major: u64_field(major),
            minor: u64_field(minor),
            mountpoint: pre[4].to_string(),
            fs_type: post[0].to_string(),
            source: post[1].to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CPU_STAT: &str = include_str!("../fixtures/cpu.stat");
    const MEMORY_STAT: &str = include_str!("../fixtures/memory.stat");
    const NET_DEV: &str = include_str!("../fixtures/proc_net_dev");
    const CPUINFO: &str = include_str!("../fixtures/proc_cpuinfo");
    const MEMINFO: &str = include_str!("../fixtures/proc_meminfo");
    const DISKSTATS: &str = include_str!("../fixtures/proc_diskstats");
    const MOUNTINFO: &str = include_str!("../fixtures/proc_mountinfo");

    #[test]
    fn cpu_stat_real_fixture() {
        let c = parse_cpu_stat(CPU_STAT);
        assert_eq!(c.usage_usec, 8_884_177);
        assert_eq!(c.user_usec, 4_314_716);
        assert_eq!(c.system_usec, 4_569_460);
    }

    #[test]
    fn memory_stat_real_fixture() {
        let m = parse_memory_stat(MEMORY_STAT);
        assert_eq!(m.anon, 278_528);
        assert_eq!(m.file, 4_096);
        assert!(m.pgfault > 0);
    }

    #[test]
    fn u64_or_max() {
        assert_eq!(parse_u64_or_max("max\n"), u64::MAX);
        assert_eq!(parse_u64_or_max("1867776\n"), 1_867_776);
    }

    #[test]
    fn cpu_max_variants() {
        assert_eq!(parse_cpu_max("max 100000\n"), (None, 100_000));
        assert_eq!(parse_cpu_max("50000 100000\n"), (Some(50_000), 100_000));
    }

    #[test]
    fn weight_to_shares_matches_v0492() {
        // podman default cpu.weight=100 -> cadvisor reports shares 2597
        // (verified against the captured v1 spec fixture).
        assert_eq!(cpu_weight_to_shares(100), 2597);
        assert_eq!(cpu_weight_to_shares(1), 2);
        assert_eq!(cpu_weight_to_shares(10000), 262144);
    }

    #[test]
    fn io_stat_rows() {
        let rows = parse_io_stat("259:0 rbytes=1024 wbytes=2048 rios=3 wios=4 dbytes=0 dios=0\n8:16 rbytes=5 wbytes=6 rios=7 wios=8\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], IoDevStat { major: 259, minor: 0, rbytes: 1024, wbytes: 2048, rios: 3, wios: 4 });
        assert_eq!(parse_io_stat(""), vec![]);
    }

    #[test]
    fn net_dev_skips_lo_and_parses_eth0() {
        let ifaces = parse_proc_net_dev(NET_DEV);
        assert_eq!(ifaces.len(), 1);
        let eth0 = &ifaces[0];
        assert_eq!(eth0.name, "eth0");
        assert_eq!(eth0.rx_bytes, 8513);
        assert_eq!(eth0.rx_packets, 76);
        assert_eq!(eth0.tx_bytes, 908);
        assert_eq!(eth0.tx_packets, 12);
    }

    #[test]
    fn tcp_state_counting() {
        let data = "  sl  local_address rem_address   st ...\n\
             0: 0100007F:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 c0\n\
             1: 0100007F:AAAA 0100007F:0016 01 00000000:00000000 00:00000000 00000000     0        0 2 1 c0\n\
             2: 0100007F:BBBB 0100007F:0016 06 00000000:00000000 00:00000000 00000000     0        0 3 1 c0\n";
        let tcp = parse_tcp_states(data);
        assert_eq!(tcp.listen, 1);
        assert_eq!(tcp.established, 1);
        assert_eq!(tcp.time_wait, 1);
    }

    #[test]
    fn udp_stats() {
        let data = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n\
             0: 0100007F:0035 00000000:0000 07 00000010:00000020 00:00000000 00000000     0        0 123 2 ffff 5\n";
        let udp = parse_udp_stats(data);
        assert_eq!(udp.listen, 1);
        assert_eq!(udp.tx_queued, 0x10);
        assert_eq!(udp.rx_queued, 0x20);
        assert_eq!(udp.dropped, 5);
    }

    #[test]
    fn meminfo_real_fixture() {
        let m = parse_meminfo(MEMINFO);
        assert_eq!(m.total_bytes, 16_342_752 * 1024);
    }

    #[test]
    fn cpuinfo_real_fixture() {
        let c = parse_cpuinfo(CPUINFO);
        assert_eq!(c.vendor_id, "GenuineIntel");
        assert_eq!(c.sockets, 1);
        assert_eq!(c.physical_cores, 8);
        assert_eq!(c.khz_fallback, 3_494_400);
    }

    #[test]
    fn diskstats_filters_devices() {
        let rows = parse_diskstats(DISKSTATS);
        assert!(rows.iter().any(|r| r.name == "sda"));
        // partitions like sda1 match the upstream regex too; sr0 must not.
        assert!(rows.iter().any(|r| r.name == "sda1"));
        assert!(!rows.iter().any(|r| r.name == "sr0"));
        let sda = rows.iter().find(|r| r.name == "sda").unwrap();
        assert_eq!(sda.reads_completed, 880_123);
        assert_eq!(sda.weighted_io_time, 133_243_114);
    }

    #[test]
    fn device_filter_edge_cases() {
        for ok in ["sda", "sdb2", "vda", "xvda1", "dm-0", "dm-12"] {
            assert!(diskstats_device_matches(ok), "{ok} should match");
        }
        for bad in ["sr0", "loop0", "nbd0", "sd", "dm-", "dm-x", "sda1x"] {
            assert!(!diskstats_device_matches(bad), "{bad} should not match");
        }
    }

    #[test]
    fn mountinfo_real_fixture() {
        let rows = parse_mountinfo(MOUNTINFO);
        let root = rows.iter().find(|r| r.mountpoint == "/").unwrap();
        assert_eq!(root.fs_type, "btrfs");
        assert_eq!(root.source, "/dev/sda4");
        assert_eq!((root.major, root.minor), (0, 33));
    }
}
