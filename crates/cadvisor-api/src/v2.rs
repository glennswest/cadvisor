//! v2.0/v2.1 request handling.
//!
//! Shapes verified against captured v0.49.2 responses: v2.0 `stats` returns
//! `map[name][]DeprecatedContainerStats`, v2.1 returns
//! `map[name]ContainerInfo` with the root container skipped; `machine`
//! returns the v1 MachineInfo; `version` a bare JSON string.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::Response;
use cadvisor_manager::Manager;
use cadvisor_manager::manager::ContainerHandle;
use cadvisor_model::v2;
use cadvisor_model::{GoTime, v1};

use crate::common::{Params, json, plain};

/// Parses upstream RequestOptions from query params (defaults: type=name,
/// count=64, recursive=false).
pub struct RequestOptions {
    pub id_type: String,
    pub count: i64,
    pub recursive: bool,
}

impl RequestOptions {
    pub fn parse(params: &Params) -> Result<Self, String> {
        let id_type = params.get("type").unwrap_or(v2::TYPE_NAME).to_string();
        if !matches!(id_type.as_str(), v2::TYPE_NAME | v2::TYPE_DOCKER | v2::TYPE_PODMAN) {
            return Err(format!("unknown 'type' {id_type:?}"));
        }
        let count = match params.get("count") {
            Some(v) => v.parse::<i64>().map_err(|e| format!("invalid 'count' option: {e}"))?,
            None => 64,
        };
        Ok(RequestOptions {
            id_type,
            count,
            recursive: params.get("recursive") == Some("true"),
        })
    }
}

fn v2_spec(handle: &ContainerHandle) -> v2::ContainerSpec {
    let s = handle.spec.read().unwrap().clone();
    let reference = handle.reference.read().unwrap().clone();
    v2::ContainerSpec {
        creation_time: s.creation_time,
        aliases: reference.aliases,
        namespace: reference.namespace,
        labels: s.labels,
        envs: s.envs,
        has_cpu: s.has_cpu,
        // Upstream's v1->v2 spec conversion copies only shares/max_limit/mask
        // (no period/quota) and drops the pids limit entirely — verified
        // against captured v0.49.2 /api/v2.0/spec output.
        cpu: v1::CpuSpec {
            limit: s.cpu.limit,
            max_limit: s.cpu.max_limit,
            mask: s.cpu.mask,
            quota: 0,
            period: 0,
        },
        has_memory: s.has_memory,
        memory: s.memory,
        has_hugetlb: s.has_hugetlb,
        has_custom_metrics: s.has_custom_metrics,
        custom_metrics: s.custom_metrics,
        has_processes: s.has_processes,
        processes: v1::ProcessSpec::default(),
        has_network: s.has_network,
        has_filesystem: s.has_filesystem,
        has_disk_io: s.has_disk_io,
        image: s.image,
    }
}

fn v2_network(n: &v1::NetworkStats) -> v2::NetworkStats {
    v2::NetworkStats {
        interfaces: n.interfaces.clone(),
        tcp: n.tcp.clone(),
        tcp6: n.tcp6.clone(),
        udp: n.udp.clone(),
        udp6: n.udp6.clone(),
        tcp_advanced: n.tcp_advanced.clone(),
    }
}

fn v2_stats(handle: &ContainerHandle, spec: &v1::ContainerSpec, count: i64) -> Vec<v2::ContainerStats> {
    let samples = handle.store.lock().unwrap().in_range(GoTime::zero(), GoTime::zero(), count);
    let mut out = Vec::with_capacity(samples.len());
    for (i, s) in samples.iter().enumerate() {
        let cpu_inst = if i > 0 {
            cadvisor_manager::derive::inst_cpu(&samples[i - 1], s).map(|c| v1::CpuInstStats {
                usage: v1::CpuInstUsage {
                    total: c.usage.total,
                    per_cpu: c.usage.per_cpu,
                    user: c.usage.user,
                    system: c.usage.system,
                },
            })
        } else {
            None
        };
        out.push(v2::ContainerStats {
            timestamp: s.timestamp,
            cpu: spec.has_cpu.then(|| s.cpu.clone()),
            cpu_inst,
            disk_io: spec.has_disk_io.then(|| s.disk_io.clone()),
            memory: spec.has_memory.then(|| s.memory.clone()),
            hugetlb: spec.has_hugetlb.then(|| s.hugetlb.clone()),
            network: spec.has_network.then(|| v2_network(&s.network)),
            processes: spec.has_processes.then(|| {
                s.processes.clone()
            }),
            filesystem: spec.has_filesystem.then(|| {
                let mut fs = v2::FilesystemStats::default();
                if s.filesystem.len() == 1 {
                    fs.total_usage_bytes = Some(s.filesystem[0].usage);
                    fs.base_usage_bytes = Some(s.filesystem[0].base_usage);
                    fs.inode_usage = Some(handle.fs_usage.read().unwrap().1);
                }
                fs
            }),
            load: None,
            accelerators: s.accelerators.clone(),
            custom_metrics: s.custom_metrics.clone(),
            perf_stats: s.perf_stats.clone(),
            perf_uncore_stats: s.perf_uncore_stats.clone(),
            referenced_memory: s.referenced_memory,
            resctrl: s.resctrl.clone(),
        });
    }
    out
}

