//! Container registry, in-memory stats ring buffer, adaptive housekeeping,
//! and cgroup discovery.
//!
//! Platform-neutral pieces ([`store`], [`derive`]) are tested everywhere; the
//! [`manager`] itself drives the Linux data plane and is
//! `cfg(target_os = "linux")`.
//!
//! Deferred: /summary percentile aggregation (M6), runtime (containerd/CRI-O)
//! metadata enrichment and per-pod network attribution (M7), OOM events (M8).

pub mod derive;
pub mod store;

#[cfg(target_os = "linux")]
pub mod manager;

pub use store::TimedStore;

#[cfg(target_os = "linux")]
pub use manager::{Manager, ManagerConfig, ManagerError};
