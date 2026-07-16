//! Linux data plane for cadvisor-rs: cgroup-v2 stat reading, procfs/sysfs
//! parsing, filesystem stats, machine info, and cgroup-tree watching.
//!
//! All parsers take `&str` and are testable on any platform (fixtures in
//! `fixtures/` were captured from a real Fedora 43 / cgroup-v2 host). The
//! file/syscall plumbing (`CgroupReader`, `FsService`, `machine_info`,
//! `watch`) is `cfg(target_os = "linux")`.
//!
//! Semantics replicate google/cadvisor v0.49.2 on cgroup v2:
//! - working_set = memory.current − inactive_file (clamped at 0)
//! - cache = memory.stat `file`, rss = `anon`, mapped_file = `file_mapped`
//! - max_usage = memory.peak, swap = memory.swap.current, kernel = 0
//! - pgfault/pgmajfault are reported in both container and hierarchical scope
//! - cpu.stat µs values ×1000 → ns; cpu.weight converted back to v1 shares

pub mod error;
pub mod parse;

#[cfg(target_os = "linux")]
pub mod cgroup;
#[cfg(target_os = "linux")]
pub mod fs;
#[cfg(target_os = "linux")]
pub mod machine;
#[cfg(target_os = "linux")]
pub mod watch;

pub use error::HostError;