fn deprecated_stats(handle: &ContainerHandle, spec: &v1::ContainerSpec, count: i64) -> Vec<v2::DeprecatedContainerStats> {
    v2_stats(handle, spec, count)
        .into_iter()
        .map(|s| v2::DeprecatedContainerStats {
            timestamp: s.timestamp,
            has_cpu: spec.has_cpu,
            cpu: s.cpu.unwrap_or_default(),
            cpu_inst: s.cpu_inst,
            has_disk_io: spec.has_disk_io,
            disk_io: s.disk_io.unwrap_or_default(),
            has_memory: spec.has_memory,
            memory: s.memory.unwrap_or_default(),
            has_hugetlb: spec.has_hugetlb,
            hugetlb: s.hugetlb.unwrap_or_default(),
            has_network: spec.has_network,
            network: s.network.unwrap_or_default(),
            has_processes: spec.has_processes,
            processes: s.processes.unwrap_or_default(),
            has_filesystem: spec.has_filesystem,
            filesystem: Vec::new(),
            has_load: false,
            load: Default::default(),
            has_custom_metrics: spec.has_custom_metrics,
            custom_metrics: s.custom_metrics,
            perf_stats: s.perf_stats,
            perf_uncore_stats: s.perf_uncore_stats,
            referenced_memory: s.referenced_memory,
            resctrl: s.resctrl,
        })
        .collect()
}

/// Resolves the queried container set (name/docker/podman + recursive).
fn resolve(
    manager: &Manager,
    name: &str,
    opts: &RequestOptions,
) -> Result<Vec<Arc<ContainerHandle>>, String> {
    if opts.id_type != v2::TYPE_NAME {
        // docker/podman types resolve via runtime (containerd/CRI-O) metadata.
        let id = name.trim_start_matches('/');
        let found: Vec<Arc<ContainerHandle>> = manager
            .handles()
            .into_iter()
            .filter(|h| {
                let r = h.reference.read().unwrap();
                !r.namespace.is_empty()
                    && (id.is_empty()
                        || r.id.starts_with(id)
                        || r.aliases.iter().any(|a| a == id))
            })
            .collect();
        if found.is_empty() && !id.is_empty() {
            return Err(format!("unable to find container {name:?}"));
        }
        return Ok(found);
    }
    let Some(handle) = manager.handle(name) else {
        return Err(format!("unknown container {name:?}"));
    };
    if !opts.recursive {
        return Ok(vec![handle]);
    }
    let prefix = if name == "/" { "/".to_string() } else { format!("{name}/") };
    let mut handles: Vec<Arc<ContainerHandle>> = manager
        .handles()
        .into_iter()
        .filter(|h| h.name() == name || h.name().starts_with(&prefix))
        .collect();
    handles.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(handles)
}

fn percentiles_from(mut values: Vec<u64>) -> v2::Percentiles {
    if values.is_empty() {
        return v2::Percentiles::default();
    }
    values.sort_unstable();
    let n = values.len();
    let pick = |p: f64| values[((n - 1) as f64 * p) as usize];
    let mean = values.iter().sum::<u64>() / n as u64;
    v2::Percentiles {
        present: true,
        mean,
        max: values[n - 1],
        fifty: pick(0.50),
        ninety: pick(0.90),
        ninety_five: pick(0.95),
    }
}

fn derived_stats(handle: &ContainerHandle) -> v2::DerivedStats {
    let samples = handle.store.lock().unwrap().in_range(GoTime::zero(), GoTime::zero(), -1);
    let mut cpu_rates = Vec::new();
    let mut mem = Vec::new();
    for (i, s) in samples.iter().enumerate() {
        mem.push(s.memory.working_set);
        if i > 0 {
            if let Some(inst) = cadvisor_manager::derive::inst_cpu(&samples[i - 1], s) {
                // milliCPU/s
                cpu_rates.push(inst.usage.total / 1_000_000);
            }
        }
    }
    let latest = samples.last();
    let minute = v2::Usage {
        percent_complete: if samples.len() >= 60 { 100 } else { (samples.len() * 100 / 60) as i32 },
        cpu: percentiles_from(cpu_rates.clone()),
        memory: percentiles_from(mem.clone()),
    };
    v2::DerivedStats {
        timestamp: latest.map(|s| s.timestamp).unwrap_or_else(GoTime::now),
        latest_usage: v2::InstantUsage {
            cpu: cpu_rates.last().copied().unwrap_or(0),
            memory: latest.map(|s| s.memory.working_set).unwrap_or(0),
        },
        minute_usage: minute,
        hour_usage: v2::Usage::default(),
        day_usage: v2::Usage::default(),
    }
}

