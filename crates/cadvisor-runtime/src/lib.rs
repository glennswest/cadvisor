//! Container runtime metadata clients.
//!
//! - containerd: gRPC over its unix socket (`containers.v1` for metadata,
//!   `tasks.v1` for the init PID), default namespace `k8s.io`.
//! - CRI-O: HTTP/1 over its unix socket (`GET /info`, `GET /containers/<id>`).
//!
//! Both return a runtime-agnostic [`ContainerMeta`] the manager's factory
//! chain uses to enrich raw cgroups. Missing sockets degrade gracefully.

use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("container not found")]
    NotFound,
    #[error("runtime unavailable: {0}")]
    Unavailable(String),
    #[error("{0}")]
    Other(String),
}

/// Runtime-agnostic container metadata.
#[derive(Debug, Clone, Default)]
pub struct ContainerMeta {
    pub id: String,
    /// Runtime namespace tag ("containerd" or "crio").
    pub namespace: String,
    pub aliases: Vec<String>,
    pub image: String,
    pub labels: BTreeMap<String, String>,
    pub init_pid: Option<u32>,
    /// Whether this container owns the pod network (sandbox/pause container).
    pub reports_network: bool,
    /// Writable-layer directory for filesystem usage accounting (CRI-O
    /// overlay: storage root with /merged -> /diff).
    pub rootfs_diff: Option<String>,
    /// The container's environment, `KEY=value` (containerd: the OCI spec's
    /// `process.env`; CRI-O's inspect carries none). Filtered by the
    /// manager against `-env_metadata_whitelist`.
    pub env: Vec<String>,
}

/// Upstream's `-env_metadata_whitelist` rule: keep each `KEY=value` whose
/// key starts with one of the (non-empty) whitelist entries.
pub fn whitelisted_env(env: &[String], whitelist: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for prefix in whitelist.iter().filter(|w| !w.is_empty()) {
        for var in env {
            if let Some((k, v)) = var.split_once('=') {
                if k.starts_with(prefix.as_str()) {
                    out.insert(k.to_string(), v.to_string());
                }
            }
        }
    }
    out
}

/// `process.env` from an OCI runtime spec (JSON).
pub fn oci_spec_env(spec_json: &[u8]) -> Vec<String> {
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct Process {
        env: Vec<String>,
    }
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct Spec {
        process: Process,
    }
    serde_json::from_slice::<Spec>(spec_json).map(|s| s.process.env).unwrap_or_default()
}

#[cfg(target_os = "linux")]
mod containerd;
#[cfg(target_os = "linux")]
mod crio;

#[cfg(target_os = "linux")]
pub use containerd::ContainerdClient;
#[cfg(target_os = "linux")]
pub use crio::CrioClient;

/// Extracts a 64-hex container id from a cgroup basename, upstream-style:
/// matches `<64hex>`, `cri-containerd-<64hex>.scope`, `crio-<64hex>.scope`,
/// `libpod-<64hex>.scope`, etc. Returns None for `.mount` units.
pub fn extract_container_id(basename: &str) -> Option<&str> {
    if basename.ends_with(".mount") {
        return None;
    }
    let bytes = basename.as_bytes();
    let is_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    let mut run_start = 0;
    let mut run_len = 0;
    for i in 0..=bytes.len() {
        if i < bytes.len() && is_hex(bytes[i]) {
            if run_len == 0 {
                run_start = i;
            }
            run_len += 1;
        } else {
            if run_len == 64 {
                return Some(&basename[run_start..run_start + 64]);
            }
            run_len = 0;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_extraction() {
        let id = "a".repeat(64);
        assert_eq!(extract_container_id(&id), Some(id.as_str()));
        assert_eq!(
            extract_container_id(&format!("cri-containerd-{id}.scope")),
            Some(id.as_str())
        );
        assert_eq!(extract_container_id(&format!("crio-{id}.scope")), Some(id.as_str()));
        assert_eq!(extract_container_id(&format!("libpod-{id}.scope")), Some(id.as_str()));
        assert_eq!(extract_container_id("system.slice"), None);
        assert_eq!(extract_container_id(&format!("{id}.mount")), None);
        assert_eq!(extract_container_id(&"a".repeat(63)), None);
        assert_eq!(extract_container_id(&"a".repeat(65)), None);
    }

    #[test]
    fn env_whitelist_is_a_prefix_match() {
        let env: Vec<String> =
            ["PATH=/usr/bin", "APP_MODE=prod", "APP_ID=7", "TZ=UTC", "EMPTY=", "NOEQ"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        let wl = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let got = whitelisted_env(&env, &wl(&["APP_", "TZ", "EMPTY", "NOEQ"]));
        let want: BTreeMap<String, String> =
            [("APP_ID", "7"), ("APP_MODE", "prod"), ("EMPTY", ""), ("TZ", "UTC")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        assert_eq!(got, want);
        assert!(whitelisted_env(&env, &wl(&[""])).is_empty());
        assert!(whitelisted_env(&env, &[]).is_empty());
    }

    #[test]
    fn oci_env() {
        let spec = br#"{"ociVersion":"1.1.0","process":{"args":["sh"],"env":["A=1","B=x=y"]}}"#;
        assert_eq!(oci_spec_env(spec), ["A=1", "B=x=y"]);
        assert!(oci_spec_env(b"{}").is_empty());
        assert!(oci_spec_env(b"not json").is_empty());
    }
}
