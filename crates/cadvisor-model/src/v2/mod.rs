//! Types mirroring google/cadvisor `info/v2` (v2.0/v2.1 REST API shapes).
//!
//! `TcpStat`/`UdpStat`/`TcpAdvancedStat`/`InterfaceStats` and the shared stat
//! structs are re-used from [`crate::v1`] — upstream's v2 either aliases or
//! redeclares them with identical wire shapes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::GoTime;
use crate::omit::is_zero;
use crate::v1;

/// Container identifier types accepted by `RequestOptions.IdType`.
pub const TYPE_NAME: &str = "name";
pub const TYPE_DOCKER: &str = "docker";
pub const TYPE_PODMAN: &str = "podman";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerInfo {
    pub spec: ContainerSpec,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stats: Vec<ContainerStats>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerSpec {
    pub creation_time: GoTime,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub namespace: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub envs: BTreeMap<String, String>,
    pub has_cpu: bool,
    pub cpu: v1::CpuSpec,
    pub has_memory: bool,
    pub memory: v1::MemorySpec,
    pub has_hugetlb: bool,
    pub has_custom_metrics: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub custom_metrics: Vec<v1::MetricSpec>,
    pub has_processes: bool,
    pub processes: v1::ProcessSpec,
    pub has_network: bool,
    pub has_filesystem: bool,
    #[serde(rename = "has_diskio")]
    pub has_disk_io: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub image: String,
}

/// The v2.0 `/stats` sample shape (`has_*` booleans, non-pointer fields).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeprecatedContainerStats {
    pub timestamp: GoTime,
    pub has_cpu: bool,
    pub cpu: v1::CpuStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_inst: Option<v1::CpuInstStats>,
    #[serde(rename = "has_diskio")]
    pub has_disk_io: bool,
    #[serde(rename = "diskio")]
    pub disk_io: v1::DiskIoStats,
    pub has_memory: bool,
    pub memory: v1::MemoryStats,
    pub has_hugetlb: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub hugetlb: BTreeMap<String, v1::HugetlbStats>,
    pub has_network: bool,
    pub network: NetworkStats,
    pub has_processes: bool,
    pub processes: v1::ProcessStats,
    pub has_filesystem: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub filesystem: Vec<v1::FsStats>,
    pub has_load: bool,
    #[serde(rename = "load_stats")]
    pub load: v1::LoadStats,
    pub has_custom_metrics: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_metrics: BTreeMap<String, Vec<v1::MetricVal>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub perf_stats: Vec<v1::PerfStat>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub perf_uncore_stats: Vec<v1::PerfUncoreStat>,
    #[serde(skip_serializing_if = "is_zero")]
    pub referenced_memory: u64,
    pub resctrl: v1::ResctrlStats,
}

/// The v2.1 `/stats` sample shape (pointer fields).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerStats {
    pub timestamp: GoTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<v1::CpuStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_inst: Option<v1::CpuInstStats>,
    #[serde(rename = "diskio", skip_serializing_if = "Option::is_none")]
    pub disk_io: Option<v1::DiskIoStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<v1::MemoryStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hugetlb: Option<BTreeMap<String, v1::HugetlbStats>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processes: Option<v1::ProcessStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<FilesystemStats>,
    #[serde(rename = "load_stats", skip_serializing_if = "Option::is_none")]
    pub load: Option<v1::LoadStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub accelerators: Vec<v1::AcceleratorStats>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_metrics: BTreeMap<String, Vec<v1::MetricVal>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub perf_stats: Vec<v1::PerfStat>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub perf_uncore_stats: Vec<v1::PerfUncoreStat>,
    #[serde(skip_serializing_if = "is_zero")]
    pub referenced_memory: u64,
    pub resctrl: v1::ResctrlStats,
}

/// v2 network stats: no inlined default interface (unlike v1).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkStats {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<v1::InterfaceStats>,
    pub tcp: v1::TcpStat,
    pub tcp6: v1::TcpStat,
    pub udp: v1::UdpStat,
    pub udp6: v1::UdpStat,
    pub tcp_advanced: v1::TcpAdvancedStat,
}

