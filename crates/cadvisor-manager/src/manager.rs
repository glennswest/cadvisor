//! The manager: container registry, discovery, and per-container adaptive
//! housekeeping (Linux only).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use cadvisor_host::cgroup::{CgroupReader, CgroupSpec};
use cadvisor_host::fs::FsService;
use cadvisor_host::watch::{CgroupEvent, CgroupWatcher};
use cadvisor_host::{machine, parse};
use cadvisor_model::v1;
use cadvisor_model::GoTime;
use tokio::sync::mpsc;

use crate::TimedStore;

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("unknown container {0:?}")]
    UnknownContainer(String),
    #[error(transparent)]
    Host(#[from] cadvisor_host::HostError),
}

#[derive(Debug, Clone)]
pub struct ManagerConfig {
    pub cgroup_root: String,
    pub housekeeping_interval: Duration,
    pub max_housekeeping_interval: Duration,
    pub allow_dynamic_housekeeping: bool,
    pub global_housekeeping_interval: Duration,
    pub storage_duration: Duration,
    pub cadvisor_version: String,
    pub containerd_socket: String,
    pub containerd_namespace: String,
    pub crio_socket: String,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        ManagerConfig {
            cgroup_root: cadvisor_host::cgroup::UNIFIED_MOUNTPOINT.to_string(),
            housekeeping_interval: Duration::from_secs(1),
            max_housekeeping_interval: Duration::from_secs(60),
            allow_dynamic_housekeeping: true,
            global_housekeeping_interval: Duration::from_secs(60),
            storage_duration: Duration::from_secs(120),
            cadvisor_version: env!("CARGO_PKG_VERSION").to_string(),
            containerd_socket: "/run/containerd/containerd.sock".to_string(),
            containerd_namespace: "k8s.io".to_string(),
            crio_socket: "/var/run/crio/crio.sock".to_string(),
        }
    }
}

pub struct ContainerHandle {
    name: String,
    pub reference: RwLock<v1::ContainerReference>,
    pub spec: RwLock<v1::ContainerSpec>,
    pub store: Mutex<TimedStore>,
    /// Init PID for /proc/<pid>/net readings (root container: 1; runtime
    /// containers: task/conmon child pid; plain raw cgroups: none).
    pub init_pid: RwLock<Option<u32>>,
    /// Only the pod sandbox (and the root container) reports network stats.
    pub reports_network: std::sync::atomic::AtomicBool,
    /// Writable-layer dir for fs accounting + cached (bytes, inodes) from the
    /// periodic directory walk.
    pub rootfs_diff: Option<String>,
    pub fs_usage: RwLock<(u64, u64)>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ContainerHandle {
    pub fn name(&self) -> &str {
        &self.name
    }
}

pub struct Manager {
    cfg: ManagerConfig,
    reader: CgroupReader,
    fs: FsService,
    containers: RwLock<HashMap<String, Arc<ContainerHandle>>>,
    machine_info: RwLock<v1::MachineInfo>,
    version_info: v1::VersionInfo,
    events: Mutex<Vec<Arc<v1::Event>>>,
    event_tx: tokio::sync::broadcast::Sender<Arc<v1::Event>>,
    containerd: tokio::sync::OnceCell<Option<cadvisor_runtime::ContainerdClient>>,
    crio: cadvisor_runtime::CrioClient,
}

/// Filter for the v1.3 events endpoint.
#[derive(Debug, Clone, Default)]
pub struct EventQuery {
    /// Absolute container name the query is anchored at.
    pub container_name: String,
    /// Include events of subcontainers (prefix match).
    pub include_subcontainers: bool,
    /// Event types to include.
    pub event_types: Vec<v1::EventType>,
    /// Maximum number of (most recent) events to return; 0 = default 10.
    pub max_events: usize,
    pub start_time: GoTime,
    pub end_time: GoTime,
}

impl EventQuery {
    pub fn matches(&self, ev: &v1::Event) -> bool {
        if !self.event_types.contains(&ev.event_type) {
            return false;
        }
        let name_ok = ev.container_name == self.container_name
            || (self.include_subcontainers
                && ev
                    .container_name
                    .starts_with(&format!("{}/", self.container_name.trim_end_matches('/'))));
        if !name_ok {
            return false;
        }
        if !self.start_time.is_zero() && ev.timestamp < self.start_time {
            return false;
        }
        if !self.end_time.is_zero() && ev.timestamp > self.end_time {
            return false;
        }
        true
    }
}

fn system_time_to_gotime(t: SystemTime) -> GoTime {
    let dt: chrono::DateTime<chrono::Utc> = t.into();
    GoTime(dt)
}

fn read_os_release_pretty_name() -> String {
    let Ok(content) = std::fs::read_to_string("/etc/os-release") else {
        return String::new();
    };
    for line in content.lines() {
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            return v.trim_matches('"').to_string();
        }
    }
    String::new()
}

