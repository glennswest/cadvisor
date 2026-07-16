//! Machine/version types mirroring upstream `lib/model/machine.go`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::GoTime;

/// Machine-level filesystem description (upstream `FilesystemInfo`, which
/// `info/v1` aliases as `FsInfo` — distinct from the v2 runtime `FsInfo`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesystemInfo {
    pub device: String,
    /// `json:"-"` upstream; used only for blkio correlation.
    #[serde(skip)]
    pub device_major: u64,
    #[serde(skip)]
    pub device_minor: u64,
    pub capacity: u64,
    #[serde(rename = "type")]
    pub fs_type: String,
    pub inodes: u64,
    pub has_inodes: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Node {
    #[serde(rename = "node_id")]
    pub id: i64,
    pub memory: u64,
    pub hugepages: Vec<HugePagesInfo>,
    pub cores: Vec<Core>,
    pub caches: Vec<Cache>,
    pub distances: Vec<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Core {
    #[serde(rename = "core_id")]
    pub id: i64,
    #[serde(rename = "thread_ids")]
    pub threads: Vec<i64>,
    pub caches: Vec<Cache>,
    /// Go nil slice serializes as null (no omitempty), so None <-> null.
    pub uncore_caches: Option<Vec<Cache>>,
    pub socket_id: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cache {
    pub id: i64,
    /// Size in bytes.
    pub size: u64,
    /// data, instruction, or unified.
    #[serde(rename = "type")]
    pub cache_type: String,
    pub level: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HugePagesInfo {
    /// Huge page size in kB.
    pub page_size: u64,
    pub num_pages: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiskInfo {
    pub name: String,
    pub major: u64,
    pub minor: u64,
    /// Size in bytes.
    pub size: u64,
    /// One of "none", "noop", "cfq", "deadline".
    pub scheduler: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetInfo {
    pub name: String,
    pub mac_address: String,
    /// Speed in MBits/s.
    pub speed: i64,
    pub mtu: i64,
}

/// Upstream string enums; kept as plain strings on the wire.
pub type CloudProvider = String;
pub type InstanceType = String;
pub type InstanceID = String;

pub const UNKNOWN_PROVIDER: &str = "Unknown";
pub const UNKNOWN_INSTANCE: &str = "Unknown";
pub const UNNAMED_INSTANCE: &str = "None";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MachineInfo {
    pub timestamp: GoTime,
    #[serde(rename = "vendor_id")]
    pub cpu_vendor_id: String,
    pub num_cores: i64,
    pub num_physical_cores: i64,
    pub num_sockets: i64,
    #[serde(rename = "cpu_frequency_khz")]
    pub cpu_frequency: u64,
    pub memory_capacity: u64,
    pub swap_capacity: u64,
    pub memory_by_type: BTreeMap<String, MemoryInfo>,
    #[serde(rename = "nvm")]
    pub nvm_info: NVMInfo,
    #[serde(rename = "hugepages")]
    pub huge_pages: Vec<HugePagesInfo>,
    pub machine_id: String,
    pub system_uuid: String,
    pub boot_id: String,
    pub filesystems: Vec<FilesystemInfo>,
    pub disk_map: BTreeMap<String, DiskInfo>,
    pub network_devices: Vec<NetInfo>,
    pub topology: Vec<Node>,
    pub cloud_provider: CloudProvider,
    pub instance_type: InstanceType,
    pub instance_id: InstanceID,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryInfo {
    /// Bytes.
    pub capacity: u64,
    pub dimm_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NVMInfo {
    pub memory_mode_capacity: u64,
    /// The wire key contains a literal space — upstream typo, replicated for
    /// byte compatibility.
    #[serde(rename = "app direct_mode_capacity")]
    pub app_direct_mode_capacity: u64,
    pub avg_power_budget: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VersionInfo {
    pub kernel_version: String,
    pub container_os_version: String,
    pub docker_version: String,
    #[serde(rename = "docker_api_version")]
    pub docker_api_version: String,
    pub cadvisor_version: String,
    pub cadvisor_revision: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn nvm_replicates_upstream_space_typo() {
        let v = serde_json::to_value(NVMInfo { app_direct_mode_capacity: 9, ..Default::default() }).unwrap();
        assert_eq!(v["app direct_mode_capacity"], json!(9));
        assert!(v.get("app_direct_mode_capacity").is_none());
    }

    #[test]
    fn filesystem_info_skips_major_minor() {
        let fs = FilesystemInfo { device_major: 8, device_minor: 1, ..Default::default() };
        let v = serde_json::to_value(&fs).unwrap();
        assert!(v.get("device_major").is_none());
        assert_eq!(
            v.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["device", "capacity", "type", "inodes", "has_inodes"]
        );
        // skipped fields deserialize to default
        let back: FilesystemInfo = serde_json::from_value(v).unwrap();
        assert_eq!(back.device_major, 0);
    }

    #[test]
    fn machine_info_zero_value_keys() {
        let v = serde_json::to_value(MachineInfo::default()).unwrap();
        let keys: Vec<_> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "timestamp", "vendor_id", "num_cores", "num_physical_cores", "num_sockets",
                "cpu_frequency_khz", "memory_capacity", "swap_capacity", "memory_by_type",
                "nvm", "hugepages", "machine_id", "system_uuid", "boot_id", "filesystems",
                "disk_map", "network_devices", "topology", "cloud_provider", "instance_type",
                "instance_id",
            ]
        );
    }

    #[test]
    fn topology_wire_keys() {
        let node = Node {
            id: 0,
            cores: vec![Core { id: 1, threads: vec![0, 8], socket_id: 0, ..Default::default() }],
            ..Default::default()
        };
        let v = serde_json::to_value(&node).unwrap();
        assert!(v.get("node_id").is_some());
        assert_eq!(v["cores"][0]["core_id"], json!(1));
        assert_eq!(v["cores"][0]["thread_ids"], json!([0, 8]));
        // Go nil slice -> null (matches real v0.49.2 output on hosts without
        // uncore cache info).
        assert_eq!(v["cores"][0]["uncore_caches"], Value::Null);
    }
}