/// Per-container filesystem rollup (v2.1 `filesystem` field).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesystemStats {
    #[serde(rename = "totalUsageBytes", skip_serializing_if = "Option::is_none")]
    pub total_usage_bytes: Option<u64>,
    #[serde(rename = "baseUsageBytes", skip_serializing_if = "Option::is_none")]
    pub base_usage_bytes: Option<u64>,
    /// Upstream key is misspelled ("containter"); replicated verbatim.
    #[serde(rename = "containter_inode_usage", skip_serializing_if = "Option::is_none")]
    pub inode_usage: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Attributes {
    pub kernel_version: String,
    pub container_os_version: String,
    pub docker_version: String,
    #[serde(rename = "docker_api_version")]
    pub docker_api_version: String,
    pub cadvisor_version: String,
    pub num_cores: i64,
    #[serde(rename = "cpu_frequency_khz")]
    pub cpu_frequency: u64,
    pub memory_capacity: u64,
    pub machine_id: String,
    pub system_uuid: String,
    #[serde(rename = "hugepages")]
    pub huge_pages: Vec<v1::HugePagesInfo>,
    pub filesystems: Vec<v1::FilesystemInfo>,
    pub disk_map: BTreeMap<String, v1::DiskInfo>,
    pub network_devices: Vec<v1::NetInfo>,
    pub topology: Vec<v1::Node>,
    pub cloud_provider: v1::CloudProvider,
    pub instance_type: v1::InstanceType,
}