impl Manager {
    pub fn new(cfg: ManagerConfig) -> Result<Self, ManagerError> {
        let reader = CgroupReader::new(&cfg.cgroup_root);
        let fs = FsService::new()?;
        let machine_info = machine::machine_info(&fs, GoTime::now())?;
        let version_info = v1::VersionInfo {
            kernel_version: std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            container_os_version: read_os_release_pretty_name(),
            docker_version: String::new(),
            docker_api_version: String::new(),
            cadvisor_version: cfg.cadvisor_version.clone(),
            cadvisor_revision: String::new(),
        };
        let crio = cadvisor_runtime::CrioClient::new(&cfg.crio_socket);
        Ok(Manager {
            cfg,
            reader,
            fs,
            containers: RwLock::new(HashMap::new()),
            machine_info: RwLock::new(machine_info),
            version_info,
            events: Mutex::new(Vec::new()),
            event_tx: tokio::sync::broadcast::channel(1024).0,
            containerd: tokio::sync::OnceCell::new(),
            crio,
        })
    }

    pub fn config(&self) -> &ManagerConfig {
        &self.cfg
    }

    pub fn machine_info(&self) -> v1::MachineInfo {
        self.machine_info.read().unwrap().clone()
    }

    pub fn version_info(&self) -> v1::VersionInfo {
        self.version_info.clone()
    }

    pub fn handles(&self) -> Vec<Arc<ContainerHandle>> {
        self.containers.read().unwrap().values().cloned().collect()
    }

    pub fn handle(&self, name: &str) -> Option<Arc<ContainerHandle>> {
        self.containers.read().unwrap().get(name).cloned()
    }

    /// Lazily-connected containerd client (absent socket -> None, logged once).
    async fn containerd(&self) -> Option<&cadvisor_runtime::ContainerdClient> {
        self.containerd
            .get_or_init(|| async {
                match cadvisor_runtime::ContainerdClient::connect(
                    &self.cfg.containerd_socket,
                    &self.cfg.containerd_namespace,
                )
                .await
                {
                    Ok(c) => Some(c),
                    Err(e) => {
                        tracing::info!(socket = %self.cfg.containerd_socket, error = %e, "containerd not available");
                        None
                    }
                }
            })
            .await
            .as_ref()
    }

