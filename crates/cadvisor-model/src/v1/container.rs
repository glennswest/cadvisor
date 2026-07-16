//! Container spec/stats types mirroring upstream `lib/model/container.go`.
//!
//! Field declaration order matches the Go structs so serde_json emits keys in
//! the same order as encoding/json (which emits struct fields in declaration
//! order), keeping conformance diffs trivial.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::GoTime;
use crate::omit::is_zero;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuSpec {
    /// Requested cpu shares. Default is 1024.
    pub limit: u64,
    /// Requested cpu hard limit (milli-cpus). Default is unlimited (0).
    pub max_limit: u64,
    /// Cpu affinity mask.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mask: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub quota: u64,
    /// CPU reference period, in ns.
    #[serde(skip_serializing_if = "is_zero")]
    pub period: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemorySpec {
    /// Memory limit in bytes. Default is unlimited (-1).
    #[serde(skip_serializing_if = "is_zero")]
    pub limit: u64,
    /// Guaranteed memory in bytes. Default is 0.
    #[serde(skip_serializing_if = "is_zero")]
    pub reservation: u64,
    /// Swap limit in bytes. Default is unlimited (-1).
    #[serde(skip_serializing_if = "is_zero")]
    pub swap_limit: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcessSpec {
    #[serde(skip_serializing_if = "is_zero")]
    pub limit: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerSpec {
    // Go tag has omitempty, but time.Time is a struct so it always serializes.
    pub creation_time: GoTime,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub envs: BTreeMap<String, String>,
    pub has_cpu: bool,
    pub cpu: CpuSpec,
    pub has_memory: bool,
    pub memory: MemorySpec,
    pub has_hugetlb: bool,
    pub has_network: bool,
    pub has_processes: bool,
    pub processes: ProcessSpec,
    pub has_filesystem: bool,
    #[serde(rename = "has_diskio")]
    pub has_disk_io: bool,
    pub has_custom_metrics: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub custom_metrics: Vec<super::MetricSpec>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub image: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerReference {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// The absolute name of the container (unique on the machine).
    pub name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub namespace: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerInfoRequest {
    /// Max number of stats to return; -1 for all. Default: 60. Upstream
    /// decodes request bodies over the default value, which `serde(default)`
    /// reproduces per absent field.
    #[serde(skip_serializing_if = "is_zero")]
    pub num_stats: i64,
    pub start: GoTime,
    pub end: GoTime,
}

impl Default for ContainerInfoRequest {
    fn default() -> Self {
        ContainerInfoRequest {
            num_stats: 60,
            start: GoTime::zero(),
            end: GoTime::zero(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerInfo {
    #[serde(flatten)]
    pub reference: ContainerReference,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subcontainers: Vec<ContainerReference>,
    pub spec: ContainerSpec,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stats: Vec<ContainerStats>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoadStats {
    pub nr_sleeping: u64,
    pub nr_running: u64,
    pub nr_stopped: u64,
    pub nr_uninterruptible: u64,
    pub nr_io_wait: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuUsage {
    /// Total CPU usage in nanoseconds.
    pub total: u64,
    /// Per-core usage in nanoseconds.
    #[serde(rename = "per_cpu_usage", skip_serializing_if = "Vec::is_empty")]
    pub per_cpu: Vec<u64>,
    /// Time spent in user space, in nanoseconds.
    pub user: u64,
    /// Time spent in kernel space, in nanoseconds.
    pub system: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuCFS {
    pub periods: u64,
    pub throttled_periods: u64,
    /// Nanoseconds.
    pub throttled_time: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuSchedstat {
    pub run_time: u64,
    pub runqueue_time: u64,
    pub run_periods: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuStats {
    pub usage: CpuUsage,
    pub cfs: CpuCFS,
    pub schedstat: CpuSchedstat,
    /// Smoothed average of runnable threads x 1000 over 10s.
    pub load_average: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerDiskStats {
    pub device: String,
    pub major: u64,
    pub minor: u64,
    pub stats: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiskIoStats {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub io_service_bytes: Vec<PerDiskStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub io_serviced: Vec<PerDiskStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub io_queued: Vec<PerDiskStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sectors: Vec<PerDiskStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub io_service_time: Vec<PerDiskStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub io_wait_time: Vec<PerDiskStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub io_merged: Vec<PerDiskStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub io_time: Vec<PerDiskStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub io_cost_usage: Vec<PerDiskStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub io_cost_wait: Vec<PerDiskStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub io_cost_indebt: Vec<PerDiskStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub io_cost_indelay: Vec<PerDiskStats>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HugetlbStats {
    #[serde(skip_serializing_if = "is_zero")]
    pub usage: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub max_usage: u64,
    pub failcnt: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryStats {
    /// Current memory usage in bytes (`memory.current` on cgroup v2).
    pub usage: u64,
    /// Maximum recorded usage; no cgroup-v2 source, reported as 0.
    pub max_usage: u64,
    /// Page cache in bytes (`file` in memory.stat on v2).
    pub cache: u64,
    /// Anonymous + swap cache in bytes (`anon` on v2).
    pub rss: u64,
    /// Swap usage in bytes (memory.swap.current - memory.current on v2).
    pub swap: u64,
    /// Mapped files in bytes (`file_mapped` on v2).
    pub mapped_file: u64,
    /// usage - inactive_file, clamped at 0.
    pub working_set: u64,
    /// cgroup v1 only; 0 on v2.
    pub failcnt: u64,
    /// Kernel memory in bytes.
    #[serde(rename = "kernel")]
    pub kernel_usage: u64,
    pub container_data: MemoryStatsMemoryData,
    pub hierarchical_data: MemoryStatsMemoryData,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CPUSetStats {
    pub memory_migrate: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryNumaStats {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub file: BTreeMap<u8, u64>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub anon: BTreeMap<u8, u64>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub unevictable: BTreeMap<u8, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryStatsMemoryData {
    pub pgfault: u64,
    pub pgmajfault: u64,
    pub numa_stats: MemoryNumaStats,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceStats {
    pub name: String,
    pub rx_bytes: u64,
    pub rx_packets: u64,
    pub rx_errors: u64,
    pub rx_dropped: u64,
    pub tx_bytes: u64,
    pub tx_packets: u64,
    pub tx_errors: u64,
    pub tx_dropped: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkStats {
    /// Upstream embeds InterfaceStats (the default interface) inline.
    #[serde(flatten)]
    pub interface: InterfaceStats,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<InterfaceStats>,
    pub tcp: TcpStat,
    pub tcp6: TcpStat,
    pub udp: UdpStat,
    pub udp6: UdpStat,
    pub tcp_advanced: TcpAdvancedStat,
}

/// Untagged in Go, so JSON keys are the exported Go field names.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct TcpStat {
    pub established: u64,
    pub syn_sent: u64,
    pub syn_recv: u64,
    pub fin_wait1: u64,
    pub fin_wait2: u64,
    pub time_wait: u64,
    pub close: u64,
    pub close_wait: u64,
    pub last_ack: u64,
    pub listen: u64,
    pub closing: u64,
}

/// Untagged in Go, so JSON keys are the exported Go field names verbatim —
/// PascalCase conversion is NOT safe here (`TCPHPHits`, `PAWSActive`, ...),
/// hence per-field renames.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TcpAdvancedStat {
    #[serde(rename = "RtoAlgorithm")]
    pub rto_algorithm: u64,
    #[serde(rename = "RtoMin")]
    pub rto_min: u64,
    #[serde(rename = "RtoMax")]
    pub rto_max: u64,
    #[serde(rename = "MaxConn")]
    pub max_conn: i64,
    #[serde(rename = "ActiveOpens")]
    pub active_opens: u64,
    #[serde(rename = "PassiveOpens")]
    pub passive_opens: u64,
    #[serde(rename = "AttemptFails")]
    pub attempt_fails: u64,
    #[serde(rename = "EstabResets")]
    pub estab_resets: u64,
    #[serde(rename = "CurrEstab")]
    pub curr_estab: u64,
    #[serde(rename = "InSegs")]
    pub in_segs: u64,
    #[serde(rename = "OutSegs")]
    pub out_segs: u64,
    #[serde(rename = "RetransSegs")]
    pub retrans_segs: u64,
    #[serde(rename = "InErrs")]
    pub in_errs: u64,
    #[serde(rename = "OutRsts")]
    pub out_rsts: u64,
    #[serde(rename = "InCsumErrors")]
    pub in_csum_errors: u64,
    #[serde(rename = "EmbryonicRsts")]
    pub embryonic_rsts: u64,
    #[serde(rename = "SyncookiesSent")]
    pub syncookies_sent: u64,
    #[serde(rename = "SyncookiesRecv")]
    pub syncookies_recv: u64,
    #[serde(rename = "SyncookiesFailed")]
    pub syncookies_failed: u64,
    #[serde(rename = "PruneCalled")]
    pub prune_called: u64,
    #[serde(rename = "RcvPruned")]
    pub rcv_pruned: u64,
    #[serde(rename = "OfoPruned")]
    pub ofo_pruned: u64,
    #[serde(rename = "OutOfWindowIcmps")]
    pub out_of_window_icmps: u64,
    #[serde(rename = "LockDroppedIcmps")]
    pub lock_dropped_icmps: u64,
    #[serde(rename = "TW")]
    pub tw: u64,
    #[serde(rename = "TWRecycled")]
    pub tw_recycled: u64,
    #[serde(rename = "TWKilled")]
    pub tw_killed: u64,
    #[serde(rename = "TCPTimeWaitOverflow")]
    pub tcp_time_wait_overflow: u64,
    #[serde(rename = "TCPTimeouts")]
    pub tcp_timeouts: u64,
    #[serde(rename = "TCPSpuriousRTOs")]
    pub tcp_spurious_rtos: u64,
    #[serde(rename = "TCPLossProbes")]
    pub tcp_loss_probes: u64,
    #[serde(rename = "TCPLossProbeRecovery")]
    pub tcp_loss_probe_recovery: u64,
    #[serde(rename = "TCPRenoRecoveryFail")]
    pub tcp_reno_recovery_fail: u64,
    #[serde(rename = "TCPSackRecoveryFail")]
    pub tcp_sack_recovery_fail: u64,
    #[serde(rename = "TCPRenoFailures")]
    pub tcp_reno_failures: u64,
    #[serde(rename = "TCPSackFailures")]
    pub tcp_sack_failures: u64,
    #[serde(rename = "TCPLossFailures")]
    pub tcp_loss_failures: u64,
    #[serde(rename = "DelayedACKs")]
    pub delayed_acks: u64,
    #[serde(rename = "DelayedACKLocked")]
    pub delayed_ack_locked: u64,
    #[serde(rename = "DelayedACKLost")]
    pub delayed_ack_lost: u64,
    #[serde(rename = "ListenOverflows")]
    pub listen_overflows: u64,
    #[serde(rename = "ListenDrops")]
    pub listen_drops: u64,
    #[serde(rename = "TCPHPHits")]
    pub tcp_hp_hits: u64,
    #[serde(rename = "TCPPureAcks")]
    pub tcp_pure_acks: u64,
    #[serde(rename = "TCPHPAcks")]
    pub tcp_hp_acks: u64,
    #[serde(rename = "TCPRenoRecovery")]
    pub tcp_reno_recovery: u64,
    #[serde(rename = "TCPSackRecovery")]
    pub tcp_sack_recovery: u64,
    #[serde(rename = "TCPSACKReneging")]
    pub tcp_sack_reneging: u64,
    #[serde(rename = "TCPFACKReorder")]
    pub tcp_fack_reorder: u64,
    #[serde(rename = "TCPSACKReorder")]
    pub tcp_sack_reorder: u64,
    #[serde(rename = "TCPRenoReorder")]
    pub tcp_reno_reorder: u64,
    #[serde(rename = "TCPTSReorder")]
    pub tcp_ts_reorder: u64,
    #[serde(rename = "TCPFullUndo")]
    pub tcp_full_undo: u64,
    #[serde(rename = "TCPPartialUndo")]
    pub tcp_partial_undo: u64,
    #[serde(rename = "TCPDSACKUndo")]
    pub tcp_dsack_undo: u64,
    #[serde(rename = "TCPLossUndo")]
    pub tcp_loss_undo: u64,
    #[serde(rename = "TCPFastRetrans")]
    pub tcp_fast_retrans: u64,
    #[serde(rename = "TCPSlowStartRetrans")]
    pub tcp_slow_start_retrans: u64,
    #[serde(rename = "TCPLostRetransmit")]
    pub tcp_lost_retransmit: u64,
    #[serde(rename = "TCPRetransFail")]
    pub tcp_retrans_fail: u64,
    #[serde(rename = "TCPRcvCollapsed")]
    pub tcp_rcv_collapsed: u64,
    #[serde(rename = "TCPDSACKOldSent")]
    pub tcp_dsack_old_sent: u64,
    #[serde(rename = "TCPDSACKOfoSent")]
    pub tcp_dsack_ofo_sent: u64,
    #[serde(rename = "TCPDSACKRecv")]
    pub tcp_dsack_recv: u64,
    #[serde(rename = "TCPDSACKOfoRecv")]
    pub tcp_dsack_ofo_recv: u64,
    #[serde(rename = "TCPAbortOnData")]
    pub tcp_abort_on_data: u64,
    #[serde(rename = "TCPAbortOnClose")]
    pub tcp_abort_on_close: u64,
    #[serde(rename = "TCPAbortOnMemory")]
    pub tcp_abort_on_memory: u64,
    #[serde(rename = "TCPAbortOnTimeout")]
    pub tcp_abort_on_timeout: u64,
    #[serde(rename = "TCPAbortOnLinger")]
    pub tcp_abort_on_linger: u64,
    #[serde(rename = "TCPAbortFailed")]
    pub tcp_abort_failed: u64,
    #[serde(rename = "TCPMemoryPressures")]
    pub tcp_memory_pressures: u64,
    #[serde(rename = "TCPMemoryPressuresChrono")]
    pub tcp_memory_pressures_chrono: u64,
    #[serde(rename = "TCPSACKDiscard")]
    pub tcp_sack_discard: u64,
    #[serde(rename = "TCPDSACKIgnoredOld")]
    pub tcp_dsack_ignored_old: u64,
    #[serde(rename = "TCPDSACKIgnoredNoUndo")]
    pub tcp_dsack_ignored_no_undo: u64,
    #[serde(rename = "TCPMD5NotFound")]
    pub tcp_md5_not_found: u64,
    #[serde(rename = "TCPMD5Unexpected")]
    pub tcp_md5_unexpected: u64,
    #[serde(rename = "TCPMD5Failure")]
    pub tcp_md5_failure: u64,
    #[serde(rename = "TCPSackShifted")]
    pub tcp_sack_shifted: u64,
    #[serde(rename = "TCPSackMerged")]
    pub tcp_sack_merged: u64,
    #[serde(rename = "TCPSackShiftFallback")]
    pub tcp_sack_shift_fallback: u64,
    #[serde(rename = "TCPBacklogDrop")]
    pub tcp_backlog_drop: u64,
    #[serde(rename = "PFMemallocDrop")]
    pub pf_memalloc_drop: u64,
    #[serde(rename = "TCPMinTTLDrop")]
    pub tcp_min_ttl_drop: u64,
    #[serde(rename = "TCPDeferAcceptDrop")]
    pub tcp_defer_accept_drop: u64,
    #[serde(rename = "IPReversePathFilter")]
    pub ip_reverse_path_filter: u64,
    #[serde(rename = "TCPReqQFullDoCookies")]
    pub tcp_req_q_full_do_cookies: u64,
    #[serde(rename = "TCPReqQFullDrop")]
    pub tcp_req_q_full_drop: u64,
    #[serde(rename = "TCPFastOpenActive")]
    pub tcp_fast_open_active: u64,
    #[serde(rename = "TCPFastOpenActiveFail")]
    pub tcp_fast_open_active_fail: u64,
    #[serde(rename = "TCPFastOpenPassive")]
    pub tcp_fast_open_passive: u64,
    #[serde(rename = "TCPFastOpenPassiveFail")]
    pub tcp_fast_open_passive_fail: u64,
    #[serde(rename = "TCPFastOpenListenOverflow")]
    pub tcp_fast_open_listen_overflow: u64,
    #[serde(rename = "TCPFastOpenCookieReqd")]
    pub tcp_fast_open_cookie_reqd: u64,
    #[serde(rename = "TCPSynRetrans")]
    pub tcp_syn_retrans: u64,
    #[serde(rename = "TCPOrigDataSent")]
    pub tcp_orig_data_sent: u64,
    #[serde(rename = "PAWSActive")]
    pub paws_active: u64,
    #[serde(rename = "PAWSEstab")]
    pub paws_estab: u64,
}

/// Untagged in Go, so JSON keys are the exported Go field names.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct UdpStat {
    pub listen: u64,
    pub dropped: u64,
    pub rx_queued: u64,
    pub tx_queued: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FsStats {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub device: String,
    #[serde(rename = "type")]
    pub fs_type: String,
    /// Go field is `Limit`; the wire key is `capacity` upstream.
    #[serde(rename = "capacity")]
    pub limit: u64,
    pub usage: u64,
    pub base_usage: u64,
    pub available: u64,
    pub has_inodes: bool,
    pub inodes: u64,
    pub inodes_free: u64,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AcceleratorStats {
    pub make: String,
    pub model: String,
    pub id: String,
    pub memory_total: u64,
    pub memory_used: u64,
    pub duty_cycle: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerfValue {
    pub scaling_ratio: f64,
    pub value: u64,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerfStat {
    #[serde(flatten)]
    pub perf_value: PerfValue,
    pub cpu: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryBandwidthStats {
    #[serde(rename = "mbm_total_bytes", skip_serializing_if = "is_zero")]
    pub total_bytes: u64,
    #[serde(rename = "mbm_local_bytes", skip_serializing_if = "is_zero")]
    pub local_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheStats {
    #[serde(rename = "llc_occupancy", skip_serializing_if = "is_zero")]
    pub llc_occupancy: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResctrlStats {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub memory_bandwidth: Vec<MemoryBandwidthStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cache: Vec<CacheStats>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerfUncoreStat {
    #[serde(flatten)]
    pub perf_value: PerfValue,
    pub socket: i64,
    pub pmu: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UlimitSpec {
    pub name: String,
    pub soft_limit: i64,
    pub hard_limit: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcessStats {
    pub process_count: u64,
    pub fd_count: u64,
    pub socket_count: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub threads_current: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub threads_max: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ulimits: Vec<UlimitSpec>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerStats {
    pub timestamp: GoTime,
    // Cpu/DiskIo/Memory/Network/Processes are non-pointer structs in v0.49.2:
    // the omitempty tags are no-ops and they ALWAYS serialize.
    pub cpu: CpuStats,
    #[serde(rename = "diskio")]
    pub disk_io: DiskIoStats,
    pub memory: MemoryStats,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub hugetlb: BTreeMap<String, HugetlbStats>,
    pub network: NetworkStats,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub filesystem: Vec<FsStats>,
    pub task_stats: LoadStats,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub accelerators: Vec<AcceleratorStats>,
    pub processes: ProcessStats,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_metrics: BTreeMap<String, Vec<super::MetricVal>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub perf_stats: Vec<PerfStat>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub perf_uncore_stats: Vec<PerfUncoreStat>,
    #[serde(skip_serializing_if = "is_zero")]
    pub referenced_memory: u64,
    pub resctrl: ResctrlStats,
    #[serde(rename = "cpuset")]
    pub cpu_set: CPUSetStats,
    #[serde(skip_serializing_if = "is_zero")]
    pub oom_events: u64,
}

/// Instantaneous (rate) CPU usage, nanocores per second.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuInstStats {
    pub usage: CpuInstUsage,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuInstUsage {
    pub total: u64,
    #[serde(rename = "per_cpu_usage", skip_serializing_if = "Vec::is_empty")]
    pub per_cpu: Vec<u64>,
    pub user: u64,
    pub system: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn keys(v: &Value) -> Vec<&str> {
        v.as_object().unwrap().keys().map(String::as_str).collect()
    }

    #[test]
    fn spec_zero_value_serializes_all_non_omitempty_keys() {
        let v = serde_json::to_value(ContainerSpec::default()).unwrap();
        // Go structs with omitempty still serialize (omitempty is a no-op on
        // structs): creation_time, start_time, cpu, memory, processes.
        assert_eq!(
            keys(&v),
            vec![
                "creation_time",
                "has_cpu",
                "cpu",
                "has_memory",
                "memory",
                "has_hugetlb",
                "has_network",
                "has_processes",
                "processes",
                "has_filesystem",
                "has_diskio",
                "has_custom_metrics",
            ]
        );
        assert_eq!(v["creation_time"], json!("0001-01-01T00:00:00Z"));
        assert_eq!(v["has_cpu"], json!(false));
        // MemorySpec is all-omitempty scalars -> empty object.
        assert_eq!(v["memory"], json!({}));
        // CpuSpec limit/max_limit are not omitempty.
        assert_eq!(v["cpu"], json!({"limit": 0, "max_limit": 0}));
    }

    #[test]
    fn stats_zero_value_shape() {
        let v = serde_json::to_value(ContainerStats::default()).unwrap();
        // Slice/map fields are omitted; struct fields (non-pointer in Go
        // v0.49.2) always stay.
        assert_eq!(
            keys(&v),
            vec![
                "timestamp", "cpu", "diskio", "memory", "network", "task_stats",
                "processes", "resctrl", "cpuset",
            ]
        );
    }

    #[test]
    fn memory_stats_wire_keys() {
        let v = serde_json::to_value(MemoryStats::default()).unwrap();
        assert!(v.get("kernel").is_some(), "KernelUsage serializes as 'kernel'");
        // Struct-typed omitempty fields always present.
        for key in ["container_data", "hierarchical_data"] {
            assert!(v.get(key).is_some(), "{key} must always serialize");
        }
        assert_eq!(v["container_data"], json!({"pgfault": 0, "pgmajfault": 0, "numa_stats": {}}));
    }

    #[test]
    fn fs_stats_limit_serializes_as_capacity() {
        let v = serde_json::to_value(FsStats { limit: 42, ..Default::default() }).unwrap();
        assert_eq!(v["capacity"], json!(42));
        assert!(v.get("limit").is_none());
        assert!(v.get("device").is_none(), "empty device is omitted");
        assert_eq!(v["type"], json!(""));
    }

    #[test]
    fn tcp_udp_stats_use_go_field_names() {
        let v = serde_json::to_value(TcpStat { syn_sent: 1, fin_wait1: 2, ..Default::default() }).unwrap();
        assert_eq!(v["SynSent"], json!(1));
        assert_eq!(v["FinWait1"], json!(2));
        assert_eq!(
            keys(&v),
            vec!["Established", "SynSent", "SynRecv", "FinWait1", "FinWait2", "TimeWait",
                 "Close", "CloseWait", "LastAck", "Listen", "Closing"]
        );
        let v = serde_json::to_value(UdpStat { rx_queued: 3, ..Default::default() }).unwrap();
        assert_eq!(v["RxQueued"], json!(3));

        let v = serde_json::to_value(TcpAdvancedStat::default()).unwrap();
        for key in ["RtoAlgorithm", "MaxConn", "TW", "TCPHPHits", "TCPPureAcks",
                    "DelayedACKs", "TCPSACKReneging", "TCPDSACKIgnoredNoUndo",
                    "TCPMD5NotFound", "PFMemallocDrop", "IPReversePathFilter",
                    "TCPReqQFullDoCookies", "PAWSActive", "PAWSEstab"] {
            assert!(v.get(key).is_some(), "missing TcpAdvancedStat key {key}");
        }
        assert_eq!(v.as_object().unwrap().len(), 99);
    }

    #[test]
    fn network_stats_inlines_default_interface() {
        let net = NetworkStats {
            interface: InterfaceStats { name: "eth0".into(), rx_bytes: 7, ..Default::default() },
            ..Default::default()
        };
        let v = serde_json::to_value(net).unwrap();
        assert_eq!(v["name"], json!("eth0"));
        assert_eq!(v["rx_bytes"], json!(7));
        assert!(v.get("interfaces").is_none());
        for key in ["tcp", "tcp6", "udp", "udp6", "tcp_advanced"] {
            assert!(v.get(key).is_some(), "{key} must always serialize");
        }
    }

    #[test]
    fn container_info_flattens_reference() {
        let info = ContainerInfo {
            reference: ContainerReference {
                id: "abc".into(),
                name: "/docker/abc".into(),
                aliases: vec!["web".into()],
                namespace: "docker".into(),
            },
            ..Default::default()
        };
        let v = serde_json::to_value(info).unwrap();
        assert_eq!(v["id"], json!("abc"));
        assert_eq!(v["name"], json!("/docker/abc"));
        assert_eq!(v["namespace"], json!("docker"));
        assert!(v.get("spec").is_some());
        assert!(v.get("stats").is_none());
    }

    #[test]
    fn request_decodes_over_defaults() {
        // Empty body fields keep the default NumStats=60 (upstream decodes
        // the POST body over DefaultContainerInfoRequest()).
        let r: ContainerInfoRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(r.num_stats, 60);
        assert!(r.start.is_zero() && r.end.is_zero());

        let r: ContainerInfoRequest =
            serde_json::from_str(r#"{"num_stats": -1, "start": "2026-07-16T00:00:00Z"}"#).unwrap();
        assert_eq!(r.num_stats, -1);
        assert!(!r.start.is_zero());
    }

    #[test]
    fn perf_stat_flattens_value() {
        let p = PerfStat {
            perf_value: PerfValue { scaling_ratio: 1.0, value: 5, name: "cycles".into() },
            cpu: 2,
        };
        let v = serde_json::to_value(p).unwrap();
        assert_eq!(keys(&v), vec!["scaling_ratio", "value", "name", "cpu"]);
    }

    #[test]
    fn round_trip_preserves_value() {
        let mut stats = ContainerStats::default();
        stats.cpu = CpuStats {
            usage: CpuUsage { total: 123, per_cpu: vec![100, 23], user: 60, system: 63 },
            ..Default::default()
        };
        stats.hugetlb.insert("2Mi".into(), HugetlbStats { usage: 1, max_usage: 0, failcnt: 0 });
        let text = serde_json::to_string(&stats).unwrap();
        let back: ContainerStats = serde_json::from_str(&text).unwrap();
        assert_eq!(stats, back);
    }
}
