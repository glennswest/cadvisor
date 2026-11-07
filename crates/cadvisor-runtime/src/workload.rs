//! stormpump workload identity records (rustkube-node#84).
//!
//! stormcos runs pods under stormpump, not containerd or CRI-O, and names
//! their cgroups `/stormpump/w<tag>-<n>`. The kubelet publishes one JSON file
//! per workload in the node's `/run/rustkube/workloads/`, named after the
//! cgroup's last component (`w<tag>-<n>.json`), written atomically when the
//! container starts or the pod-network sandbox is made and removed with it.
//! Format: rustkube-node `docs/metrics.md`, "Workload identity for cadvisor".
//!
//! A record carries the CRI labels plus upstream's `io.kubernetes.pod.*` and
//! `io.kubernetes.container.name`, so a stormpump container gets the same
//! `spec.labels` (and `container_label_*` series) a containerd one does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::ContainerMeta;

/// Where the kubelet writes the records on the host.
pub const HOST_DIR: &str = "/run/rustkube/workloads";
/// The same directory inside cadvisor's stormcos container, where the host's
/// `/run` is mounted at `/hostrun` (stormcos#391).
pub const HOSTRUN_DIR: &str = "/hostrun/rustkube/workloads";

/// `ContainerReference.namespace` for stormpump workloads (upstream's
/// runtimes use their own name: `containerd`, `crio`).
pub const NAMESPACE: &str = "stormpump";

/// One record (fields cadvisor uses; unknown ones are ignored).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorkloadRecord {
    pub cgroup: String,
    pub pid: u32,
    /// `container`, `sandbox` or `vm`.
    pub kind: String,
    pub reports_network: bool,
    pub namespace: String,
    pub pod: String,
    pub pod_uid: String,
    pub container: String,
    pub container_id: String,
    pub image: String,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
}

impl WorkloadRecord {
    pub fn into_meta(self) -> ContainerMeta {
        let aliases = if self.container_id.is_empty() { Vec::new() } else { vec![self.container_id.clone()] };
        ContainerMeta {
            id: self.container_id,
            namespace: NAMESPACE.to_string(),
            aliases,
            image: self.image,
            labels: self.labels,
            init_pid: (self.pid > 0).then_some(self.pid),
            reports_network: self.reports_network,
            rootfs_diff: None,
            env: Vec::new(),
        }
    }
}

/// Reads records from the first of `dirs` that exists. The directory is made
/// by the kubelet after boot, so it is looked up on every call, not once.
#[derive(Debug, Clone)]
pub struct WorkloadRecords {
    dirs: Vec<PathBuf>,
}

impl WorkloadRecords {
    /// `dir` empty: the host path, then the stormcos `/hostrun` one.
    pub fn new(dir: &str) -> Self {
        let dirs = if dir.is_empty() {
            vec![PathBuf::from(HOST_DIR), PathBuf::from(HOSTRUN_DIR)]
        } else {
            vec![PathBuf::from(dir)]
        };
        WorkloadRecords { dirs }
    }

    fn dir(&self) -> Option<&Path> {
        self.dirs.iter().map(PathBuf::as_path).find(|d| d.is_dir())
    }

    /// Whether any record directory exists (the node runs rustkube's kubelet).
    pub fn available(&self) -> bool {
        self.dir().is_some()
    }

    /// The record for cgroup `name` (cadvisor's container name, the v2 path
    /// under the cgroup root). None when there is no file, it does not parse,
    /// or it names another cgroup (same basename elsewhere in the tree).
    pub fn lookup(&self, name: &str) -> Option<WorkloadRecord> {
        let basename = name.rsplit('/').next().filter(|b| !b.is_empty())?;
        if basename.starts_with('.') || basename.contains("..") {
            return None;
        }
        let path = self.dir()?.join(format!("{basename}.json"));
        let bytes = std::fs::read(&path).ok()?;
        let record: WorkloadRecord = match serde_json::from_slice(&bytes) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "workload record unreadable");
                return None;
            }
        };
        (record.cgroup.trim_end_matches('/') == name).then_some(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cadv-wl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const CONTAINER: &str = r#"{
      "cgroup": "/stormpump/wab12-3", "pid": 4242, "kind": "container",
      "reports_network": false, "namespace": "default", "pod": "web-0",
      "pod_uid": "u-1", "container": "nginx", "container_id": "c0ffee",
      "image": "nginx:1.27",
      "labels": {"io.kubernetes.pod.name": "web-0",
                 "io.kubernetes.pod.namespace": "default",
                 "io.kubernetes.pod.uid": "u-1",
                 "io.kubernetes.container.name": "nginx"},
      "annotations": {"a": "b"}, "future_field": 1
    }"#;

    #[test]
    fn a_record_becomes_container_metadata() {
        let d = scratch("meta");
        std::fs::write(d.join("wab12-3.json"), CONTAINER).unwrap();
        let recs = WorkloadRecords::new(d.to_str().unwrap());
        assert!(recs.available());
        let meta = recs.lookup("/stormpump/wab12-3").expect("record").into_meta();
        assert_eq!(meta.id, "c0ffee");
        assert_eq!(meta.namespace, "stormpump");
        assert_eq!(meta.aliases, ["c0ffee"]);
        assert_eq!(meta.image, "nginx:1.27");
        assert_eq!(meta.init_pid, Some(4242));
        assert!(!meta.reports_network);
        // The three labels rustkube's metrics.k8s.io reads (rustkube#89).
        assert_eq!(meta.labels["io.kubernetes.pod.namespace"], "default");
        assert_eq!(meta.labels["io.kubernetes.pod.name"], "web-0");
        assert_eq!(meta.labels["io.kubernetes.container.name"], "nginx");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_sandbox_reports_the_pod_network() {
        let d = scratch("sbx");
        std::fs::write(
            d.join("wab12-1.json"),
            r#"{"cgroup":"/stormpump/wab12-1","pid":7,"kind":"sandbox","reports_network":true,
                "labels":{"io.cri-containerd.kind":"sandbox","io.kubernetes.container.name":"POD"}}"#,
        )
        .unwrap();
        let meta = WorkloadRecords::new(d.to_str().unwrap())
            .lookup("/stormpump/wab12-1")
            .unwrap()
            .into_meta();
        assert!(meta.reports_network);
        assert_eq!(meta.init_pid, Some(7));
        assert!(meta.aliases.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_the_named_cgroup_matches() {
        let d = scratch("match");
        std::fs::write(d.join("wab12-3.json"), CONTAINER).unwrap();
        std::fs::write(d.join("bad.json"), "{not json").unwrap();
        let recs = WorkloadRecords::new(d.to_str().unwrap());
        // Same basename under another parent: not this record.
        assert!(recs.lookup("/other/wab12-3").is_none());
        assert!(recs.lookup("/stormpump/wab12-4").is_none());
        assert!(recs.lookup("/stormpump/bad").is_none());
        assert!(recs.lookup("/").is_none());
        assert!(recs.lookup("/stormpump/..").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_directory_is_not_an_error() {
        let recs = WorkloadRecords::new("/nonexistent/cadvisor/workloads");
        assert!(!recs.available());
        assert!(recs.lookup("/stormpump/w1-1").is_none());
        assert_eq!(WorkloadRecords::new("").dirs, [PathBuf::from(HOST_DIR), PathBuf::from(HOSTRUN_DIR)]);
    }
}
