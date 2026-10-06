//! What the runner hands the container (stormcentral `docs/test-standard.md`),
//! and the few knobs cadvisor's suites add to it.

use std::time::{Duration, Instant};

/// Where the kubelet mounts the Job's ServiceAccount.
pub const SA_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// cadvisor's port on a stormcos node (`argv = ["--port", "9096", …]` in
/// cadvisor's entry in stormcentral's component registry).
pub const CADVISOR_PORT: u16 = 9096;

/// stormd's API in cadvisor's container: a service golden's port + 100.
pub const STORMD_PORT: u16 = CADVISOR_PORT + 100;

pub struct Env {
    pub suite: String,
    pub run_id: String,
    pub namespace: String,
    /// `STORM_API`: the apiserver the suites create workload pods through.
    pub api: String,
    /// `STORM_NODE`: the node under test.
    pub node: String,
    /// cadvisor, `host:port`. `CADVISOR_TEST_ADDR`, else `STORM_NODE:9096`.
    pub cadvisor: String,
    /// stormd of cadvisor's container, `host:port`. `CADVISOR_TEST_STORMD`,
    /// else `STORM_NODE:9196`; `none` when there is no stormd (an RPM host,
    /// the hermetic harness).
    pub stormd: Option<String>,
    /// This pod's name (`HOSTNAME`, which the kubelet sets to it).
    pub pod: String,
    pub token: Option<String>,
    pub ca: Option<Vec<u8>>,
    pub timeout: Duration,
    pub started: Instant,
    /// `CADVISOR_TEST_NO_PODS=1`: report the tests that start workload pods
    /// as skip. Only the hermetic harness sets it; it has no apiserver.
    pub no_pods: bool,
}

impl Env {
    pub fn read(suite_arg: Option<String>) -> Env {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let secs = |k: &str, d: u64| Duration::from_secs(var(k).and_then(|v| v.parse().ok()).unwrap_or(d));
        let sa = |f: &str| std::fs::read_to_string(format!("{SA_DIR}/{f}")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let suite = suite_arg.or_else(|| var("STORM_SUITE")).unwrap_or_else(|| "short".into());
        let node = var("STORM_NODE").unwrap_or_default();
        let budget = match suite.as_str() {
            "medium" => 1800,
            "long" => 8 * 3600,
            _ => 120,
        };
        Env {
            run_id: var("STORM_RUN_ID").unwrap_or_default(),
            namespace: var("STORM_NAMESPACE").or_else(|| sa("namespace")).unwrap_or_default(),
            api: var("STORM_API").unwrap_or_default(),
            cadvisor: var("CADVISOR_TEST_ADDR").unwrap_or_else(|| join(&node, CADVISOR_PORT)),
            stormd: match var("CADVISOR_TEST_STORMD").as_deref() {
                Some("none") => None,
                Some(s) => Some(s.to_string()),
                None => Some(join(&node, STORMD_PORT)),
            },
            pod: var("HOSTNAME").unwrap_or_default(),
            token: sa("token"),
            ca: std::fs::read(format!("{SA_DIR}/ca.crt")).ok(),
            timeout: secs("STORM_TIMEOUT", budget),
            started: Instant::now(),
            no_pods: var("CADVISOR_TEST_NO_PODS").as_deref() == Some("1"),
            suite,
            node,
        }
    }

    /// What the runner must have set and did not.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut m = Vec::new();
        if self.cadvisor.starts_with(':') {
            m.push("STORM_NODE");
        }
        if !self.no_pods {
            if self.api.is_empty() {
                m.push("STORM_API");
            }
            if self.namespace.is_empty() {
                m.push("STORM_NAMESPACE");
            }
        }
        if self.run_id.is_empty() {
            m.push("STORM_RUN_ID");
        }
        m
    }

    pub fn remaining(&self) -> Duration {
        self.timeout.saturating_sub(self.started.elapsed())
    }

    /// The run id as a DNS label, at most 30 characters: pod names and
    /// workload markers carry it, so two runs never collide.
    pub fn slug(&self) -> String {
        let s: String = self
            .run_id
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let s = s.trim_matches('-');
        let s = &s[..s.len().min(30)];
        let s = s.trim_end_matches('-');
        if s.is_empty() { "run".into() } else { s.to_string() }
    }

    /// A workload's marker: its own argv token, so cadvisor's `ps` finds its
    /// process and no other run's.
    pub fn marker(&self, what: &str) -> String {
        format!("cadvt-{}-{}", self.slug(), what)
    }
}

/// `host:port`, bracketing a bare IPv6 address.
pub fn join(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(run_id: &str) -> Env {
        Env {
            suite: "short".into(),
            run_id: run_id.into(),
            namespace: "ns".into(),
            api: "https://127.0.0.1:6443".into(),
            node: "127.0.0.1".into(),
            cadvisor: "127.0.0.1:9096".into(),
            stormd: None,
            pod: "p".into(),
            token: None,
            ca: None,
            timeout: Duration::from_secs(1),
            started: Instant::now(),
            no_pods: false,
        }
    }

    #[test]
    fn markers_are_dns_safe_and_per_run() {
        assert_eq!(env("Run_42/x").marker("a"), "cadvt-run-42-x-a");
        assert_eq!(env("--").slug(), "run");
        assert!(env(&"x".repeat(90)).slug().len() <= 30);
    }

    #[test]
    fn ipv6_nodes_are_bracketed() {
        assert_eq!(join("fd00::1", 9096), "[fd00::1]:9096");
        assert_eq!(join("10.0.0.1", 9196), "10.0.0.1:9196");
    }

    #[test]
    fn the_runner_must_name_the_node() {
        let mut e = env("r");
        assert!(e.missing().is_empty());
        e.cadvisor = ":9096".into();
        e.api.clear();
        assert_eq!(e.missing(), vec!["STORM_NODE", "STORM_API"]);
        e.no_pods = true;
        assert_eq!(e.missing(), vec!["STORM_NODE"]);
    }
}
