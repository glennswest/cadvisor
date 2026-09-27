//! cadvisor's test container (stormcentral `docs/test-standard.md`).
//!
//! What cadvisor does on a node is watch every cgroup and report what each
//! one uses, over `/metrics` and its REST API at `STORM_NODE:9096`. The
//! suites test exactly that, from a pod, through its HTTP interfaces and the
//! apiserver:
//!
//! - `short` (< 2 min): health, stormd's view, version, machine, `/metrics`,
//!   housekeeping, and one workload pod seen, measured and let go.
//! - `medium` (< 30 min): every endpoint and its error contract, events
//!   (history and stream), OOM kills, accuracy against a known load, many
//!   containers at once, concurrent scrapes, no restart under stormd.
//! - `long` (the night window): waves of workload containers sized from the
//!   machine, measured for slowdown and residue across waves.
//!
//! Workloads are this same image, run in pods in the run's namespace as
//! `/test workload <marker> …` (see [`workload`]). stormpump names pod
//! cgroups opaquely, so a workload is found through cadvisor itself: the new
//! container whose `ps` shows its marker.

pub mod cad;
pub mod checks;
pub mod ctx;
pub mod env;
pub mod expo;
pub mod gotime;
pub mod k8s;
pub mod long;
pub mod medium;
pub mod report;
pub mod short;
pub mod workload;

use env::Env;
use report::{Outcome, Report};

/// Run the suite `env.suite` names, recording into `r`.
pub async fn run(env: &Env, r: &mut Report) {
    if let Some(o) = unreachable(env).await {
        r.record("cadvisor-up", o, 0, None);
        return;
    }
    match env.suite.as_str() {
        // Boxed: each suite is one large future.
        "short" => Box::pin(short::run(env, r)).await,
        "medium" => Box::pin(medium::run(env, r)).await,
        "long" => Box::pin(long::run(env, r)).await,
        other => {
            r.record("suite", Outcome::Infra(format!("suite {other:?} is not short, medium or long")), 0, None);
        }
    }
}

/// `None` when cadvisor answers. When it does not: a failure if its stormd
/// answers (cadvisor is down on a node that runs it), else could-not-run
/// (the node, or the path to it, is down). stormcos starts cadvisor on every
/// node, so neither is a configuration to skip.
async fn unreachable(env: &Env) -> Option<Outcome> {
    let cad = cad::Cad::new(&env.cadvisor);
    let mut last = String::new();
    for _ in 0..5 {
        match cad.get("/healthz").await {
            Ok(_) => return None,
            Err(e) => last = e,
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    if let Some(sd) = &env.stormd {
        if cad::stormd(sd).await.is_ok() || tcp(sd).await {
            return Some(Outcome::Fail(format!("cadvisor at {} does not answer, and its stormd at {sd} does: {last}", env.cadvisor)));
        }
    }
    Some(Outcome::Infra(format!("neither cadvisor at {} nor its stormd answers: {last}", env.cadvisor)))
}

async fn tcp(addr: &str) -> bool {
    matches!(
        tokio::time::timeout(std::time::Duration::from_secs(5), tokio::net::TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}
