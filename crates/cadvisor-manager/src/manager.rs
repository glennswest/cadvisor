//! The manager: container registry, discovery, and per-container adaptive
//! housekeeping (Linux only).

use std::collections::{BTreeMap, HashMap, HashSet};
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
    #[error("{0} is not a cgroup v2 mount (no cgroup.controllers): cadvisor needs the host's unified cgroup tree there")]
    NotCgroup2(String),
}

/// Upstream exits when its raw container factory cannot register; the same
/// here, rather than serving a root with zero CPU and no subcontainers when
/// `root` is an empty directory or a mount of something else.
pub fn check_cgroup2(root: &str) -> Result<(), ManagerError> {
    if std::path::Path::new(root).join("cgroup.controllers").is_file() {
        Ok(())
    } else {
        Err(ManagerError::NotCgroup2(root.to_string()))
    }
}

/// How long after discovery a cgroup without identity is re-checked for a
/// workload record on every housekeeping tick.
const IDENTITY_GRACE: Duration = Duration::from_secs(30);

/// After a failed containerd connection, how long before the next attempt
/// (#42 `de041f31067b`: a socket that appears later is used then).
const CONNECT_RETRY: Duration = Duration::from_secs(10);

/// A runtime client connected on first use. Only a success is kept; a
/// failure is retried after `retry`, so a runtime that starts after cadvisor
/// is picked up (#42 `de041f31067b`).
struct Reconnect<T> {
    client: Option<Arc<T>>,
    failed_at: Option<std::time::Instant>,
    logged: bool,
}

impl<T> Default for Reconnect<T> {
    fn default() -> Self {
        Reconnect { client: None, failed_at: None, logged: false }
    }
}

impl<T> Reconnect<T> {
    async fn get<E: std::fmt::Display, F: std::future::Future<Output = Result<T, E>>>(
        &mut self,
        retry: Duration,
        connect: impl FnOnce() -> F,
    ) -> Option<Arc<T>> {
        if let Some(c) = &self.client {
            return Some(Arc::clone(c));
        }
        if self.failed_at.is_some_and(|t| t.elapsed() < retry) {
            return None;
        }
        match connect().await {
            Ok(c) => {
                if self.logged {
                    tracing::info!("containerd connected");
                }
                let c = Arc::new(c);
                self.client = Some(Arc::clone(&c));
                Some(c)
            }
            Err(e) => {
                if !self.logged {
                    tracing::info!(error = %e, "containerd not available (retried every {}s)", retry.as_secs());
                    self.logged = true;
                }
                self.failed_at = Some(std::time::Instant::now());
                None
            }
        }
    }
}

/// Whether a cgroup basename names a runtime container that the runtime
/// chain can enrich (a 64-hex id; not a CRI-O conmon shim).
fn runtime_candidate(basename: &str) -> bool {
    !basename.starts_with("crio-conmon-") && cadvisor_runtime::extract_container_id(basename).is_some()
}

#[derive(Debug, Clone)]
pub struct ManagerConfig {
    pub cgroup_root: String,
    pub housekeeping_interval: Duration,
    pub max_housekeeping_interval: Duration,
    pub allow_dynamic_housekeeping: bool,
    pub global_housekeeping_interval: Duration,
    /// How often machine info (incl. the disk map) is re-read; upstream
    /// `-update_machine_info_interval`.
    pub update_machine_info_interval: Duration,
    pub storage_duration: Duration,
    pub cadvisor_version: String,
    pub containerd_socket: String,
    pub containerd_namespace: String,
    pub crio_socket: String,
    /// Env var key prefixes exported as `spec.envs` (upstream
    /// `-env_metadata_whitelist`); empty = none.
    pub env_metadata_whitelist: Vec<String>,
    /// Directory of stormpump workload identity records (rustkube-node#84);
    /// empty = `/run/rustkube/workloads`, then `/hostrun/rustkube/workloads`.
    pub workload_identity_dir: String,
    /// Optional stats collected only when their metric group is enabled, as
    /// upstream gates them on `includedMetrics` (#40).
    pub collect: Collect,
}

/// Which optional stats housekeeping reads. All off by default: upstream's
/// default `-disable_metrics` lists `tcp`, `udp`, `sched` and `hugetlb`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Collect {
    /// `/proc/<pid>/net/tcp` and `tcp6` connection states (`tcp`).
    pub tcp: bool,
    /// `/proc/<pid>/net/udp` and `udp6` (`udp`).
    pub udp: bool,
    /// `/proc/<pid>/schedstat` of every process in the cgroup tree (`sched`).
    pub sched: bool,
    /// `hugetlb.<size>.{current,events}` (`hugetlb`).
    pub hugetlb: bool,
}

