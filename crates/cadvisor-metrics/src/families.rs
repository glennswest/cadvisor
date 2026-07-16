//! Metric family definitions: names, types, and HELP text captured verbatim
//! from cadvisor v0.49.2 output (`conformance/fixtures/metrics.prom`).

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Counter,
    Gauge,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Counter => "counter",
            Kind::Gauge => "gauge",
        }
    }
}

/// (name, kind, help) for every family this exporter can emit.
pub const FAMILIES: &[(&str, Kind, &str)] = &[
    ("cadvisor_version_info", Kind::Gauge, "A metric with a constant '1' value labeled by kernel version, OS version, docker version, cadvisor version & cadvisor revision."),
    ("container_blkio_device_usage_total", Kind::Counter, "Blkio Device bytes usage"),
    ("container_cpu_load_average_10s", Kind::Gauge, "Value of container cpu load average over the last 10 seconds."),
    ("container_cpu_system_seconds_total", Kind::Counter, "Cumulative system cpu time consumed in seconds."),
    ("container_cpu_usage_seconds_total", Kind::Counter, "Cumulative cpu time consumed in seconds."),
    ("container_cpu_user_seconds_total", Kind::Counter, "Cumulative user cpu time consumed in seconds."),
    ("container_fs_inodes_free", Kind::Gauge, "Number of available Inodes"),
    ("container_fs_inodes_total", Kind::Gauge, "Number of Inodes"),
    ("container_fs_io_current", Kind::Gauge, "Number of I/Os currently in progress"),
    ("container_fs_io_time_seconds_total", Kind::Counter, "Cumulative count of seconds spent doing I/Os"),
    ("container_fs_io_time_weighted_seconds_total", Kind::Counter, "Cumulative weighted I/O time in seconds"),
    ("container_fs_limit_bytes", Kind::Gauge, "Number of bytes that can be consumed by the container on this filesystem."),
    ("container_fs_read_seconds_total", Kind::Counter, "Cumulative count of seconds spent reading"),
    ("container_fs_reads_bytes_total", Kind::Counter, "Cumulative count of bytes read"),
    ("container_fs_reads_merged_total", Kind::Counter, "Cumulative count of reads merged"),
    ("container_fs_reads_total", Kind::Counter, "Cumulative count of reads completed"),
    ("container_fs_sector_reads_total", Kind::Counter, "Cumulative count of sector reads completed"),
    ("container_fs_sector_writes_total", Kind::Counter, "Cumulative count of sector writes completed"),
    ("container_fs_usage_bytes", Kind::Gauge, "Number of bytes that are consumed by the container on this filesystem."),
    ("container_fs_write_seconds_total", Kind::Counter, "Cumulative count of seconds spent writing"),
    ("container_fs_writes_bytes_total", Kind::Counter, "Cumulative count of bytes written"),
    ("container_fs_writes_merged_total", Kind::Counter, "Cumulative count of writes merged"),
    ("container_fs_writes_total", Kind::Counter, "Cumulative count of writes completed"),
    ("container_last_seen", Kind::Gauge, "Last time a container was seen by the exporter"),
    ("container_memory_cache", Kind::Gauge, "Number of bytes of page cache memory."),
    ("container_memory_failcnt", Kind::Counter, "Number of memory usage hits limits"),
    ("container_memory_failures_total", Kind::Counter, "Cumulative count of memory allocation failures."),
    ("container_memory_kernel_usage", Kind::Gauge, "Size of kernel memory allocated in bytes."),
    ("container_memory_mapped_file", Kind::Gauge, "Size of memory mapped files in bytes."),
    ("container_memory_max_usage_bytes", Kind::Gauge, "Maximum memory usage recorded in bytes"),
    ("container_memory_rss", Kind::Gauge, "Size of RSS in bytes."),
    ("container_memory_swap", Kind::Gauge, "Container swap usage in bytes."),
    ("container_memory_usage_bytes", Kind::Gauge, "Current memory usage in bytes, including all memory regardless of when it was accessed"),
    ("container_memory_working_set_bytes", Kind::Gauge, "Current working set in bytes."),
    ("container_network_receive_bytes_total", Kind::Counter, "Cumulative count of bytes received"),
    ("container_network_receive_errors_total", Kind::Counter, "Cumulative count of errors encountered while receiving"),
    ("container_network_receive_packets_dropped_total", Kind::Counter, "Cumulative count of packets dropped while receiving"),
    ("container_network_receive_packets_total", Kind::Counter, "Cumulative count of packets received"),
    ("container_network_transmit_bytes_total", Kind::Counter, "Cumulative count of bytes transmitted"),
    ("container_network_transmit_errors_total", Kind::Counter, "Cumulative count of errors encountered while transmitting"),
    ("container_network_transmit_packets_dropped_total", Kind::Counter, "Cumulative count of packets dropped while transmitting"),
    ("container_network_transmit_packets_total", Kind::Counter, "Cumulative count of packets transmitted"),
    ("container_oom_events_total", Kind::Counter, "Count of out of memory events observed for the container"),
    ("container_scrape_error", Kind::Gauge, "1 if there was an error while getting container metrics, 0 otherwise"),
    ("container_spec_cpu_period", Kind::Gauge, "CPU period of the container."),
    ("container_spec_cpu_shares", Kind::Gauge, "CPU share of the container."),
    ("container_spec_memory_limit_bytes", Kind::Gauge, "Memory limit for the container."),
    ("container_spec_memory_reservation_limit_bytes", Kind::Gauge, "Memory reservation limit for the container."),
    ("container_spec_memory_swap_limit_bytes", Kind::Gauge, "Memory swap limit for the container."),
    ("container_start_time_seconds", Kind::Gauge, "Start time of the container since unix epoch in seconds."),
    ("container_tasks_state", Kind::Gauge, "Number of tasks in given state"),
    ("machine_cpu_cores", Kind::Gauge, "Number of logical CPU cores."),
    ("machine_cpu_physical_cores", Kind::Gauge, "Number of physical CPU cores."),
    ("machine_cpu_sockets", Kind::Gauge, "Number of CPU sockets."),
    ("machine_memory_bytes", Kind::Gauge, "Amount of memory installed on the machine."),
    ("machine_nvm_avg_power_budget_watts", Kind::Gauge, "NVM power budget."),
    ("machine_nvm_capacity", Kind::Gauge, "NVM capacity value labeled by NVM mode (memory mode or app direct mode)."),
    ("machine_scrape_error", Kind::Gauge, "1 if there was an error while getting machine metrics, 0 otherwise."),
    ("machine_swap_bytes", Kind::Gauge, "Amount of swap memory available on the machine."),
];

pub fn family(name: &str) -> Option<&'static (&'static str, Kind, &'static str)> {
    FAMILIES.iter().find(|(n, _, _)| *n == name)
}
