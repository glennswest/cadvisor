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
    /// The apiserver's bearer and CA: the Job's ServiceAccount.
    pub token: Option<String>,
    pub ca: Option<Vec<u8>>,
    /// How cadvisor is reached; see [`CadAuth`].
    pub cad_auth: CadAuth,
    pub timeout: Duration,
    pub started: Instant,
    /// `CADVISOR_TEST_NO_PODS=1`: report the tests that start workload pods
    /// as skip. Only the hermetic harness sets it; it has no apiserver.
    pub no_pods: bool,
}

/// How the suites reach cadvisor. On a stormcos node since stormcos#143,
/// cadvisor serves https with a node-CA pair (`/data/stormcert/cadvisor.crt`)
/// and `--auth-mode review`: every path but the health ones needs a bearer
/// whose ServiceAccount may `get nodes/metrics`.
#[derive(Debug, Clone, PartialEq)]
pub struct CadAuth {
    /// `CADVISOR_TEST_SCHEME`: `https` (the default, as the golden runs) or
    /// `http` (an RPM host with no TLS flags).
    pub scheme: String,
    /// `CADVISOR_TEST_CA`: a PEM file, else the Job's ServiceAccount
    /// `ca.crt`, the node CA that signs cadvisor's certificate. With none,
    /// the client's built-in roots.
    pub ca: Option<Vec<u8>>,
    /// `CADVISOR_TEST_TOKEN_FILE`: the bearer's file, else the Job's
    /// ServiceAccount token; `none` sends no bearer.
    pub token: Option<String>,
    /// What went wrong reading them: an unknown scheme, an unreadable file.
    pub error: Option<String>,
}

impl Env {
    pub fn read(suite_arg: Option<String>) -> Env {
        Env::from_vars(suite_arg, |k| std::env::var(k).ok(), SA_DIR)
    }

    /// [`Env::read`] from any variables and ServiceAccount directory, so a
    /// test can build one without touching the process environment.
    pub fn from_vars(suite_arg: Option<String>, get: impl Fn(&str) -> Option<String>, sa_dir: &str) -> Env {
        let var = |k: &str| get(k).filter(|v| !v.is_empty());
        let secs = |k: &str, d: u64| Duration::from_secs(var(k).and_then(|v| v.parse().ok()).unwrap_or(d));
        let sa = |f: &str| std::fs::read_to_string(format!("{sa_dir}/{f}")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let sa_ca = std::fs::read(format!("{sa_dir}/ca.crt")).ok();
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
            cad_auth: cad_auth(&var, sa("token"), sa_ca.clone()),
            token: sa("token"),
            ca: sa_ca,
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

    /// cadvisor's base URL, `scheme://host:port`.
    pub fn cadvisor_url(&self) -> String {
        format!("{}://{}", self.cad_auth.scheme, self.cadvisor)
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

fn cad_auth(var: &impl Fn(&str) -> Option<String>, sa_token: Option<String>, sa_ca: Option<Vec<u8>>) -> CadAuth {
    let mut errors = Vec::new();
    let scheme = var("CADVISOR_TEST_SCHEME").unwrap_or_else(|| "https".into()).to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        errors.push(format!("CADVISOR_TEST_SCHEME={scheme:?} is not http or https"));
    }
    let ca = match var("CADVISOR_TEST_CA") {
        Some(f) => std::fs::read(&f).map_err(|e| errors.push(format!("CADVISOR_TEST_CA={f}: {e}"))).ok(),
        None => sa_ca,
    };
    let token = match var("CADVISOR_TEST_TOKEN_FILE").as_deref() {
        Some("none") => None,
        Some(f) => match std::fs::read_to_string(f) {
            Ok(t) if !t.trim().is_empty() => Some(t.trim().to_string()),
            Ok(_) => {
                errors.push(format!("CADVISOR_TEST_TOKEN_FILE={f} is empty"));
                None
            }
            Err(e) => {
                errors.push(format!("CADVISOR_TEST_TOKEN_FILE={f}: {e}"));
                None
            }
        },
        None => sa_token,
    };
    CadAuth { scheme, ca, token, error: (!errors.is_empty()).then(|| errors.join("; ")) }
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
            cad_auth: CadAuth { scheme: "http".into(), ca: None, token: None, error: None },
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

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cadvt-env-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn read(vars: &[(&str, &str)], sa: &std::path::Path) -> Env {
        let vars: std::collections::HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Env::from_vars(None, |k| vars.get(k).cloned(), sa.to_str().unwrap())
    }

    #[test]
    fn cadvisor_is_https_with_the_service_accounts_token_and_ca_by_default() {
        let sa = scratch("sa");
        std::fs::write(sa.join("token"), "sa-token-EXAMPLE\n").unwrap(); // notsecret
        std::fs::write(sa.join("ca.crt"), "node-ca").unwrap();
        let e = read(&[("STORM_NODE", "10.0.0.5")], &sa);
        assert_eq!(e.cadvisor_url(), "https://10.0.0.5:9096");
        assert_eq!(e.cad_auth.token.as_deref(), Some("sa-token-EXAMPLE")); // notsecret
        assert_eq!(e.cad_auth.ca.as_deref(), Some(&b"node-ca"[..]));
        assert_eq!(e.cad_auth.error, None);
    }

    #[test]
    fn cadvisors_scheme_token_and_ca_can_be_named() {
        let sa = scratch("named");
        std::fs::write(sa.join("token"), "sa-token-EXAMPLE").unwrap(); // notsecret
        let (tok, ca) = (sa.join("cad.token"), sa.join("cad-ca.pem"));
        std::fs::write(&tok, " other-EXAMPLE \n").unwrap(); // notsecret
        std::fs::write(&ca, "other-ca").unwrap();
        let e = read(
            &[("STORM_NODE", "n"), ("CADVISOR_TEST_SCHEME", "HTTP"), ("CADVISOR_TEST_TOKEN_FILE", tok.to_str().unwrap()), ("CADVISOR_TEST_CA", ca.to_str().unwrap())],
            &sa,
        );
        assert_eq!(e.cadvisor_url(), "http://n:9096");
        assert_eq!(e.cad_auth.token.as_deref(), Some("other-EXAMPLE")); // notsecret
        assert_eq!(e.cad_auth.ca.as_deref(), Some(&b"other-ca"[..]));
        // The apiserver keeps the ServiceAccount's.
        assert_eq!(e.token.as_deref(), Some("sa-token-EXAMPLE")); // notsecret
        let e = read(&[("CADVISOR_TEST_TOKEN_FILE", "none")], &sa);
        assert_eq!(e.cad_auth.token, None);
    }

    #[test]
    fn a_bad_scheme_or_a_missing_file_is_said() {
        let sa = scratch("bad");
        let e = read(&[("CADVISOR_TEST_SCHEME", "ftp"), ("CADVISOR_TEST_TOKEN_FILE", "/nonexistent/t"), ("CADVISOR_TEST_CA", "/nonexistent/ca")], &sa);
        let err = e.cad_auth.error.unwrap();
        for want in ["CADVISOR_TEST_SCHEME=\"ftp\"", "CADVISOR_TEST_TOKEN_FILE=/nonexistent/t", "CADVISOR_TEST_CA=/nonexistent/ca"] {
            assert!(err.contains(want), "{err}");
        }
    }
}
