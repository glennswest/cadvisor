//! What every suite sets up once: cadvisor's client, and, for the tests that
//! start workload pods, the apiserver and this Job's own pod (whose image and
//! node the workloads reuse).

use std::collections::HashSet;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use crate::cad::Cad;
use crate::env::Env;
use crate::k8s::{Api, Me};
use crate::report::Outcome;
use crate::workload::Load;

pub struct Ctx {
    pub cad: Cad,
    /// The apiserver and this pod, or why workload tests cannot run here.
    pub pods: Result<(Api, Me), Why>,
}

/// Why workload tests do not run: `Skip` is by configuration, `Infra` means
/// they should have and could not.
#[derive(Debug, Clone)]
pub enum Why {
    Skip(String),
    Infra(String),
}

impl Why {
    pub fn outcome(&self) -> Outcome {
        match self {
            Why::Skip(s) => Outcome::Skip(s.clone()),
            Why::Infra(s) => Outcome::Infra(s.clone()),
        }
    }
}

/// One workload pod and where cadvisor showed it.
#[derive(Debug, Clone)]
pub struct Work {
    pub pod: String,
    pub marker: String,
    pub running_at: Option<Instant>,
    pub container: Option<String>,
    pub found_at: Option<Instant>,
    /// Why it did not start, when it did not.
    pub error: Option<String>,
}

impl Work {
    /// From Running (as the API showed it) to found in cadvisor.
    pub fn discovery(&self) -> Option<Duration> {
        Some(self.found_at?.saturating_duration_since(self.running_at?))
    }
}

impl Ctx {
    pub async fn setup(env: &Env) -> Ctx {
        let cad = Cad::new(&env.cadvisor);
        if env.no_pods {
            return Ctx { cad, pods: Err(Why::Skip("CADVISOR_TEST_NO_PODS=1: no apiserver to start workloads through".into())) };
        }
        let pods = async {
            let api = Api::new(env).map_err(Why::Infra)?;
            let me = api.me(&env.pod).await.map_err(|e| Why::Infra(format!("reading this Job's pod: {e}")))?;
            // Workloads run on this pod's node; the cadvisor under test is
            // STORM_NODE's. When both are addresses and differ, what the
            // suites would see is another node's cadvisor.
            if let (Some(host), Ok(node)) = (&me.host_ip, env.node.parse::<IpAddr>()) {
                if host.parse::<IpAddr>().ok() != Some(node) {
                    return Err(Why::Infra(format!(
                        "this Job runs on {host}, not on STORM_NODE {node}: workload pods would not be on the node whose cadvisor is under test"
                    )));
                }
            }
            Ok((api, me))
        }
        .await;
        Ctx { cad, pods }
    }

    /// A pod name for this run: `w-<slug>-<what>`, a DNS label.
    pub fn pod_name(env: &Env, what: &str) -> String {
        let n = format!("w-{}-{what}", env.slug());
        n[..n.len().min(63)].trim_end_matches('-').to_string()
    }

    /// Start workload pods, wait for them to run, and find each one in
    /// cadvisor among the containers not in `before`. Errors only when the
    /// API refuses; a pod that does not start or is not found is reported in
    /// its `Work`.
    pub async fn launch(&self, env: &Env, loads: &[(String, Load)], before: &HashSet<String>, run_wait: Duration, find_wait: Duration) -> Result<Vec<Work>, String> {
        let (api, me) = self.pods.as_ref().map_err(|w| format!("{w:?}"))?;
        let mut works = Vec::new();
        for (what, load) in loads {
            let pod = Ctx::pod_name(env, what);
            let marker = env.marker(what);
            api.create_pod(&api.workload_pod(&pod, me, &marker, load)).await?;
            works.push(Work { pod, marker, running_at: None, container: None, found_at: None, error: None });
        }
        let deadline = Instant::now() + run_wait;
        for w in works.iter_mut() {
            match api.running(&w.pod, deadline).await {
                Ok(t) => w.running_at = Some(t),
                Err(e) => w.error = Some(e),
            }
        }
        let markers: Vec<String> = works.iter().filter(|w| w.error.is_none()).map(|w| w.marker.clone()).collect();
        if !markers.is_empty() {
            let found = self.cad.find(before, &markers, Instant::now() + find_wait).await;
            for w in works.iter_mut() {
                if let Some((name, at)) = found.get(&w.marker) {
                    w.container = Some(name.clone());
                    w.found_at = Some(*at);
                }
            }
        }
        Ok(works)
    }
}

/// `n` MiB in bytes.
pub const fn mib(n: u64) -> u64 {
    n << 20
}
