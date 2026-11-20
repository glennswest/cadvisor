//! Walks the manager's containers and renders the metric families.

use std::collections::BTreeMap;
use std::sync::Arc;

use cadvisor_manager::Manager;
use cadvisor_model::GoTime;
use cadvisor_model::v1;

use crate::encode::sample_line;
use crate::families::FAMILIES;

/// Options mirroring the upstream label/metric flags.
#[derive(Debug, Clone)]
pub struct MetricsOpts {
    pub store_container_labels: bool,
    pub whitelisted_container_labels: Vec<String>,
    /// Metric group tokens disabled via -disable_metrics (or everything not in
    /// -enable_metrics when that is non-empty).
    pub disabled_groups: std::collections::BTreeSet<String>,
}

impl Default for MetricsOpts {
    fn default() -> Self {
        MetricsOpts {
            store_container_labels: true,
            whitelisted_container_labels: Vec::new(),
            disabled_groups: Default::default(),
        }
    }
}

fn sanitize_label_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

/// Values above 2^62 are treated as "unlimited" and reported as 0.
fn clamp_unlimited(v: u64) -> f64 {
    if v > (1 << 62) { 0.0 } else { v as f64 }
}

type Series = (Vec<(String, String)>, f64, Option<i64>);
type Gathered = (&'static (&'static str, crate::families::Kind, &'static str), Vec<Series>);

#[derive(Default)]
struct Output {
    families: BTreeMap<&'static str, Vec<Series>>,
}

impl Output {
    fn push(&mut self, family: &'static str, labels: Vec<(String, String)>, value: f64, ts: Option<i64>) {
        self.families.entry(family).or_default().push((labels, value, ts));
    }

    /// Families in client_golang's order (by name), each family's series
    /// sorted by label values, a repeated label set served once.
    fn gathered(mut self) -> Vec<Gathered> {
        let mut out = Vec::new();
        for fam in FAMILIES {
            let Some(mut series) = self.families.remove(fam.0) else { continue };
            // client_golang orders series by label values.
            series.sort_by(|a, b| a.0.cmp(&b.0));
            // …and refuses a series "collected before with the same name and
            // label values", serving the first (upstream's ContinueOnError).
            // Two io.stat devices missing from the disk map both get
            // device="" (#14). The sort is stable, so the first pushed stays.
            series.dedup_by(|later, first| later.0 == first.0);
            out.push((fam, series));
        }
        out
    }

    /// Delimited protobuf `MetricFamily`s (#40).
    fn render_proto(self, buf: &mut Vec<u8>) {
        for ((name, kind, help), series) in self.gathered() {
            crate::proto::family(
                buf,
                name,
                help,
                *kind,
                series.iter().map(|(labels, value, ts)| {
                    (labels.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect(), *value, *ts)
                }),
            );
        }
    }

    fn render(self, buf: &mut String) {
        for ((name, kind, help), series) in self.gathered() {
            buf.push_str("# HELP ");
            buf.push_str(name);
            buf.push(' ');
            buf.push_str(help);
            buf.push_str("\n# TYPE ");
            buf.push_str(name);
            buf.push(' ');
            buf.push_str(kind.as_str());
            buf.push('\n');
            for (labels, value, ts) in series {
                let label_refs: Vec<(&str, &str)> =
                    labels.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                sample_line(buf, name, &label_refs, value, ts);
            }
        }
    }
}

struct ContainerLabels {
    base: Vec<(String, String)>,
}

impl ContainerLabels {
    /// This container's own label set (before the cross-container union).
    fn own(
        handle: &cadvisor_manager::manager::ContainerHandle,
        opts: &MetricsOpts,
    ) -> std::collections::BTreeMap<String, String> {
        let spec = handle.spec.read().unwrap();
        let reference = handle.reference.read().unwrap();
        Self::own_from(handle.name(), &spec, &reference, opts)
    }

    fn own_from(
        name: &str,
        spec: &v1::ContainerSpec,
        reference: &v1::ContainerReference,
        opts: &MetricsOpts,
    ) -> std::collections::BTreeMap<String, String> {
        let mut own = std::collections::BTreeMap::new();
        own.insert("id".to_string(), name.to_string());
        if let Some(alias) = reference.aliases.first() {
            own.insert("name".to_string(), alias.clone());
        }
        if !spec.image.is_empty() {
            own.insert("image".to_string(), spec.image.clone());
        }
        for (k, v) in &spec.labels {
            let allowed = opts.store_container_labels
                || opts.whitelisted_container_labels.iter().any(|w| w == k);
            if allowed {
                own.insert(format!("container_label_{}", sanitize_label_name(k)), v.clone());
            }
        }
        // Upstream's whitelist-only label set (store_container_labels=false)
        // carries no env labels.
        if opts.store_container_labels {
            for (k, v) in &spec.envs {
                own.insert(format!("container_env_{}", sanitize_label_name(k)), v.clone());
            }
        }
        own
    }

    /// Upstream emits the UNION of label keys across all containers, with ""
    /// for keys a container doesn't have (client_golang requires consistent
    /// label dimensions per metric family).
    fn unioned(
        own: std::collections::BTreeMap<String, String>,
        union: &std::collections::BTreeSet<String>,
    ) -> Self {
        let base = union
            .iter()
            .map(|k| (k.clone(), own.get(k).cloned().unwrap_or_default()))
            .collect();
        ContainerLabels { base }
    }

    fn with(&self, extra: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut labels = self.base.clone();
        for (k, v) in extra {
            labels.push((k.to_string(), v.to_string()));
        }
        labels.sort_by(|a, b| a.0.cmp(&b.0));
        labels
    }
}

/// The text exposition (0.0.4).
pub fn render(manager: &Manager, opts: &MetricsOpts, buf: &mut String) {
    gather(manager, opts).render(buf);
}

/// The delimited protobuf exposition (#40).
pub fn render_proto(manager: &Manager, opts: &MetricsOpts, buf: &mut Vec<u8>) {
    gather(manager, opts).render_proto(buf);
}

fn gather(manager: &Manager, opts: &MetricsOpts) -> Output {
    let mut out = Output::default();
    let now = GoTime::now();
    let now_ms = now.0.timestamp_millis();
    let enabled = |group: &str| !opts.disabled_groups.contains(group);

    let vi = manager.version_info();
    out.push(
        "cadvisor_version_info",
        vec![
            ("cadvisorRevision".into(), vi.cadvisor_revision.clone()),
            ("cadvisorVersion".into(), vi.cadvisor_version.clone()),
            ("dockerVersion".into(), vi.docker_version.clone()),
            ("kernelVersion".into(), vi.kernel_version.clone()),
            ("osVersion".into(), vi.container_os_version.clone()),
        ],
        1.0,
        None,
    );

    let handles = manager.handles();
    let own_labels: Vec<std::collections::BTreeMap<String, String>> =
        handles.iter().map(|h| ContainerLabels::own(h, opts)).collect();
    let union: std::collections::BTreeSet<String> =
        own_labels.iter().flat_map(|m| m.keys().cloned()).collect();

    for (handle, own) in handles.iter().zip(own_labels) {
        let labels = ContainerLabels::unioned(own, &union);
        let spec = handle.spec.read().unwrap().clone();
        let latest = handle.store.lock().unwrap().latest();

        // Spec metrics: no timestamps.
        if enabled("cpu") && spec.has_cpu {
            out.push("container_spec_cpu_period", labels.with(&[]), spec.cpu.period as f64, None);
            out.push("container_spec_cpu_shares", labels.with(&[]), spec.cpu.limit as f64, None);
            if spec.cpu.quota != 0 {
                out.push("container_spec_cpu_quota", labels.with(&[]), spec.cpu.quota as f64, None);
            }
        }
        if enabled("memory") && spec.has_memory {
            out.push(
                "container_spec_memory_limit_bytes",
                labels.with(&[]),
                clamp_unlimited(spec.memory.limit),
                None,
            );
            out.push(
                "container_spec_memory_swap_limit_bytes",
                labels.with(&[]),
                clamp_unlimited(spec.memory.swap_limit),
                None,
            );
            out.push(
                "container_spec_memory_reservation_limit_bytes",
                labels.with(&[]),
                clamp_unlimited(spec.memory.reservation),
                None,
            );
        }
        out.push(
            "container_start_time_seconds",
            labels.with(&[]),
            spec.creation_time.0.timestamp() as f64
                + spec.creation_time.0.timestamp_subsec_nanos() as f64 / 1e9,
            None,
        );
        out.push("container_last_seen", labels.with(&[]), now_ms as f64 / 1000.0, Some(now_ms));

        let Some(s) = latest else { continue };
        let ts = Some(s.timestamp.0.timestamp_millis());

        if enabled("cpu") {
            out.push(
                "container_cpu_user_seconds_total",
                labels.with(&[]),
                s.cpu.usage.user as f64 / 1e9,
                ts,
            );
            out.push(
                "container_cpu_system_seconds_total",
                labels.with(&[]),
                s.cpu.usage.system as f64 / 1e9,
                ts,
            );
            if !enabled("percpu") {
                // Without percpu upstream always emits the total, even at 0.
                out.push(
                    "container_cpu_usage_seconds_total",
                    labels.with(&[("cpu", "total")]),
                    s.cpu.usage.total as f64 / 1e9,
                    ts,
                );
            } else if s.cpu.usage.per_cpu.is_empty() {
                // cgroup v2 has no per-CPU usage: the total, and (as
                // upstream) only once it is above 0.
                if s.cpu.usage.total > 0 {
                    out.push(
                        "container_cpu_usage_seconds_total",
                        labels.with(&[("cpu", "total")]),
                        s.cpu.usage.total as f64 / 1e9,
                        ts,
                    );
                }
            } else {
                for (i, v) in s.cpu.usage.per_cpu.iter().enumerate() {
                    if *v > 0 {
                        out.push(
                            "container_cpu_usage_seconds_total",
                            labels.with(&[("cpu", &format!("cpu{i:02}"))]),
                            *v as f64 / 1e9,
                            ts,
                        );
                    }
                }
            }
            if spec.cpu.quota != 0 {
                out.push("container_cpu_cfs_periods_total", labels.with(&[]), s.cpu.cfs.periods as f64, ts);
                out.push(
                    "container_cpu_cfs_throttled_periods_total",
                    labels.with(&[]),
                    s.cpu.cfs.throttled_periods as f64,
                    ts,
                );
                out.push(
                    "container_cpu_cfs_throttled_seconds_total",
                    labels.with(&[]),
                    s.cpu.cfs.throttled_time as f64 / 1e9,
                    ts,
                );
            }
        }

        if enabled("sched") {
            let sc = &s.cpu.schedstat;
            out.push("container_cpu_schedstat_run_seconds_total", labels.with(&[]), sc.run_time as f64 / 1e9, ts);
            out.push(
                "container_cpu_schedstat_runqueue_seconds_total",
                labels.with(&[]),
                sc.runqueue_time as f64 / 1e9,
                ts,
            );
            out.push("container_cpu_schedstat_run_periods_total", labels.with(&[]), sc.run_periods as f64, ts);
        }

        if enabled("hugetlb") {
            for (size, h) in &s.hugetlb {
                let l: &[(&str, &str)] = &[("pagesize", size)];
                out.push("container_hugetlb_failcnt", labels.with(l), h.failcnt as f64, ts);
                out.push("container_hugetlb_usage_bytes", labels.with(l), h.usage as f64, ts);
                out.push("container_hugetlb_max_usage_bytes", labels.with(l), h.max_usage as f64, ts);
            }
        }

        if enabled("cpuLoad") {
            out.push(
                "container_cpu_load_average_10s",
                labels.with(&[]),
                s.cpu.load_average as f64 / 1000.0,
                ts,
            );
            let states: [(&str, u64); 5] = [
                ("sleeping", s.task_stats.nr_sleeping),
                ("running", s.task_stats.nr_running),
                ("stopped", s.task_stats.nr_stopped),
                ("uninterruptible", s.task_stats.nr_uninterruptible),
                ("iowaiting", s.task_stats.nr_io_wait),
            ];
            for (state, v) in states {
                out.push("container_tasks_state", labels.with(&[("state", state)]), v as f64, ts);
            }
        }

        if enabled("memory") {
            let m = &s.memory;
            out.push("container_memory_cache", labels.with(&[]), m.cache as f64, ts);
            out.push("container_memory_rss", labels.with(&[]), m.rss as f64, ts);
            out.push("container_memory_kernel_usage", labels.with(&[]), m.kernel_usage as f64, ts);
            out.push("container_memory_mapped_file", labels.with(&[]), m.mapped_file as f64, ts);
            out.push("container_memory_swap", labels.with(&[]), m.swap as f64, ts);
            out.push("container_memory_failcnt", labels.with(&[]), m.failcnt as f64, ts);
            out.push("container_memory_usage_bytes", labels.with(&[]), m.usage as f64, ts);
            out.push("container_memory_max_usage_bytes", labels.with(&[]), m.max_usage as f64, ts);
            out.push("container_memory_working_set_bytes", labels.with(&[]), m.working_set as f64, ts);
            let failures: [(&str, &str, u64); 4] = [
                ("pgfault", "container", m.container_data.pgfault),
                ("pgmajfault", "container", m.container_data.pgmajfault),
                ("pgfault", "hierarchy", m.hierarchical_data.pgfault),
                ("pgmajfault", "hierarchy", m.hierarchical_data.pgmajfault),
            ];
            for (ft, scope, v) in failures {
                out.push(
                    "container_memory_failures_total",
                    labels.with(&[("failure_type", ft), ("scope", scope)]),
                    v as f64,
                    ts,
                );
            }
        }

        if enabled("disk") {
            for fs in &s.filesystem {
                let dev: &[(&str, &str)] = &[("device", &fs.device)];
                out.push("container_fs_inodes_free", labels.with(dev), fs.inodes_free as f64, ts);
                out.push("container_fs_inodes_total", labels.with(dev), fs.inodes as f64, ts);
                out.push("container_fs_limit_bytes", labels.with(dev), fs.limit as f64, ts);
                out.push("container_fs_usage_bytes", labels.with(dev), fs.usage as f64, ts);
            }
        }

        if enabled("diskIO") {
            // Byte counters come from cgroup io.stat (io_service_bytes).
            let disk_map = manager.machine_info().disk_map;
            let resolve = |major: u64, minor: u64| -> String {
                disk_map
                    .get(&format!("{major}:{minor}"))
                    .map(|d| format!("/dev/{}", d.name))
                    .unwrap_or_default()
            };
            for entry in &s.disk_io.io_service_bytes {
                let device = if entry.device.is_empty() {
                    resolve(entry.major, entry.minor)
                } else {
                    entry.device.clone()
                };
                let major = entry.major.to_string();
                let minor = entry.minor.to_string();
                for (op, v) in &entry.stats {
                    out.push(
                        "container_blkio_device_usage_total",
                        labels.with(&[
                            ("device", &device),
                            ("major", &major),
                            ("minor", &minor),
                            ("operation", op),
                        ]),
                        *v as f64,
                        ts,
                    );
                }
                if let Some(v) = entry.stats.get("Read") {
                    out.push(
                        "container_fs_reads_bytes_total",
                        labels.with(&[("device", &device)]),
                        *v as f64,
                        ts,
                    );
                }
                if let Some(v) = entry.stats.get("Write") {
                    out.push(
                        "container_fs_writes_bytes_total",
                        labels.with(&[("device", &device)]),
                        *v as f64,
                        ts,
                    );
                }
            }
            for entry in &s.disk_io.io_serviced {
                let device = if entry.device.is_empty() {
                    resolve(entry.major, entry.minor)
                } else {
                    entry.device.clone()
                };
                if let Some(v) = entry.stats.get("Read") {
                    out.push(
                        "container_fs_reads_total",
                        labels.with(&[("device", &device)]),
                        *v as f64,
                        ts,
                    );
                }
                if let Some(v) = entry.stats.get("Write") {
                    out.push(
                        "container_fs_writes_total",
                        labels.with(&[("device", &device)]),
                        *v as f64,
                        ts,
                    );
                }
            }
            // Per-filesystem counters (root container's global partitions).
            for fs in &s.filesystem {
                let dev: &[(&str, &str)] = &[("device", &fs.device)];
                out.push("container_fs_reads_total", labels.with(dev), fs.reads_completed as f64, ts);
                out.push("container_fs_reads_merged_total", labels.with(dev), fs.reads_merged as f64, ts);
                out.push("container_fs_sector_reads_total", labels.with(dev), fs.sectors_read as f64, ts);
                out.push("container_fs_read_seconds_total", labels.with(dev), fs.read_time as f64 / 1000.0, ts);
                out.push("container_fs_writes_total", labels.with(dev), fs.writes_completed as f64, ts);
                out.push("container_fs_writes_merged_total", labels.with(dev), fs.writes_merged as f64, ts);
                out.push("container_fs_sector_writes_total", labels.with(dev), fs.sectors_written as f64, ts);
                out.push("container_fs_write_seconds_total", labels.with(dev), fs.write_time as f64 / 1000.0, ts);
                out.push("container_fs_io_current", labels.with(dev), fs.io_in_progress as f64, ts);
                out.push("container_fs_io_time_seconds_total", labels.with(dev), fs.io_time as f64 / 1000.0, ts);
                out.push(
                    "container_fs_io_time_weighted_seconds_total",
                    labels.with(dev),
                    fs.weighted_io_time as f64 / 1000.0,
                    ts,
                );
            }
        }

        if enabled("network") {
            for iface in &s.network.interfaces {
                let l: &[(&str, &str)] = &[("interface", &iface.name)];
                let nets: [(&'static str, u64); 8] = [
                    ("container_network_receive_bytes_total", iface.rx_bytes),
                    ("container_network_receive_packets_total", iface.rx_packets),
                    ("container_network_receive_packets_dropped_total", iface.rx_dropped),
                    ("container_network_receive_errors_total", iface.rx_errors),
                    ("container_network_transmit_bytes_total", iface.tx_bytes),
                    ("container_network_transmit_packets_total", iface.tx_packets),
                    ("container_network_transmit_packets_dropped_total", iface.tx_dropped),
                    ("container_network_transmit_errors_total", iface.tx_errors),
                ];
                for (fam, v) in nets {
                    out.push(fam, labels.with(l), v as f64, ts);
                }
            }
        }

        if enabled("tcp") {
            tcp_metrics(&mut out, &labels, &s.network, ts);
        }
        if enabled("udp") {
            udp_metrics(&mut out, &labels, &s.network, ts);
        }

        if enabled("oom_event") {
            out.push("container_oom_events_total", labels.with(&[]), s.oom_events as f64, ts);
        }
    }
    out.push("container_scrape_error", vec![], 0.0, None);

    // Machine metrics.
    let mi = manager.machine_info();
    let mts = Some(mi.timestamp.0.timestamp_millis());
    let mlabels = || {
        vec![
            ("boot_id".to_string(), mi.boot_id.clone()),
            ("machine_id".to_string(), mi.machine_id.clone()),
            ("system_uuid".to_string(), mi.system_uuid.clone()),
        ]
    };
    out.push("machine_cpu_physical_cores", mlabels(), mi.num_physical_cores as f64, mts);
    out.push("machine_cpu_cores", mlabels(), mi.num_cores as f64, mts);
    out.push("machine_cpu_sockets", mlabels(), mi.num_sockets as f64, mts);
    out.push("machine_memory_bytes", mlabels(), mi.memory_capacity as f64, mts);
    out.push("machine_swap_bytes", mlabels(), mi.swap_capacity as f64, mts);
    for (mode, v) in [
        ("app_direct_mode", mi.nvm_info.app_direct_mode_capacity),
        ("memory_mode", mi.nvm_info.memory_mode_capacity),
    ] {
        let mut l = mlabels();
        l.push(("mode".to_string(), mode.to_string()));
        l.sort_by(|a, b| a.0.cmp(&b.0));
        out.push("machine_nvm_capacity", l, v as f64, mts);
    }
    out.push(
        "machine_nvm_avg_power_budget_watts",
        mlabels(),
        mi.nvm_info.avg_power_budget as f64,
        mts,
    );
    nic_metrics(&mut out, &mi, mlabels(), mts);
    out.push("machine_scrape_error", vec![], 0.0, None);
    out
}

/// `container_network_tcp{,6}_usage_total` by `tcp_state`, for every
/// container (zeros where it has no network), as upstream.
fn tcp_metrics(out: &mut Output, labels: &ContainerLabels, net: &v1::NetworkStats, ts: Option<i64>) {
    for (family, t) in [("container_network_tcp_usage_total", &net.tcp), ("container_network_tcp6_usage_total", &net.tcp6)] {
        for (state, v) in [
            ("established", t.established),
            ("synsent", t.syn_sent),
            ("synrecv", t.syn_recv),
            ("finwait1", t.fin_wait1),
            ("finwait2", t.fin_wait2),
            ("timewait", t.time_wait),
            ("close", t.close),
            ("closewait", t.close_wait),
            ("lastack", t.last_ack),
            ("listen", t.listen),
            ("closing", t.closing),
        ] {
            out.push(family, labels.with(&[("tcp_state", state)]), v as f64, ts);
        }
    }
}

/// `container_network_udp{,6}_usage_total` by `udp_state`.
fn udp_metrics(out: &mut Output, labels: &ContainerLabels, net: &v1::NetworkStats, ts: Option<i64>) {
    for (family, u) in [("container_network_udp_usage_total", &net.udp), ("container_network_udp6_usage_total", &net.udp6)] {
        for (state, v) in [
            ("listen", u.listen),
            ("dropped", u.dropped),
            ("rxqueued", u.rx_queued),
            ("txqueued", u.tx_queued),
        ] {
            out.push(family, labels.with(&[("udp_state", state)]), v as f64, ts);
        }
    }
}

/// One MTU and one speed gauge per device in machine info's
/// `network_devices` (beyond v0.49.2; #29, stormcos#637). Same devices and
/// refresh as machine info: re-read every `-update_machine_info_interval`.
fn nic_metrics(out: &mut Output, mi: &v1::MachineInfo, machine: Vec<(String, String)>, mts: Option<i64>) {
    for nic in &mi.network_devices {
        let mut l = machine.clone();
        l.push(("device".to_string(), nic.name.clone()));
        l.push(("mac".to_string(), nic.mac_address.clone()));
        l.sort_by(|a, b| a.0.cmp(&b.0));
        out.push("machine_nic_mtu_bytes", l.clone(), nic.mtu as f64, mts);
        out.push("machine_nic_speed_mbps", l, nic.speed as f64, mts);
    }
}

pub fn router(manager: Arc<Manager>, opts: MetricsOpts) -> axum::Router {
    use axum::extract::State;
    use axum::response::IntoResponse;

    let state = Arc::new((manager, opts));
    axum::Router::new()
        .route(
            "/metrics",
            axum::routing::get(
                |State(state): State<Arc<(Arc<Manager>, MetricsOpts)>>,
                 headers: axum::http::HeaderMap| async move {
                    let accept = headers
                        .get(axum::http::header::ACCEPT)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    if crate::proto::wants_protobuf(accept) {
                        let mut buf = Vec::with_capacity(256 * 1024);
                        render_proto(&state.0, &state.1, &mut buf);
                        return ([(axum::http::header::CONTENT_TYPE, crate::proto::CONTENT_TYPE)], buf)
                            .into_response();
                    }
                    let mut buf = String::with_capacity(256 * 1024);
                    render(&state.0, &state.1, &mut buf);
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
                        buf,
                    )
                        .into_response()
                },
            ),
        )
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_labels_follow_store_container_labels() {
        let mut spec = v1::ContainerSpec::default();
        spec.labels.insert("app".into(), "web".into());
        spec.envs.insert("APP_MODE".into(), "prod".into());
        let reference = v1::ContainerReference::default();
        let mut opts = MetricsOpts { store_container_labels: true, ..Default::default() };
        let own = ContainerLabels::own_from("/c", &spec, &reference, &opts);
        assert_eq!(own.get("container_env_APP_MODE").map(String::as_str), Some("prod"));
        assert_eq!(own.get("container_label_app").map(String::as_str), Some("web"));

        opts.store_container_labels = false;
        opts.whitelisted_container_labels = vec!["app".into()];
        let own = ContainerLabels::own_from("/c", &spec, &reference, &opts);
        assert!(!own.contains_key("container_env_APP_MODE"));
        assert_eq!(own.get("container_label_app").map(String::as_str), Some("web"));
    }

    #[test]
    fn each_nic_gets_an_mtu_and_a_speed() {
        let mi = v1::MachineInfo {
            network_devices: vec![
                v1::NetInfo { name: "enp1s0f0".into(), mac_address: "aa:bb:cc:00:00:01".into(), speed: 10000, mtu: 9000 },
                v1::NetInfo { name: "br0".into(), mac_address: "aa:bb:cc:00:00:02".into(), speed: -1, mtu: 1500 },
            ],
            ..Default::default()
        };
        let mut out = Output::default();
        nic_metrics(&mut out, &mi, vec![("machine_id".to_string(), "m1".to_string())], None);
        let mut buf = String::new();
        out.render(&mut buf);
        for line in [
            r#"machine_nic_mtu_bytes{device="enp1s0f0",mac="aa:bb:cc:00:00:01",machine_id="m1"} 9000"#,
            r#"machine_nic_speed_mbps{device="enp1s0f0",mac="aa:bb:cc:00:00:01",machine_id="m1"} 10000"#,
            r#"machine_nic_mtu_bytes{device="br0",mac="aa:bb:cc:00:00:02",machine_id="m1"} 1500"#,
            r#"machine_nic_speed_mbps{device="br0",mac="aa:bb:cc:00:00:02",machine_id="m1"} -1"#,
            "# TYPE machine_nic_mtu_bytes gauge",
            "# TYPE machine_nic_speed_mbps gauge",
        ] {
            assert!(buf.lines().any(|l| l == line), "missing {line:?} in\n{buf}");
        }
    }

    fn base() -> ContainerLabels {
        ContainerLabels { base: vec![("id".to_string(), "/c".to_string())] }
    }

    #[test]
    fn tcp_and_udp_states() {
        let mut net = v1::NetworkStats::default();
        net.tcp.established = 3;
        net.tcp.listen = 2;
        net.tcp6.listen = 1;
        net.udp.listen = 4;
        net.udp6.dropped = 7;
        let mut out = Output::default();
        tcp_metrics(&mut out, &base(), &net, None);
        udp_metrics(&mut out, &base(), &net, None);
        let mut buf = String::new();
        out.render(&mut buf);
        for line in [
            "# TYPE container_network_tcp_usage_total gauge",
            r#"container_network_tcp_usage_total{id="/c",tcp_state="established"} 3"#,
            r#"container_network_tcp_usage_total{id="/c",tcp_state="listen"} 2"#,
            r#"container_network_tcp_usage_total{id="/c",tcp_state="closing"} 0"#,
            r#"container_network_tcp6_usage_total{id="/c",tcp_state="listen"} 1"#,
            r#"container_network_udp_usage_total{id="/c",udp_state="listen"} 4"#,
            r#"container_network_udp6_usage_total{id="/c",udp_state="dropped"} 7"#,
        ] {
            assert!(buf.lines().any(|l| l == line), "missing {line:?} in\n{buf}");
        }
        assert_eq!(buf.lines().filter(|l| l.starts_with("container_network_tcp")).count(), 22);
        assert_eq!(buf.lines().filter(|l| l.starts_with("container_network_udp")).count(), 8);
    }

    #[test]
    fn every_pushed_family_is_rendered() {
        // A family missing from FAMILIES would be dropped silently (the cfs
        // and quota families were, until #40).
        let src = include_str!("collect.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        for chunk in code.split('"') {
            if (chunk.starts_with("container_") || chunk.starts_with("machine_"))
                && chunk.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !chunk.ends_with('_')
                && chunk != "machine_id" // a label name
            {
                assert!(crate::families::family(chunk).is_some(), "{chunk} is pushed but not in FAMILIES");
            }
        }
    }

    #[test]
    fn proto_and_text_carry_the_same_families() {
        let mut out = Output::default();
        out.push("container_last_seen", vec![("id".into(), "/".into())], 2.0, Some(7));
        out.push("machine_scrape_error", vec![], 0.0, None);
        let mut pb = Vec::new();
        out.render_proto(&mut pb);
        // Two delimited families, in name order.
        let first = pb[0] as usize;
        assert_eq!(&pb[1..4], &[0x0a, 19, b'c']);
        assert_eq!(&pb[3..22], b"container_last_seen");
        let second = &pb[1 + first..];
        assert_eq!(second[0] as usize, second.len() - 1);
        assert_eq!(&second[3..23], b"machine_scrape_error");
    }

    #[test]
    fn a_repeated_series_is_served_once_first_wins() {
        let mut out = Output::default();
        let l = |d: &str| vec![("device".to_string(), d.to_string()), ("id".to_string(), "/".to_string())];
        out.push("container_fs_reads_bytes_total", l(""), 1.0, None);
        out.push("container_fs_reads_bytes_total", l("/dev/sda"), 2.0, None);
        out.push("container_fs_reads_bytes_total", l(""), 3.0, None);
        let mut buf = String::new();
        out.render(&mut buf);
        let samples: Vec<&str> = buf.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(
            samples,
            [
                "container_fs_reads_bytes_total{device=\"\",id=\"/\"} 1",
                "container_fs_reads_bytes_total{device=\"/dev/sda\",id=\"/\"} 2",
            ]
        );
    }
}
