//! Prometheus `/metrics` exposition, byte-compatible with cadvisor v0.49.2's
//! default-flag output (family names, HELP/TYPE text, label sets, Go float
//! formatting, per-sample timestamps).
//!
//! No metrics-registry crate: one pass over the manager's containers renders
//! straight into a reused buffer. The metric groups rendered are cpu, cpuLoad,
//! memory, disk, diskIO, network, oom_event, percpu, sched, tcp, udp and
//! hugetlb (#40); the binary refuses `-enable_metrics` for any other group
//! (#38). A scraper whose `Accept` prefers it gets delimited protobuf (#40).

pub mod encode;
pub mod families;
pub mod proto;

#[cfg(target_os = "linux")]
mod collect;

#[cfg(target_os = "linux")]
pub use collect::{MetricsOpts, render, render_proto, router};
