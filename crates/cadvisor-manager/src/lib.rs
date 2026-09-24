//! Container registry, in-memory stats ring buffer, adaptive housekeeping,
//! and cgroup discovery.
//!
//! Platform-neutral pieces ([`store`], [`derive`]) are tested everywhere; the
//! [`manager`] itself drives the Linux data plane and is
//! `cfg(target_os = "linux")`.
//!
//! The manager also enriches cgroups with containerd/CRI-O metadata, attributes
//! pod network stats to the sandbox container, and records creation, deletion
//! and OOM events (from `memory.events`). `/summary` percentiles are computed
//! in `cadvisor-api` from this crate's stored samples.

pub mod derive;
pub mod store;

#[cfg(target_os = "linux")]
pub mod manager;

pub use store::TimedStore;

#[cfg(target_os = "linux")]
pub use manager::{Manager, ManagerConfig, ManagerError};
