//! Wire types for the cAdvisor-compatible API surface.
//!
//! **Pinned to the cadvisor v0.49.2 wire format** (the conformance target and
//! what deployments run). Fields added on upstream master after v0.49.2 (PSI,
//! CFS burst, memory.stat v2 extras, health, spec start_time, Percentiles
//! std/count) are NOT omitempty upstream, so carrying them would break
//! byte-compatibility with v0.49.2 — they are deliberately absent here.
//!
//! Every struct mirrors a Go struct from google/cadvisor v0.49.2 (`info/v1`,
//! `info/v2`) field-for-field, in declaration order, with the exact
//! JSON key from the Go `json:` tag — including upstream's typos
//! (`"app direct_mode_capacity"`, `"containter_inode_usage"`, the v2
//! `MachineFsStats` `"inline"` key) — so serialized output is byte-compatible.
//!
//! Go serialization semantics replicated here:
//! - `omitempty` on scalars/strings/slices/maps → `skip_serializing_if`;
//!   `omitempty` on struct-typed fields is a Go no-op, so those always serialize.
//! - Untagged Go fields (TcpStat, UdpStat, TcpAdvancedStat) keep Go's exported
//!   field names as JSON keys.
//! - Maps are `BTreeMap` because Go's encoding/json sorts map keys.
//! - `time.Time` → [`GoTime`]: RFC3339 with trailing-zero-trimmed nanoseconds,
//!   zero value `"0001-01-01T00:00:00Z"`.
//!
//! Not implemented (deferred): protobuf forms; the deprecated v1 `accelerators`
//! collectors never populate data, but the wire fields exist.

#[cfg(test)]
mod golden;
pub mod gotime;
mod omit;
pub mod v1;
pub mod v2;

pub use gotime::GoTime;