impl Attributes {
    pub fn new(mi: &v1::MachineInfo, vi: &v1::VersionInfo) -> Self {
        Attributes {
            kernel_version: vi.kernel_version.clone(),
            container_os_version: vi.container_os_version.clone(),
            docker_version: vi.docker_version.clone(),
            docker_api_version: vi.docker_api_version.clone(),
            cadvisor_version: vi.cadvisor_version.clone(),
            num_cores: mi.num_cores,
            cpu_frequency: mi.cpu_frequency,
            memory_capacity: mi.memory_capacity,
            machine_id: mi.machine_id.clone(),
            system_uuid: mi.system_uuid.clone(),
            huge_pages: mi.huge_pages.clone(),
            filesystems: mi.filesystems.clone(),
            disk_map: mi.disk_map.clone(),
            network_devices: mi.network_devices.clone(),
            topology: mi.topology.clone(),
            cloud_provider: mi.cloud_provider.clone(),
            instance_type: mi.instance_type.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MachineStats {
    pub timestamp: GoTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<v1::CpuStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_inst: Option<v1::CpuInstStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<v1::MemoryStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub filesystem: Vec<MachineFsStats>,
    #[serde(rename = "load_stats", skip_serializing_if = "Option::is_none")]
    pub load: Option<v1::LoadStats>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MachineFsStats {
    pub device: String,
    #[serde(rename = "type")]
    pub fs_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inodes_free: Option<u64>,
    /// Upstream embeds DiskStats with the tag `json:"inline"` (note: no
    /// leading comma), which Go treats as a NAMED field — the disk stats nest
    /// under a literal "inline" key. Replicated verbatim.
    #[serde(rename = "inline")]
    pub disk_stats: DiskStats,
}

/// Machine-level per-partition disk stats. The Go `*time.Duration` fields
/// marshal as integer nanoseconds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiskStats {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reads_completed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reads_merged: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sectors_read: Option<u64>,
    /// Nanoseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_duration: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writes_completed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writes_merged: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sectors_written: Option<u64>,
    /// Nanoseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_duration: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_in_progress: Option<u64>,
    /// Nanoseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_duration: Option<i64>,
    /// Nanoseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weighted_io_duration: Option<i64>,
}

/// v2 `/ps` endpoint row.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcessInfo {
    pub user: String,
    pub pid: i64,
    #[serde(rename = "parent_pid")]
    pub ppid: i64,
    pub start_time: String,
    pub percent_cpu: f32,
    #[serde(rename = "percent_mem")]
    pub percent_memory: f32,
    pub rss: u64,
    pub virtual_size: u64,
    pub status: String,
    pub running_time: String,
    pub cgroup_path: String,
    pub cmd: String,
    pub fd_count: i64,
    pub psr: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Percentiles {
    /// Whether the stats are present or not.
    pub present: bool,
    pub mean: u64,
    pub max: u64,
    pub fifty: u64,
    pub ninety: u64,
    #[serde(rename = "ninetyfive")]
    pub ninety_five: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    /// Amount of data available [0-100].
    pub percent_complete: i32,
    /// Cpu rate in milliCpus/second.
    pub cpu: Percentiles,
    /// Memory size in bytes.
    pub memory: Percentiles,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstantUsage {
    /// Cpu rate in cpu milliseconds/second.
    pub cpu: u64,
    /// Memory usage in bytes.
    pub memory: u64,
}

/// v2 `/summary` endpoint payload.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DerivedStats {
    pub timestamp: GoTime,
    pub latest_usage: InstantUsage,
    pub minute_usage: Usage,
    pub hour_usage: Usage,
    pub day_usage: Usage,
}

/// v2 `/storage` endpoint row (runtime per-filesystem info).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FsInfo {
    pub timestamp: GoTime,
    pub device: String,
    pub mountpoint: String,
    pub capacity: u64,
    pub available: u64,
    pub usage: u64,
    /// Go nil slice serializes as null (no omitempty), so None <-> null.
    pub labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inodes_free: Option<u64>,
}

/// Query options for the v2 API (parsed from query params, not a wire body).
#[derive(Debug, Clone, PartialEq)]
pub struct RequestOptions {
    /// TYPE_NAME (default), TYPE_DOCKER, or TYPE_PODMAN.
    pub id_type: String,
    /// Number of stats to return; -1 means no limit.
    pub count: i64,
    /// Whether to include stats for child subcontainers.
    pub recursive: bool,
    /// Update stats if older than this; None means no update.
    pub max_age: Option<std::time::Duration>,
}

impl Default for RequestOptions {
    fn default() -> Self {
        RequestOptions {
            id_type: TYPE_NAME.to_string(),
            count: 64,
            recursive: false,
            max_age: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filesystem_stats_replicates_containter_typo() {
        let fs = FilesystemStats { inode_usage: Some(12), ..Default::default() };
        let v = serde_json::to_value(&fs).unwrap();
        assert_eq!(v["containter_inode_usage"], json!(12));
        assert_eq!(serde_json::to_value(FilesystemStats::default()).unwrap(), json!({}));
    }

    #[test]
    fn machine_fs_stats_nests_disk_stats_under_inline_key() {
        let fs = MachineFsStats {
            device: "/dev/sda1".into(),
            fs_type: "vfs".into(),
            disk_stats: DiskStats { reads_completed: Some(10), ..Default::default() },
            ..Default::default()
        };
        let v = serde_json::to_value(&fs).unwrap();
        assert_eq!(v["inline"]["reads_completed"], json!(10));
        assert_eq!(
            v.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["device", "type", "inline"]
        );
    }

    #[test]
    fn deprecated_stats_zero_value_keys() {
        let v = serde_json::to_value(DeprecatedContainerStats::default()).unwrap();
        let keys: Vec<_> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "timestamp", "has_cpu", "cpu", "has_diskio", "diskio", "has_memory", "memory",
                "has_hugetlb", "has_network", "network", "has_processes", "processes",
                "has_filesystem", "has_load", "load_stats", "has_custom_metrics", "resctrl",
            ]
        );
    }

    #[test]
    fn v21_stats_zero_value_keys() {
        let v = serde_json::to_value(ContainerStats::default()).unwrap();
        let keys: Vec<_> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["timestamp", "resctrl"]);
    }

    #[test]
    fn summary_shape() {
        let v = serde_json::to_value(DerivedStats::default()).unwrap();
        assert_eq!(v["minute_usage"]["cpu"]["ninetyfive"], json!(0));
        assert_eq!(v["latest_usage"], json!({"cpu": 0, "memory": 0}));
    }

    #[test]
    fn process_info_wire_keys() {
        let v = serde_json::to_value(ProcessInfo::default()).unwrap();
        assert!(v.get("parent_pid").is_some());
        assert!(v.get("percent_mem").is_some());
        assert!(v.get("ppid").is_none());
    }
}
