//! Prometheus `/metrics` exposition, byte-compatible with cadvisor v0.49.2's
//! default-flag output (family names, HELP/TYPE text, label sets, Go float
//! formatting, per-sample timestamps).
//!
//! No metrics-registry crate: one pass over the manager's containers renders
//! straight into a reused buffer. The metric groups rendered are cpu, cpuLoad,
//! memory, disk, diskIO, network and oom_event; the binary refuses
//! `-enable_metrics` for any other group (#38).

pub mod encode;
pub mod families;

#[cfg(target_os = "linux")]
mod collect;

#[cfg(target_os = "linux")]
pub use collect::{MetricsOpts, render, router};
