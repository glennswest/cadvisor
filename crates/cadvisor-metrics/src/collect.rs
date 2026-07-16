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

/// Metric group token for each container family (upstream -disable_metrics
/// vocabulary). Families of default-disabled groups are simply never emitted.
fn family_group(name: &str) -> Option<&'static str> {
    if name.starts_with("container_cpu_load") || name == "container_tasks_state" {
        return Some("cpuLoad");
    }
    if name.starts_with("container_cpu") {
        return Some("cpu");
    }
    if name.starts_with("container_memory") {
        return Some("memory");
    }
    if matches!(
        name,
        "container_fs_inodes_free"
            | "container_fs_inodes_total"
            | "container_fs_limit_bytes"
            | "container_fs_usage_bytes"
    ) {
        return Some("disk");
    }
    if name.starts_with("container_fs") || name.starts_with("container_blkio") {
        return Some("diskIO");
    }
    if name.starts_with("container_network") {
        return Some("network");
    }
    if name == "container_oom_events_total" {
        return Some("oom_event");
    }
    None
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

#[derive(Default)]
struct Output {
    families: BTreeMap<&'static str, Vec<Series>>,
}

impl Output {
    fn push(&mut self, family: &'static str, labels: Vec<(String, String)>, value: f64, ts: Option<i64>) {
        self.families.entry(family).or_default().push((labels, value, ts));
    }

    fn render(mut self, buf: &mut String) {
        for (name, kind, help) in FAMILIES {
            let Some(mut series) = self.families.remove(name) else { continue };
            buf.push_str("# HELP ");
            buf.push_str(name);
            buf.push(' ');
            buf.push_str(help);
            buf.push_str("\n# TYPE ");
            buf.push_str(name);
            buf.push(' ');
            buf.push_str(kind.as_str());
            buf.push('\n');
            // client_golang orders series by label values.
            series.sort_by(|a, b| a.0.cmp(&b.0));
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
        let mut own = std::collections::BTreeMap::new();
        own.insert("id".to_string(), handle.name().to_string());
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
        for (k, v) in &spec.envs {
            own.insert(format!("container_env_{}", sanitize_label_name(k)), v.clone());
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

pub fn render(manager: &Manager, opts: &MetricsOpts, buf: &mut String) {
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
            if s.cpu.usage.per_cpu.is_empty() {
                // Upstream only emits series for cpus with usage > 0.
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
    out.push("machine_scrape_error", vec![], 0.0, None);

    out.render(buf);
}

pub fn router(manager: Arc<Manager>, opts: MetricsOpts) -> axum::Router {
    use axum::extract::State;
    use axum::response::IntoResponse;

    let state = Arc::new((manager, opts));
    axum::Router::new()
        .route(
            "/metrics",
            axum::routing::get(
                |State(state): State<Arc<(Arc<Manager>, MetricsOpts)>>| async move {
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