/// Upstream's `schedulerStatsFromProcs` state: the last reading per live
/// pid, plus the totals of pids that have exited, so the counters never go
/// backwards when a process ends.
#[derive(Debug, Default)]
pub struct SchedCache {
    live: HashMap<u32, v1::CpuSchedstat>,
    exited: v1::CpuSchedstat,
}

impl SchedCache {
    /// Folds one round of `(pid, reading)` into the cache and returns the
    /// container's totals.
    pub fn update(&mut self, readings: impl IntoIterator<Item = (u32, v1::CpuSchedstat)>) -> v1::CpuSchedstat {
        let mut alive = HashSet::new();
        for (pid, r) in readings {
            alive.insert(pid);
            self.live.insert(pid, r);
        }
        let exited = &mut self.exited;
        self.live.retain(|pid, r| {
            if alive.contains(pid) {
                return true;
            }
            exited.run_time += r.run_time;
            exited.runqueue_time += r.runqueue_time;
            exited.run_periods += r.run_periods;
            false
        });
        let mut total = self.exited.clone();
        for r in self.live.values() {
            total.run_time += r.run_time;
            total.runqueue_time += r.runqueue_time;
            total.run_periods += r.run_periods;
        }
        total
    }
}

impl Default for ManagerConfig {
    fn default() -> Self {
        ManagerConfig {
            cgroup_root: cadvisor_host::cgroup::UNIFIED_MOUNTPOINT.to_string(),
            housekeeping_interval: Duration::from_secs(1),
            max_housekeeping_interval: Duration::from_secs(60),
            allow_dynamic_housekeeping: true,
            global_housekeeping_interval: Duration::from_secs(60),
            update_machine_info_interval: Duration::from_secs(300),
            storage_duration: Duration::from_secs(120),
            cadvisor_version: env!("CARGO_PKG_VERSION").to_string(),
            containerd_socket: "/run/containerd/containerd.sock".to_string(),
            containerd_namespace: "k8s.io".to_string(),
            crio_socket: "/var/run/crio/crio.sock".to_string(),
            env_metadata_whitelist: Vec::new(),
            workload_identity_dir: String::new(),
            collect: Collect::default(),
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
    pub rootfs_diff: RwLock<Option<String>>,
    pub fs_usage: RwLock<(u64, u64)>,
    /// Per-pid schedstat readings (only with `Collect::sched`).
    pub sched: Mutex<SchedCache>,
    /// A runtime container (64-hex cgroup) whose runtime metadata could not
    /// be read yet: housekeeping retries it (#42 `de041f31067b`).
    runtime_pending: std::sync::atomic::AtomicBool,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ContainerHandle {
    fn new(name: &str, spec: v1::ContainerSpec, storage_duration: Duration) -> Self {
        ContainerHandle {
            name: name.to_string(),
            reference: RwLock::new(v1::ContainerReference { name: name.to_string(), ..Default::default() }),
            spec: RwLock::new(spec),
            store: Mutex::new(TimedStore::new(storage_duration)),
            init_pid: RwLock::new(if name == "/" { Some(1) } else { None }),
            reports_network: std::sync::atomic::AtomicBool::new(name == "/"),
            rootfs_diff: RwLock::new(None),
            fs_usage: RwLock::new((0, 0)),
            sched: Mutex::new(SchedCache::default()),
            runtime_pending: std::sync::atomic::AtomicBool::new(false),
            task: Mutex::new(None),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

pub struct Manager {
    cfg: ManagerConfig,
    reader: CgroupReader,
    /// The mount table, re-read with machine info (#42 `0cfda650cfd7`). Each
    /// reader takes one snapshot for its whole answer.
    fs: RwLock<Arc<FsService>>,
    containers: RwLock<HashMap<String, Arc<ContainerHandle>>>,
    machine_info: RwLock<v1::MachineInfo>,
    /// `major:minor` keys seen in `io.stat` but missing from the disk map
    /// since the last re-scan, so each new device triggers one re-scan.
    disk_misses: Mutex<HashSet<String>>,
    version_info: v1::VersionInfo,
    events: Mutex<Vec<Arc<v1::Event>>>,
    event_tx: tokio::sync::broadcast::Sender<Arc<v1::Event>>,
    containerd: tokio::sync::Mutex<Reconnect<cadvisor_runtime::ContainerdClient>>,
    crio: cadvisor_runtime::CrioClient,
    workloads: cadvisor_runtime::WorkloadRecords,
    /// When the manager was made: the start of the startup grace of
    /// [`Manager::collection_health`].
    created: std::time::Instant,
}

/// Whether housekeeping is still producing root samples (#44
/// `a9c7ff6db1ab`): the API health probe reads this, because the stats and
/// metrics endpoints serve the last stored sample however old it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionHealth {
    /// `ok`, `starting` (no root sample yet, within the grace), `stale` (the
    /// newest root sample is too old) or `absent` (no root sample after the
    /// grace).
    pub status: &'static str,
    /// Age of the newest root sample, if there is one.
    pub root_sample_age: Option<Duration>,
    /// A root sample older than this is stale; it is also the startup grace.
    pub stale_after: Duration,
}

impl CollectionHealth {
    /// `ok` and `starting` are healthy; `stale` and `absent` are not.
    pub fn healthy(&self) -> bool {
        matches!(self.status, "ok" | "starting")
    }

    /// The judgement, apart from the clocks: the newest root sample's age
    /// (None: no sample), how long the manager has been up, and the bound.
    pub fn judge(age: Option<Duration>, up: Duration, stale_after: Duration) -> Self {
        let status = match age {
            Some(a) if a <= stale_after => "ok",
            Some(_) => "stale",
            None if up <= stale_after => "starting",
            None => "absent",
        };
        CollectionHealth { status, root_sample_age: age, stale_after }
    }
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
        check_cgroup2(&cfg.cgroup_root)?;
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
        let workloads = cadvisor_runtime::WorkloadRecords::new(&cfg.workload_identity_dir);
        Ok(Manager {
            cfg,
            reader,
            fs: RwLock::new(Arc::new(fs)),
            containers: RwLock::new(HashMap::new()),
            machine_info: RwLock::new(machine_info),
            disk_misses: Mutex::new(HashSet::new()),
            version_info,
            events: Mutex::new(Vec::new()),
            event_tx: tokio::sync::broadcast::channel(1024).0,
            containerd: tokio::sync::Mutex::new(Reconnect::default()),
            crio,
            workloads,
            created: std::time::Instant::now(),
        })
    }

    /// How old a root sample may be before housekeeping counts as stopped:
    /// three of the longest gaps root housekeeping can leave between samples
    /// (`-max_housekeeping_interval` when dynamic housekeeping is on, else
    /// `-housekeeping_interval`). Defaults: 3 × 60 s = 180 s.
    pub fn stale_after(&self) -> Duration {
        let longest = if self.cfg.allow_dynamic_housekeeping {
            self.cfg.max_housekeeping_interval.max(self.cfg.housekeeping_interval)
        } else {
            self.cfg.housekeeping_interval
        };
        longest * 3
    }

    /// Whether the root container's newest sample is recent (#44
    /// `a9c7ff6db1ab`). Reads the manager's container map and the root's
    /// sample store, the locks housekeeping writes under.
    pub fn collection_health(&self) -> CollectionHealth {
        let latest = self.handle("/").and_then(|h| h.store.lock().unwrap().latest());
        let age = latest.map(|s| {
            // A clock stepped back makes the sample "from the future": age 0.
            (chrono::Utc::now() - s.timestamp.0).to_std().unwrap_or_default()
        });
        CollectionHealth::judge(age, self.created.elapsed(), self.stale_after())
    }

    pub fn config(&self) -> &ManagerConfig {
        &self.cfg
    }

    pub fn machine_info(&self) -> v1::MachineInfo {
        self.machine_info.read().unwrap().clone()
    }

    fn fs(&self) -> Arc<FsService> {
        Arc::clone(&self.fs.read().unwrap())
    }

    /// Re-reads the mount table and machine info, as upstream does every
    /// `-update_machine_info_interval`: a filesystem mounted or unmounted
    /// since is seen everywhere the mount table is used (#42
    /// `0cfda650cfd7`). On error the old table and info are kept.
    pub fn refresh_machine_info(&self) {
        match FsService::new() {
            Ok(fs) => self.install_fs(fs),
            Err(e) => tracing::warn!(error = %e, "mountinfo re-read failed"),
        }
    }

    /// Machine info from `fs`; on success both replace the old ones.
    fn install_fs(&self, fs: FsService) {
        match machine::machine_info(&fs, GoTime::now()) {
            Ok(mi) => {
                *self.fs.write().unwrap() = Arc::new(fs);
                *self.machine_info.write().unwrap() = mi;
                self.disk_misses.lock().unwrap().clear();
            }
            Err(e) => tracing::warn!(error = %e, "machine info refresh failed"),
        }
    }

    /// Re-scans `/sys/block` when `io.stat` names a device the disk map does
    /// not know (a volume attached after the last scan), once per new device.
    fn note_disks(&self, entries: &[v1::PerDiskStats]) {
        let unknown = unknown_disks(entries, &self.machine_info.read().unwrap().disk_map);
        if unknown.is_empty() {
            return;
        }
        {
            let mut misses = self.disk_misses.lock().unwrap();
            let before = misses.len();
            misses.extend(unknown);
            if misses.len() == before {
                return;
            }
        }
        let map = machine::disk_map();
        tracing::debug!(devices = map.len(), "disk map re-scanned");
        self.machine_info.write().unwrap().disk_map = map;
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

    /// The containerd client, connected on first use; a failed connection is
    /// tried again after [`CONNECT_RETRY`] (logged once).
    async fn containerd(&self) -> Option<Arc<cadvisor_runtime::ContainerdClient>> {
        self.containerd
            .lock()
            .await
            .get(CONNECT_RETRY, || {
                cadvisor_runtime::ContainerdClient::connect(&self.cfg.containerd_socket, &self.cfg.containerd_namespace)
            })
            .await
    }

    /// Runtime metadata for a 64-hex container cgroup, via the factory chain
    /// crio -> containerd, `attempts` times (registration races: a container
    /// not found yet, or a transient error, is asked again). conmon shim
    /// cgroups stay raw (upstream's crio factory declines them; raw monitors
    /// them without enrichment).
    async fn runtime_meta(&self, basename: &str, attempts: u32) -> Option<cadvisor_runtime::ContainerMeta> {
        if !runtime_candidate(basename) {
            return None;
        }
        let id = cadvisor_runtime::extract_container_id(basename)?;
        let is_crio = basename.starts_with("crio-");
        for attempt in 0..attempts {
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
                    Err(cadvisor_runtime::RuntimeError::NotFound) => continue,
                    Err(e) => {
                        tracing::debug!(id, error = %e, "containerd inspect failed");
                        continue;
                    }
                }
            } else {
                return None;
            }
        }
        None
    }

    /// Identity for a stormpump workload cgroup from the kubelet's record.
    fn workload_meta(&self, name: &str) -> Option<cadvisor_runtime::ContainerMeta> {
        if name == "/" {
            return None;
        }
        self.workloads.lookup(name).map(cadvisor_runtime::WorkloadRecord::into_meta)
    }

    /// Fills a handle's reference and spec from runtime metadata.
    fn apply_meta(&self, handle: &ContainerHandle, meta: cadvisor_runtime::ContainerMeta) {
        {
            let mut reference = handle.reference.write().unwrap();
            reference.id = meta.id;
            reference.aliases = meta.aliases;
            reference.namespace = meta.namespace;
        }
        {
            let mut spec = handle.spec.write().unwrap();
            spec.image = meta.image;
            spec.labels = meta.labels;
            spec.envs = cadvisor_runtime::whitelisted_env(&meta.env, &self.cfg.env_metadata_whitelist);
            spec.has_network = meta.reports_network;
        }
        *handle.init_pid.write().unwrap() = meta.init_pid;
        handle.reports_network.store(meta.reports_network, std::sync::atomic::Ordering::Relaxed);
        if meta.rootfs_diff.is_some() {
            handle.spec.write().unwrap().has_filesystem = true;
            *handle.rootfs_diff.write().unwrap() = meta.rootfs_diff;
        }
    }

    /// A runtime container added before its runtime knew it (or before the
    /// runtime's socket was up) gets its metadata once the runtime answers:
    /// same cadence as [`Self::retry_workload_identity`] (#42 `de041f31067b`).
    async fn retry_runtime_identity(&self, handle: &ContainerHandle) {
        use std::sync::atomic::Ordering;
        if !handle.runtime_pending.load(Ordering::Relaxed) {
            return;
        }
        let basename = handle.name().rsplit('/').next().unwrap_or(handle.name());
        if let Some(meta) = self.runtime_meta(basename, 1).await {
            tracing::debug!(container = handle.name(), "runtime metadata found");
            self.apply_meta(handle, meta);
            handle.runtime_pending.store(false, Ordering::Relaxed);
        }
    }

    /// A cgroup found before its workload record was written (the kubelet
    /// writes it as the container starts) is labelled once the record shows
    /// up: looked for on every housekeeping tick for the first
    /// [`IDENTITY_GRACE`], then once a minute.
    fn retry_workload_identity(&self, handle: &ContainerHandle) {
        if !handle.reference.read().unwrap().namespace.is_empty() || !self.workloads.available() {
            return;
        }
        if let Some(meta) = self.workload_meta(handle.name()) {
            tracing::debug!(container = handle.name(), "workload identity found");
            self.apply_meta(handle, meta);
        }
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

        // Machine info refresh (upstream -update_machine_info_interval).
        let mgr = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(mgr.cfg.update_machine_info_interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                tick.tick().await;
                let m = Arc::clone(&mgr);
                let _ = tokio::task::spawn_blocking(move || m.refresh_machine_info()).await;
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

        // Factory chain: a stormpump workload record (rustkube-node#84),
        // then runtime enrichment for 64-hex container cgroups, raw fallback
        // otherwise.
        let meta = match self.workload_meta(name) {
            Some(meta) => Some(meta),
            None => self.runtime_meta(basename, 3).await,
        };
        // Unchanged from before #42: has_filesystem only with a writable
        // layer (apply_meta sets it), even for `/` (#43).
        spec.has_filesystem = false;
        let handle = Arc::new(ContainerHandle::new(name, spec, self.cfg.storage_duration));
        match meta {
            Some(meta) => self.apply_meta(&handle, meta),
            None if name != "/" && runtime_candidate(basename) => {
                handle.runtime_pending.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            None => {}
        }
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
        let added = std::time::Instant::now();
        loop {
            let slow_due = last_slow.elapsed() >= Duration::from_secs(60) || first_iteration;
            if handle.name() != "/" && (slow_due || added.elapsed() < IDENTITY_GRACE) {
                self.retry_workload_identity(&handle);
                self.retry_runtime_identity(&handle).await;
            }
            // Slow path (once a minute, plus once shortly after creation):
            // refresh writable-layer usage (upstream fsHandler cadence) and
            // re-read the cgroup spec — controller files appear a beat after
            // the cgroup itself when systemd updates subtree_control.
            if slow_due {
                first_iteration = false;
                last_slow = std::time::Instant::now();
                let diff_dir = handle.rootfs_diff.read().unwrap().clone();
                if let Some(diff_dir) = diff_dir {
                    let usage =
                        cadvisor_host::fs::dir_usage(std::path::Path::new(diff_dir));
                    *handle.fs_usage.write().unwrap() = usage;
                }
                if handle.name() != "/"
                    && let Ok(cg) = self.reader.read_spec(handle.name())
                {
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
            match self.sample(&handle) {
                Ok(sample) => {
                    let sample = Arc::new(sample);
                    // OOM kill counter went up -> emit oom + oomKill events.
                    if let Some(p) = &prev
                        && sample.oom_events > p.oom_events
                    {
                        self.record_event(handle.name(), v1::EventType::Oom, GoTime::now());
                        self.record_event(handle.name(), v1::EventType::OomKill, GoTime::now());
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
        self.note_disks(&stats.disk_io.io_service_bytes);
        let pid = *handle.init_pid.read().unwrap();
        if handle.reports_network.load(std::sync::atomic::Ordering::Relaxed)
            && let Some(pid) = pid
            && let Ok(content) = std::fs::read_to_string(format!("/proc/{pid}/net/dev"))
        {
            let interfaces = parse::parse_proc_net_dev(&content);
            if let Some(first) = interfaces.first() {
                stats.network.interface = first.clone();
            }
            stats.network.interfaces = interfaces;
        }
        if handle.reports_network.load(std::sync::atomic::Ordering::Relaxed)
            && let Some(pid) = pid
        {
            let net = |f: &str| std::fs::read_to_string(format!("/proc/{pid}/net/{f}")).ok();
            if self.cfg.collect.tcp {
                stats.network.tcp = net("tcp").map(|c| parse::parse_tcp_states(&c)).unwrap_or_default();
                stats.network.tcp6 = net("tcp6").map(|c| parse::parse_tcp_states(&c)).unwrap_or_default();
            }
            if self.cfg.collect.udp {
                stats.network.udp = net("udp").map(|c| parse::parse_udp_stats(&c)).unwrap_or_default();
                stats.network.udp6 = net("udp6").map(|c| parse::parse_udp_stats(&c)).unwrap_or_default();
            }
        }
        if self.cfg.collect.sched
            && let Ok(pids) = self.reader.procs_recursive(name)
        {
            // A process that exits between the listing and the read is
            // left out of this round (and kept as exited by the cache).
            let readings = pids.into_iter().filter_map(|p| {
                let c = std::fs::read_to_string(format!("/proc/{p}/schedstat")).ok()?;
                let (run_time, runqueue_time, run_periods) = parse::parse_schedstat(&c)?;
                Some((p, v1::CpuSchedstat { run_time, runqueue_time, run_periods }))
            });
            stats.cpu.schedstat = handle.sched.lock().unwrap().update(readings);
        }
        if self.cfg.collect.hugetlb {
            stats.hugetlb = self.reader.read_hugetlb(name).unwrap_or_default();
        }
        if name == "/" {
            self.fill_root_stats(&mut stats)?;
        } else if let Some(diff_dir) = handle.rootfs_diff.read().unwrap().clone() {
            // Runtime container writable layer: partition capacity/inodes
            // from statfs, usage from the cached directory walk.
            let fs = self.fs();
            if let Some(p) = fs.partition_for_path(&diff_dir)
                && let Ok(u) = fs.usage(&p.mountpoint)
            {
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
        Ok(stats)
    }

    fn fill_root_stats(&self, stats: &mut v1::ContainerStats) -> Result<(), ManagerError> {
        let fs = self.fs();
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
        let disk = fs.disk_stats().unwrap_or_default();
        for p in fs.partitions() {
            let Ok(u) = fs.usage(&p.mountpoint) else { continue };
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
        let fs = self.fs();
        use cadvisor_model::v2;
        let now = GoTime::now();
        let mut out = Vec::new();
        for p in fs.partitions() {
            let Ok(u) = fs.usage(&p.mountpoint) else { continue };
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
        let fs = self.fs();
        use cadvisor_model::v2;
        let disk = fs.disk_stats().unwrap_or_default();
        let mut out = Vec::new();
        for p in fs.partitions() {
            let Ok(u) = fs.usage(&p.mountpoint) else { continue };
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

/// `major:minor` keys of unnamed `io.stat` entries the disk map lacks.
fn unknown_disks(entries: &[v1::PerDiskStats], map: &BTreeMap<String, v1::DiskInfo>) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.device.is_empty())
        .map(|e| format!("{}:{}", e.major, e.minor))
        .filter(|k| !map.contains_key(k))
        .collect()
}

#[cfg(test)]
mod cgroup2_tests {
    use super::*;

    #[test]
    fn a_root_without_cgroup_controllers_is_refused() {
        let dir = std::env::temp_dir().join(format!("cadvisor-cg2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.to_str().unwrap().to_string();
        let err = check_cgroup2(&root).unwrap_err().to_string();
        assert!(err.contains("is not a cgroup v2 mount"), "{err}");
        std::fs::write(dir.join("cgroup.controllers"), "cpu memory io pids\n").unwrap();
        assert!(check_cgroup2(&root).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod disk_tests {
    use super::*;

    fn entry(major: u64, minor: u64) -> v1::PerDiskStats {
        v1::PerDiskStats { major, minor, ..Default::default() }
    }

    #[test]
    fn unknown_disks_lists_only_unmapped() {
        let mut map = BTreeMap::new();
        map.insert("253:0".to_string(), v1::DiskInfo::default());
        let entries = [entry(253, 0), entry(259, 1), entry(259, 2)];
        assert_eq!(unknown_disks(&entries, &map), ["259:1", "259:2"]);
        let named = v1::PerDiskStats { device: "/dev/x".into(), ..entry(8, 0) };
        assert!(unknown_disks(&[named], &map).is_empty());
    }
}

#[cfg(test)]
mod sched_tests {
    use super::*;

    fn r(run_time: u64, runqueue_time: u64, run_periods: u64) -> v1::CpuSchedstat {
        v1::CpuSchedstat { run_time, runqueue_time, run_periods }
    }

    #[test]
    fn an_exited_pid_keeps_its_time() {
        let mut c = SchedCache::default();
        assert_eq!(c.update([(1, r(10, 1, 2)), (2, r(5, 2, 1))]), r(15, 3, 3));
        // pid 2 exits; pid 1 runs on: totals never go backwards.
        assert_eq!(c.update([(1, r(20, 1, 4))]), r(25, 3, 5));
        assert_eq!(c.update([(1, r(20, 1, 4)), (3, r(1, 0, 1))]), r(26, 3, 6));
        assert_eq!(c.update([]), r(26, 3, 6));
    }
}

#[cfg(test)]
mod refresh_tests {
    //! #42: the mount table is re-read with machine info (`0cfda650cfd7`),
    //! and runtime metadata is retried until the runtime answers
    //! (`de041f31067b`).
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cadvisor-42-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manager(dir: &std::path::Path) -> Manager {
        let cg = dir.join("cgroup");
        std::fs::create_dir_all(&cg).unwrap();
        std::fs::write(cg.join("cgroup.controllers"), "cpu memory io pids\n").unwrap();
        Manager::new(ManagerConfig {
            cgroup_root: cg.to_string_lossy().into_owned(),
            crio_socket: dir.join("crio.sock").to_string_lossy().into_owned(),
            containerd_socket: dir.join("containerd.sock").to_string_lossy().into_owned(),
            ..ManagerConfig::default()
        })
        .unwrap()
    }

    #[test]
    fn a_mount_added_later_is_seen_and_a_removed_one_goes() {
        let dir = scratch("fs");
        let mnt = dir.join("mnt");
        std::fs::create_dir_all(&mnt).unwrap();
        let mgr = manager(&dir);
        let dev = "/dev/sc42test";
        let has = |mgr: &Manager| {
            (
                mgr.storage_info().iter().any(|f| f.device == dev),
                mgr.machine_fs_stats().iter().any(|f| f.device == dev),
                mgr.machine_info().filesystems.iter().any(|f| f.device == dev),
            )
        };
        assert_eq!(has(&mgr), (false, false, false));

        // Mounted after startup: in /storage, /machinestats and machine info.
        let line = format!("99 1 253:7 / {} rw,relatime shared:1 - ext4 {dev} rw\n", mnt.display());
        mgr.install_fs(FsService::from_mountinfo(&line));
        assert_eq!(has(&mgr), (true, true, true));
        let fs = mgr.storage_info().into_iter().find(|f| f.device == dev).unwrap();
        assert_eq!(fs.mountpoint, mnt.to_string_lossy());

        // Unmounted: the periodic refresh re-reads this machine's real
        // mountinfo, which has no such device, and it is gone everywhere.
        mgr.refresh_machine_info();
        assert_eq!(has(&mgr), (false, false, false));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_failed_connection_is_not_kept() {
        let calls = AtomicUsize::new(0);
        let connect = |ok: bool| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { if ok { Ok(7u32) } else { Err("refused") } }
        };
        let mut conn = Reconnect::<u32>::default();
        let long = Duration::from_secs(3600);
        assert!(conn.get(long, || connect(false)).await.is_none());
        // Within the retry interval: not tried again.
        assert!(conn.get(long, || connect(true)).await.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // After it: tried, and the success is kept.
        assert_eq!(conn.get(Duration::ZERO, || connect(true)).await.as_deref(), Some(&7));
        assert_eq!(conn.get(Duration::ZERO, || connect(false)).await.as_deref(), Some(&7));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// A CRI-O socket answering `/containers/<id>`: 404 for the first
    /// `misses` requests, then the container.
    async fn fake_crio(sock: std::path::PathBuf, misses: usize) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let served = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let served = Arc::clone(&served);
                tokio::spawn(async move {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend_from_slice(&buf[..n]),
                        }
                    }
                    let body = r#"{"name":"web","pid":4242,"image":"docker.io/library/nginx:1.27","labels":{"io.kubernetes.container.name":"web","io.kubernetes.pod.name":"web-0"},"root":""}"#;
                    let resp = if served.fetch_add(1, Ordering::SeqCst) < misses {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string()
                    } else {
                        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len())
                    };
                    let _ = s.write_all(resp.as_bytes()).await;
                });
            }
        });
    }

    #[tokio::test]
    async fn a_container_the_runtime_did_not_know_yet_is_enriched_later() {
        let dir = scratch("rt");
        let mgr = manager(&dir);
        let id = "0123456789abcdef".repeat(4);
        let name = format!("/kubepods.slice/crio-{id}.scope");
        let h = ContainerHandle::new(&name, v1::ContainerSpec::default(), Duration::from_secs(60));
        assert!(runtime_candidate(name.rsplit('/').next().unwrap()));
        h.runtime_pending.store(true, Ordering::Relaxed);

        // The runtime is not up yet: stays raw, still pending.
        mgr.retry_runtime_identity(&h).await;
        assert!(h.reference.read().unwrap().namespace.is_empty());
        assert!(h.runtime_pending.load(Ordering::Relaxed));

        // Up, but the container is not registered yet (404): still pending.
        fake_crio(dir.join("crio.sock"), 1).await;
        mgr.retry_runtime_identity(&h).await;
        assert!(h.reference.read().unwrap().namespace.is_empty());
        assert!(h.runtime_pending.load(Ordering::Relaxed));

        // Registered: labels, image, init pid; no more retries.
        mgr.retry_runtime_identity(&h).await;
        let r = h.reference.read().unwrap().clone();
        assert_eq!(r.namespace, "crio");
        assert_eq!(r.aliases, vec!["web".to_string(), id.clone()]);
        let spec = h.spec.read().unwrap().clone();
        assert_eq!(spec.image, "docker.io/library/nginx:1.27");
        assert_eq!(spec.labels.get("io.kubernetes.pod.name").map(String::as_str), Some("web-0"));
        assert_eq!(*h.init_pid.read().unwrap(), Some(4242));
        assert!(!h.runtime_pending.load(Ordering::Relaxed));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_runtime_containers_are_retried() {
        let id = "0123456789abcdef".repeat(4);
        assert!(runtime_candidate(&format!("crio-{id}.scope")));
        assert!(runtime_candidate(&format!("cri-containerd-{id}.scope")));
        assert!(!runtime_candidate(&format!("crio-conmon-{id}.scope")));
        assert!(!runtime_candidate("system.slice"));
        assert!(!runtime_candidate("w7f3a-1"));
    }
}

#[cfg(test)]
mod collection_health_tests {
    //! #44 `a9c7ff6db1ab`: the collection health probe goes unhealthy when
    //! housekeeping stops producing root samples, though the stats store
    //! still holds the last one.
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn the_judgement() {
        let bound = 3 * S;
        let j = |age, up| CollectionHealth::judge(age, up, bound);
        assert_eq!(j(Some(S), 10 * S).status, "ok");
        assert_eq!(j(Some(bound), 10 * S).status, "ok");
        assert_eq!(j(Some(4 * S), 10 * S).status, "stale");
        assert_eq!(j(None, 2 * S).status, "starting");
        assert_eq!(j(None, 4 * S).status, "absent");
        assert!(j(Some(S), S).healthy() && j(None, S).healthy());
        assert!(!j(Some(4 * S), S).healthy() && !j(None, 4 * S).healthy());
    }

    fn fixture(tag: &str) -> (std::path::PathBuf, ManagerConfig) {
        let dir = std::env::temp_dir().join(format!("cadvisor-44-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cg = dir.join("cgroup");
        std::fs::create_dir_all(&cg).unwrap();
        std::fs::write(cg.join("cgroup.controllers"), "cpu memory io pids\n").unwrap();
        std::fs::write(cg.join("cpu.stat"), "usage_usec 1\nuser_usec 1\nsystem_usec 0\n").unwrap();
        let cfg = ManagerConfig {
            cgroup_root: cg.to_string_lossy().into_owned(),
            housekeeping_interval: Duration::from_millis(50),
            max_housekeeping_interval: Duration::from_millis(50),
            crio_socket: dir.join("crio.sock").to_string_lossy().into_owned(),
            containerd_socket: dir.join("containerd.sock").to_string_lossy().into_owned(),
            workload_identity_dir: dir.join("workloads").to_string_lossy().into_owned(),
            ..ManagerConfig::default()
        };
        (dir, cfg)
    }

    async fn wait_for(mgr: &Manager, want: &str) -> CollectionHealth {
        let deadline = std::time::Instant::now() + 10 * S;
        loop {
            let h = mgr.collection_health();
            if h.status == want {
                return h;
            }
            assert!(std::time::Instant::now() < deadline, "never {want}: last {h:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[test]
    fn the_bound_follows_the_housekeeping_flags() {
        let (dir, cfg) = fixture("bound");
        let dynamic = Manager::new(cfg.clone()).unwrap();
        assert_eq!(dynamic.stale_after(), Duration::from_millis(150));
        let fixed = Manager::new(ManagerConfig {
            housekeeping_interval: Duration::from_millis(20),
            max_housekeeping_interval: Duration::from_millis(500),
            allow_dynamic_housekeeping: false,
            ..cfg.clone()
        })
        .unwrap();
        assert_eq!(fixed.stale_after(), Duration::from_millis(60));
        let defaults = Manager::new(ManagerConfig { cgroup_root: cfg.cgroup_root, ..ManagerConfig::default() })
            .unwrap();
        assert_eq!(defaults.stale_after(), 180 * S);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn no_root_sample_past_the_grace_is_absent() {
        let (dir, cfg) = fixture("absent");
        // Never started: housekeeping takes no sample at all.
        let mgr = Manager::new(cfg).unwrap();
        assert_eq!(mgr.collection_health().status, "starting");
        let h = wait_for(&mgr, "absent").await;
        assert!(!h.healthy() && h.root_sample_age.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_sampler_that_stops_reads_stale_then_recovers() {
        let (dir, cfg) = fixture("stale");
        let cpu_stat = std::path::Path::new(&cfg.cgroup_root).join("cpu.stat");
        let mgr = Arc::new(Manager::new(cfg).unwrap());
        mgr.start().await.unwrap();
        wait_for(&mgr, "ok").await;

        // cpu.stat turned into a directory: every root read fails (EISDIR),
        // so housekeeping stores nothing new while the old sample stays.
        std::fs::remove_file(&cpu_stat).unwrap();
        std::fs::create_dir(&cpu_stat).unwrap();
        let h = wait_for(&mgr, "stale").await;
        assert!(!h.healthy());
        assert!(h.root_sample_age.unwrap() > h.stale_after, "{h:?}");
        assert!(mgr.handle("/").unwrap().store.lock().unwrap().latest().is_some(), "old sample still stored");

        // Reads work again: fresh samples, healthy again.
        std::fs::remove_dir(&cpu_stat).unwrap();
        std::fs::write(&cpu_stat, "usage_usec 2\nuser_usec 2\nsystem_usec 0\n").unwrap();
        wait_for(&mgr, "ok").await;
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