    /// Runtime metadata for a 64-hex container cgroup, via the factory chain
    /// crio -> containerd (with retries for registration races). conmon shim
    /// cgroups stay raw (upstream's crio factory declines them; raw monitors
    /// them without enrichment).
    async fn runtime_meta(&self, basename: &str) -> Option<cadvisor_runtime::ContainerMeta> {
        if basename.starts_with("crio-conmon-") {
            return None;
        }
        let id = cadvisor_runtime::extract_container_id(basename)?;
        let is_crio = basename.starts_with("crio-");
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(100 << attempt)).await;
            }
            if is_crio {
                if !self.crio.available() {
                    return None;
                }
                match self.crio.inspect(id).await {
                    Ok(meta) => return Some(meta),
                    Err(cadvisor_runtime::RuntimeError::NotFound) => continue,
                    Err(e) => {
                        tracing::warn!(id, error = %e, "crio inspect failed");
                        return None;
                    }
                }
            } else if let Some(client) = self.containerd().await {
                match client.inspect(id).await {
                    Ok(meta) => return Some(meta),
                    Err(cadvisor_runtime::RuntimeError::NotFound) => return None,
                    Err(_) => continue,
                }
            } else {
                return None;
            }
        }
        None
    }

    /// Starts discovery and housekeeping. Call once, from the runtime.
    pub async fn start(self: &Arc<Self>) -> Result<(), ManagerError> {
        // Initial sweep: pre-existing containers get no creation events
        // (matches upstream, whose event store starts empty).
        for name in self.walk_tree()? {
            self.add_container(&name, false).await;
        }

        // inotify watcher on a dedicated thread.
        let (tx, mut rx) = mpsc::unbounded_channel::<CgroupEvent>();
        let watcher = CgroupWatcher::new(&self.cfg.cgroup_root)?;
        std::thread::Builder::new()
            .name("cgroup-watch".into())
            .spawn(move || watcher.run(tx))
            .expect("spawn watcher thread");

        let mgr = Arc::clone(self);
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                match ev {
                    CgroupEvent::Added(name) => mgr.add_container(&name, true).await,
                    CgroupEvent::Removed(name) => mgr.remove_container(&name),
                }
            }
        });

        // Global sweep safety net.
        let mgr = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(mgr.cfg.global_housekeeping_interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Ok(current) = mgr.walk_tree() else { continue };
                let known: Vec<String> =
                    mgr.containers.read().unwrap().keys().cloned().collect();
                for name in &current {
                    if !known.iter().any(|k| k == name) {
                        mgr.add_container(name, true).await;
                    }
                }
                for name in known {
                    if name != "/" && !current.contains(&name) && !mgr.reader.exists(&name) {
                        mgr.remove_container(&name);
                    }
                }
            }
        });
        Ok(())
    }

    fn walk_tree(&self) -> Result<Vec<String>, ManagerError> {
        let mut out = vec!["/".to_string()];
        let mut stack = vec!["/".to_string()];
        while let Some(cg) = stack.pop() {
            let mut children = Vec::new();
            if self.reader.list_children(&cg, &mut children).is_ok() {
                for child in children {
                    stack.push(child.clone());
                    out.push(child);
                }
            }
        }
        Ok(out)
    }

    /// Records an event. `timestamp` for creation events is the container's
    /// creation time (upstream semantics) — which makes events for cgroups
    /// older than the 24h storage age prune out immediately, exactly like a
    /// default cadvisor.
    fn record_event(self: &Arc<Self>, name: &str, event_type: v1::EventType, timestamp: GoTime) {
        let event = Arc::new(v1::Event {
            container_name: name.to_string(),
            timestamp,
            event_type,
            event_data: Default::default(),
        });
        let _ = self.event_tx.send(Arc::clone(&event));
        let mut events = self.events.lock().unwrap();
        let pos = events.partition_point(|e| e.timestamp <= event.timestamp);
        events.insert(pos, event);
        // Upstream storage policy: per-type 24h age + 100k count. A single
        // ordered buffer with the same limits is equivalent at our volumes.
        if let Some(newest) = events.last().map(|e| e.timestamp) {
            let cutoff = newest.0 - chrono::Duration::hours(24);
            events.retain(|e| e.timestamp.0 >= cutoff);
        }
        let excess = events.len().saturating_sub(100_000);
        if excess > 0 {
            events.drain(..excess);
        }
    }

    async fn add_container(self: &Arc<Self>, name: &str, record_creation: bool) {
        // Upstream ignores systemd mount-unit cgroups.
        let basename = name.rsplit('/').next().unwrap_or(name);
        if name.ends_with(".mount") {
            return;
        }
        if self.containers.read().unwrap().contains_key(name) {
            return;
        }
        let mut spec = match self.build_spec(name) {
            Ok(spec) => spec,
            Err(e) => {
                if !matches!(&e, ManagerError::Host(h) if h.is_not_found()) {
                    tracing::warn!(container = name, error = %e, "failed to read spec");
                }
                return;
            }
        };

        // Factory chain: runtime enrichment for 64-hex container cgroups,
        // raw fallback otherwise.
        let mut reference = v1::ContainerReference { name: name.to_string(), ..Default::default() };
        let mut init_pid = if name == "/" { Some(1) } else { None };
        let mut reports_network = name == "/";
        let mut rootfs_diff = None;
        if let Some(meta) = self.runtime_meta(basename).await {
            reference.id = meta.id;
            reference.aliases = meta.aliases;
            reference.namespace = meta.namespace;
            spec.image = meta.image;
            spec.labels = meta.labels;
            spec.has_network = meta.reports_network;
            spec.has_filesystem = meta.rootfs_diff.is_some();
            init_pid = meta.init_pid;
            reports_network = meta.reports_network;
            rootfs_diff = meta.rootfs_diff;
        }

        let handle = Arc::new(ContainerHandle {
            name: name.to_string(),
            reference: RwLock::new(reference),
            spec: RwLock::new(spec),
            store: Mutex::new(TimedStore::new(self.cfg.storage_duration)),
            init_pid: RwLock::new(init_pid),
            reports_network: std::sync::atomic::AtomicBool::new(reports_network),
            rootfs_diff,
            fs_usage: RwLock::new((0, 0)),
            task: Mutex::new(None),
        });
        {
            let mut map = self.containers.write().unwrap();
            if map.contains_key(name) {
                return;
            }
            map.insert(name.to_string(), Arc::clone(&handle));
        }
        tracing::debug!(container = name, "container added");
        if record_creation {
            let created = handle.spec.read().unwrap().creation_time;
            self.record_event(name, v1::EventType::ContainerCreation, created);
        }

        let mgr = Arc::clone(self);
        let task_handle = Arc::clone(&handle);
        let task = tokio::spawn(async move { mgr.housekeeping(task_handle).await });
        *handle.task.lock().unwrap() = Some(task);
    }

    fn remove_container(self: &Arc<Self>, name: &str) {
        let removed = self.containers.write().unwrap().remove(name);
        if let Some(handle) = removed {
            if let Some(task) = handle.task.lock().unwrap().take() {
                task.abort();
            }
            tracing::debug!(container = name, "container removed");
            self.record_event(name, v1::EventType::ContainerDeletion, GoTime::now());
        }
    }

    /// Per-container housekeeping with upstream's adaptive interval: unchanged
    /// stats double the interval up to the max; a change resets it.
    async fn housekeeping(self: Arc<Self>, handle: Arc<ContainerHandle>) {
        let base = self.cfg.housekeeping_interval;
        let mut interval = base;
        let mut prev: Option<Arc<v1::ContainerStats>> = None;
        let mut last_slow = std::time::Instant::now() - Duration::from_secs(3600);
        let mut first_iteration = true;
        loop {
            // Slow path (once a minute, plus once shortly after creation):
            // refresh writable-layer usage (upstream fsHandler cadence) and
            // re-read the cgroup spec — controller files appear a beat after
            // the cgroup itself when systemd updates subtree_control.
            if last_slow.elapsed() >= Duration::from_secs(60) || first_iteration {
                first_iteration = false;
                last_slow = std::time::Instant::now();
                if let Some(diff_dir) = &handle.rootfs_diff {
                    let usage =
                        cadvisor_host::fs::dir_usage(std::path::Path::new(diff_dir));
                    *handle.fs_usage.write().unwrap() = usage;
                }
                if handle.name() != "/" {
                    if let Ok(cg) = self.reader.read_spec(handle.name()) {
                        let mut spec = handle.spec.write().unwrap();
                        spec.has_memory = cg.has_memory;
                        spec.memory.limit = cg.memory_limit;
                        spec.memory.swap_limit = cg.swap_limit;
                        spec.memory.reservation = cg.memory_reservation;
                        spec.cpu.limit = cg.cpu_shares;
                        spec.cpu.mask = cg.cpu_mask;
                        spec.cpu.period = cg.cpu_period;
                        spec.cpu.quota = cg.cpu_quota.unwrap_or(0);
                        spec.processes.limit = cg.pids_limit;
                    }
                }
            }
            match self.sample(&handle) {
                Ok(sample) => {
                    let sample = Arc::new(sample);
                    // OOM kill counter went up -> emit oom + oomKill events.
                    if let Some(p) = &prev {
                        if sample.oom_events > p.oom_events {
                            self.record_event(handle.name(), v1::EventType::Oom, GoTime::now());
                            self.record_event(handle.name(), v1::EventType::OomKill, GoTime::now());
                        }
                    }
                    if self.cfg.allow_dynamic_housekeeping {
                        let unchanged = prev.as_ref().is_some_and(|p| {
                            p.cpu.usage.total == sample.cpu.usage.total
                                && p.memory.usage == sample.memory.usage
                        });
                        interval = if unchanged {
                            (interval * 2).min(self.cfg.max_housekeeping_interval)
                        } else {
                            base
                        };
                    }
                    prev = Some(Arc::clone(&sample));
                    handle.store.lock().unwrap().push(sample);
                }
                Err(e) => {
                    if !self.reader.exists(handle.name()) {
                        return; // container went away; sweeper/watcher removes it
                    }
                    tracing::debug!(container = handle.name(), error = %e, "stats read failed");
                }
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// One stats sample, with per-container network (when this container owns
    /// a netns and reports it) and root-container extras.
    fn sample(&self, handle: &ContainerHandle) -> Result<v1::ContainerStats, ManagerError> {
        let name = handle.name();
        let mut stats = self.reader.read_stats(name, GoTime::now())?;
        if handle.reports_network.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(pid) = *handle.init_pid.read().unwrap() {
                if let Ok(content) = std::fs::read_to_string(format!("/proc/{pid}/net/dev")) {
                    let interfaces = parse::parse_proc_net_dev(&content);
                    if let Some(first) = interfaces.first() {
                        stats.network.interface = first.clone();
                    }
                    stats.network.interfaces = interfaces;
                }
            }
        }
        if name == "/" {
            self.fill_root_stats(&mut stats)?;
        } else if let Some(diff_dir) = &handle.rootfs_diff {
            // Runtime container writable layer: partition capacity/inodes
            // from statfs, usage from the cached directory walk.
            if let Some(p) = self.fs.partition_for_path(diff_dir) {
                if let Ok(u) = self.fs.usage(&p.mountpoint) {
                    let (du_bytes, _) = *handle.fs_usage.read().unwrap();
                    stats.filesystem.push(v1::FsStats {
                        device: p.device.clone(),
                        fs_type: "vfs".to_string(),
                        limit: u.capacity,
                        usage: du_bytes,
                        base_usage: du_bytes,
                        available: u.available,
                        has_inodes: true,
                        inodes: u.inodes,
                        inodes_free: u.inodes_free,
                        ..Default::default()
                    });
                }
            }
        }
        Ok(stats)
    }

    fn fill_root_stats(&self, stats: &mut v1::ContainerStats) -> Result<(), ManagerError> {
        // Root has no memory.current; upstream derives usage from meminfo.
        if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
            let mut total = 0u64;
            let mut free = 0u64;
            for line in content.lines() {
                let mut f = line.split_whitespace();
                match f.next() {
                    Some("MemTotal:") => total = f.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                    Some("MemFree:") => free = f.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                    _ => {}
                }
            }
            if stats.memory.usage == 0 && total >= free {
                stats.memory.usage = (total - free) * 1024;
                stats.memory.working_set = stats.memory.usage;
            }
        }

        // Global filesystem stats + disk IO counters.
        let disk = self.fs.disk_stats().unwrap_or_default();
        for p in self.fs.partitions() {
            let Ok(u) = self.fs.usage(&p.mountpoint) else { continue };
            let d = disk.get(&(p.major, p.minor));
            stats.filesystem.push(v1::FsStats {
                device: p.device.clone(),
                fs_type: "vfs".to_string(),
                limit: u.capacity,
                usage: u.capacity - u.free,
                base_usage: 0,
                available: u.available,
                has_inodes: true,
                inodes: u.inodes,
                inodes_free: u.inodes_free,
                reads_completed: d.map_or(0, |d| d.reads_completed),
                reads_merged: d.map_or(0, |d| d.reads_merged),
                sectors_read: d.map_or(0, |d| d.sectors_read),
                read_time: d.map_or(0, |d| d.read_time),
                writes_completed: d.map_or(0, |d| d.writes_completed),
                writes_merged: d.map_or(0, |d| d.writes_merged),
                sectors_written: d.map_or(0, |d| d.sectors_written),
                write_time: d.map_or(0, |d| d.write_time),
                io_in_progress: d.map_or(0, |d| d.io_in_progress),
                io_time: d.map_or(0, |d| d.io_time),
                weighted_io_time: d.map_or(0, |d| d.weighted_io_time),
            });
        }
        Ok(())
    }

    /// Builds a v1 ContainerSpec for a raw cgroup (root gets machine-level
    /// limits and network/filesystem flags — verified against the captured
    /// v0.49.2 root spec).
    fn build_spec(&self, name: &str) -> Result<v1::ContainerSpec, ManagerError> {
        let cg: CgroupSpec = self.reader.read_spec(name)?;
        let mut spec = v1::ContainerSpec {
            creation_time: cg
                .creation_time
                .map(system_time_to_gotime)
                .unwrap_or_default(),
            has_cpu: cg.has_cpu,
            has_memory: cg.has_memory,
            has_processes: cg.has_processes,
            has_disk_io: cg.has_disk_io,
            ..Default::default()
        };
        spec.cpu.limit = cg.cpu_shares;
        spec.cpu.mask = cg.cpu_mask;
        spec.cpu.period = cg.cpu_period;
        if let Some(q) = cg.cpu_quota {
            spec.cpu.quota = q;
        }
        spec.memory.limit = cg.memory_limit;
        spec.memory.swap_limit = cg.swap_limit;
        spec.memory.reservation = cg.memory_reservation;
        // pids.max "max" is reported verbatim as u64::MAX (matches upstream).
        spec.processes.limit = cg.pids_limit;

        if name == "/" {
            let mi = self.machine_info.read().unwrap();
            spec.has_cpu = true;
            spec.cpu.period = 0;
            spec.has_memory = true;
            spec.memory.limit = mi.memory_capacity;
            spec.memory.swap_limit = mi.swap_capacity;
            spec.has_processes = true;
            spec.has_disk_io = true;
            spec.has_filesystem = true;
            spec.has_network = !mi.network_devices.is_empty();
        }
        Ok(spec)
    }

    /// v1.0-1.3 `containers/` — info for one container with direct
    /// subcontainer references.
    pub fn container_info(
        &self,
        name: &str,
        req: &v1::ContainerInfoRequest,
    ) -> Result<v1::ContainerInfo, ManagerError> {
        let handle = self
            .handle(name)
            .ok_or_else(|| ManagerError::UnknownContainer(name.to_string()))?;
        Ok(self.assemble_info(&handle, req, true))
    }

    /// v1.1+ `subcontainers/` — self plus all recursive descendants.
    pub fn subcontainers_info(
        &self,
        name: &str,
        req: &v1::ContainerInfoRequest,
    ) -> Result<Vec<v1::ContainerInfo>, ManagerError> {
        if self.handle(name).is_none() {
            return Err(ManagerError::UnknownContainer(name.to_string()));
        }
        let prefix = if name == "/" { "/".to_string() } else { format!("{name}/") };
        let mut handles: Vec<Arc<ContainerHandle>> = self
            .handles()
            .into_iter()
            .filter(|h| h.name() == name || h.name().starts_with(&prefix))
            .collect();
        handles.sort_by(|a, b| a.name().cmp(b.name()));
        // Upstream includes each item's own direct-subcontainer references.
        Ok(handles
            .iter()
            .map(|h| self.assemble_info(h, req, true))
            .collect())
    }

    fn direct_children(&self, name: &str) -> Vec<v1::ContainerReference> {
        let prefix = if name == "/" { "/".to_string() } else { format!("{name}/") };
        let mut out: Vec<v1::ContainerReference> = self
            .containers
            .read()
            .unwrap()
            .keys()
            .filter(|k| {
                k.starts_with(&prefix)
                    && k.len() > prefix.len()
                    && !k[prefix.len()..].contains('/')
            })
            .map(|k| v1::ContainerReference { name: k.clone(), ..Default::default() })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    fn assemble_info(
        &self,
        handle: &ContainerHandle,
        req: &v1::ContainerInfoRequest,
        with_subcontainers: bool,
    ) -> v1::ContainerInfo {
        let stats = handle
            .store
            .lock()
            .unwrap()
            .in_range(req.start, req.end, req.num_stats)
            .iter()
            .map(|s| (**s).clone())
            .collect();
        v1::ContainerInfo {
            reference: handle.reference.read().unwrap().clone(),
            subcontainers: if with_subcontainers {
                self.direct_children(handle.name())
            } else {
                Vec::new()
            },
            spec: handle.spec.read().unwrap().clone(),
            stats,
        }
    }

    /// v1.2+ `docker` request type, answered from runtime (containerd/CRI-O)
    /// metadata: empty arg -> all runtime containers keyed by name; an id or
    /// alias -> single-entry map.
    pub fn docker_containers(
        &self,
        id_or_name: &str,
        req: &v1::ContainerInfoRequest,
    ) -> Result<HashMap<String, v1::ContainerInfo>, ManagerError> {
        let mut out = HashMap::new();
        for handle in self.handles() {
            let reference = handle.reference.read().unwrap().clone();
            if reference.namespace.is_empty() {
                continue;
            }
            let matched = id_or_name.is_empty()
                || reference.id.starts_with(id_or_name)
                || reference.aliases.iter().any(|a| a == id_or_name);
            if matched {
                out.insert(handle.name().to_string(), self.assemble_info(&handle, req, false));
            }
        }
        if out.is_empty() && !id_or_name.is_empty() {
            return Err(ManagerError::UnknownContainer(id_or_name.to_string()));
        }
        Ok(out)
    }

    /// PIDs in a container's cgroup (v2 /ps endpoint).
    pub fn container_pids(&self, name: &str) -> Option<Vec<u32>> {
        self.handle(name)?;
        self.reader.procs(name).ok()
    }

    /// v2 /storage endpoint: runtime capacity/usage per filesystem.
    pub fn storage_info(&self) -> Vec<cadvisor_model::v2::FsInfo> {
        use cadvisor_model::v2;
        let now = GoTime::now();
        let mut out = Vec::new();
        for p in self.fs.partitions() {
            let Ok(u) = self.fs.usage(&p.mountpoint) else { continue };
            out.push(v2::FsInfo {
                timestamp: now,
                device: p.device.clone(),
                mountpoint: p.mountpoint.clone(),
                capacity: u.capacity,
                available: u.available,
                usage: u.capacity - u.free,
                labels: (p.mountpoint == "/").then(|| vec!["root".to_string()]),
                inodes: Some(u.inodes),
                inodes_free: Some(u.inodes_free),
            });
        }
        out
    }

    /// v2.1 /machinestats filesystem section: per-partition capacity/usage
    /// plus /proc/diskstats counters (Duration fields are nanoseconds).
    pub fn machine_fs_stats(&self) -> Vec<cadvisor_model::v2::MachineFsStats> {
        use cadvisor_model::v2;
        let disk = self.fs.disk_stats().unwrap_or_default();
        let mut out = Vec::new();
        for p in self.fs.partitions() {
            let Ok(u) = self.fs.usage(&p.mountpoint) else { continue };
            let d = disk.get(&(p.major, p.minor));
            let ms_to_ns = |ms: u64| Some((ms as i64) * 1_000_000);
            out.push(v2::MachineFsStats {
                device: p.device.clone(),
                fs_type: "vfs".to_string(),
                capacity: Some(u.capacity),
                usage: Some(u.capacity - u.free),
                available: Some(u.available),
                inodes_free: Some(u.inodes_free),
                disk_stats: v2::DiskStats {
                    reads_completed: Some(d.map_or(0, |d| d.reads_completed)),
                    reads_merged: Some(d.map_or(0, |d| d.reads_merged)),
                    sectors_read: Some(d.map_or(0, |d| d.sectors_read)),
                    read_duration: ms_to_ns(d.map_or(0, |d| d.read_time)),
                    writes_completed: Some(d.map_or(0, |d| d.writes_completed)),
                    writes_merged: Some(d.map_or(0, |d| d.writes_merged)),
                    sectors_written: Some(d.map_or(0, |d| d.sectors_written)),
                    write_duration: ms_to_ns(d.map_or(0, |d| d.write_time)),
                    io_in_progress: Some(d.map_or(0, |d| d.io_in_progress)),
                    io_duration: ms_to_ns(d.map_or(0, |d| d.io_time)),
                    weighted_io_duration: ms_to_ns(d.map_or(0, |d| d.weighted_io_time)),
                },
            });
        }
        out
    }

    /// v1.3 events query (historical, non-streaming): last `max_events`
    /// matching events, chronological.
    pub fn events(&self, q: &EventQuery) -> Vec<Arc<v1::Event>> {
        let max = if q.max_events == 0 { 10 } else { q.max_events };
        let events = self.events.lock().unwrap();
        let matching: Vec<Arc<v1::Event>> =
            events.iter().filter(|e| q.matches(e)).cloned().collect();
        let skip = matching.len().saturating_sub(max);
        matching.into_iter().skip(skip).collect()
    }

    /// Live event stream (for `?stream=true`).
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<Arc<v1::Event>> {
        self.event_tx.subscribe()
    }
}
