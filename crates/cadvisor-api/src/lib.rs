//! The cAdvisor REST API: `/api/<version>/<requestType>[/<args...>]`.
//!
//! Replicates upstream's routing and error contract exactly: plain-text 500
//! bodies for lookup failures (`failed to get container "/x" with error: ...`),
//! 400 only for the two "supported ..." listing responses, JSON 200 otherwise.
//!
//! v1.0–v1.3 are handled in `v1`; v2.0/v2.1 in `v2`, dispatched from the same
//! `/api/{*rest}` route. `docker`-namespace lookups are answered from
//! containerd/CRI-O metadata (upstream needs dockershim for these).

#[cfg(target_os = "linux")]
mod common;
#[cfg(target_os = "linux")]
mod v1;
#[cfg(target_os = "linux")]
mod v2;

#[cfg(target_os = "linux")]
pub use v1::router;
