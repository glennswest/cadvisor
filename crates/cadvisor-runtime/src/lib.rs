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
}