fn process_list(manager: &Manager, name: &str) -> Vec<v2::ProcessInfo> {
    let mut out = Vec::new();
    let Some(pids) = manager.container_pids(name) else { return out };
    for pid in pids {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let cmdline = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        // /proc/<pid>/stat: fields after the parenthesized comm.
        let Some(close) = stat.rfind(')') else { continue };
        let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        if fields.len() < 40 {
            continue;
        }
        let state = fields[0].to_string();
        let ppid: i64 = fields[1].parse().unwrap_or(0);
        let vsize: u64 = fields[20].parse().unwrap_or(0);
        let rss_pages: u64 = fields[21].parse().unwrap_or(0);
        let psr: i64 = fields[36].parse().unwrap_or(0);
        let uid = status
            .lines()
            .find_map(|l| l.strip_prefix("Uid:"))
            .and_then(|l| l.split_whitespace().next().map(String::from))
            .unwrap_or_default();
        let fd_count = std::fs::read_dir(format!("/proc/{pid}/fd"))
            .map(|d| d.count() as i64)
            .unwrap_or(0);
        let cgroup_path = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
            .ok()
            .and_then(|c| c.lines().next().and_then(|l| l.splitn(3, ':').nth(2).map(String::from)))
            .unwrap_or_default();
        out.push(v2::ProcessInfo {
            user: uid,
            pid: pid as i64,
            ppid,
            start_time: String::new(),
            percent_cpu: 0.0,
            percent_memory: 0.0,
            rss: rss_pages * 4096,
            virtual_size: vsize,
            status: state,
            running_time: String::new(),
            cgroup_path,
            cmd: cmdline.replace('\0', " ").trim().to_string(),
            fd_count,
            psr,
        });
    }
    out
}

pub async fn handle_v2(
    manager: &Arc<Manager>,
    version: &str,
    resource: &str,
    container_name: &str,
    params: &Params,
) -> Response {
    let opts = match RequestOptions::parse(params) {
        Ok(o) => o,
        Err(e) => return plain(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    match resource {
        "version" => json(&manager.config().cadvisor_version),
        "machine" => json(&manager.machine_info()),
        "attributes" => {
            let attrs = v2::Attributes::new(&manager.machine_info(), &manager.version_info());
            json(&attrs)
        }
        "stats" => match resolve(manager, container_name, &opts) {
            Err(e) => plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not get stats for {container_name:?}: {e}"),
            ),
            Ok(handles) => {
                if version == "v2.1" {
                    let mut out: BTreeMap<String, v2::ContainerInfo> = BTreeMap::new();
                    for h in handles {
                        // The root container is exposed via machinestats.
                        if h.name() == "/" {
                            continue;
                        }
                        let spec1 = h.spec.read().unwrap().clone();
                        out.insert(
                            h.name().to_string(),
                            v2::ContainerInfo {
                                spec: v2_spec(&h),
                                stats: v2_stats(&h, &spec1, opts.count),
                            },
                        );
                    }
                    json(&out)
                } else {
                    let mut out: BTreeMap<String, Vec<v2::DeprecatedContainerStats>> = BTreeMap::new();
                    for h in handles {
                        let spec1 = h.spec.read().unwrap().clone();
                        out.insert(h.name().to_string(), deprecated_stats(&h, &spec1, opts.count));
                    }
                    json(&out)
                }
            }
        },
        "machinestats" if version == "v2.1" => {
            let Some(root) = manager.handle("/") else {
                return plain(StatusCode::INTERNAL_SERVER_ERROR, "could not get machine stats".into());
            };
            let spec = root.spec.read().unwrap().clone();
            let stats: Vec<v2::MachineStats> = v2_stats(&root, &spec, 10)
                .into_iter()
                .map(|s| v2::MachineStats {
                    timestamp: s.timestamp,
                    cpu: s.cpu,
                    cpu_inst: s.cpu_inst,
                    memory: s.memory,
                    network: s.network,
                    filesystem: root_machine_fs(manager),
                    load: None,
                })
                .collect();
            json(&stats)
        }
        "spec" => match resolve(manager, container_name, &opts) {
            Err(e) => plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not get spec for {container_name:?}: {e}"),
            ),
            Ok(handles) => {
                let out: BTreeMap<String, v2::ContainerSpec> =
                    handles.iter().map(|h| (h.name().to_string(), v2_spec(h))).collect();
                json(&out)
            }
        },
        "summary" => match resolve(manager, container_name, &opts) {
            Err(e) => plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not get summary for {container_name:?}: {e}"),
            ),
            Ok(handles) => {
                let out: BTreeMap<String, v2::DerivedStats> = handles
                    .iter()
                    .map(|h| (h.name().to_string(), derived_stats(h)))
                    .collect();
                json(&out)
            }
        },
        "ps" => json(&process_list(manager, container_name)),
        "storage" => {
            let label = params.get("label");
            let mut out = manager.storage_info();
            if let Some(label) = label {
                out.retain(|fs| fs.labels.as_deref().unwrap_or(&[]).iter().any(|l| l == label));
            }
            json(&out)
        }
        "appmetrics" => json(&BTreeMap::<String, serde_json::Value>::new()),
        other => plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("unknown request type {other:?}"),
        ),
    }
}

fn root_machine_fs(manager: &Manager) -> Vec<v2::MachineFsStats> {
    manager.machine_fs_stats()
}
