//! The checks that need no workload, shared by `short` and `medium`: health,
//! stormd's view, version, machine, `/metrics` and housekeeping.

use std::time::Duration;

use crate::cad::{self, Cad};
use crate::env::Env;
use crate::expo;
use crate::report::{Outcome, Report};

/// Run one check, timed, and record it.
pub async fn check(r: &mut Report, name: &str, f: impl std::future::Future<Output = Outcome>) -> bool {
    r.run(name, f).await
}

pub async fn all(env: &Env, cad: &Cad, r: &mut Report) {
    check(r, "health", health(cad)).await;
    check(r, "stormd-supervises", stormd(env)).await;
    check(r, "version", version(cad)).await;
    check(r, "machine", machine(cad)).await;
    check(r, "metrics", metrics(cad)).await;
    check(r, "housekeeping", housekeeping(cad)).await;
}

/// `/healthz`, `/-/healthy`, `/-/ready`: `200 ok`.
pub async fn health(cad: &Cad) -> Outcome {
    for p in ["/healthz", "/-/healthy", "/-/ready"] {
        match cad.get(p).await {
            Ok(r) if r.status == 200 && r.body.trim() == "ok" => {}
            Ok(r) => return Outcome::Fail(format!("{p}: {} {:?}", r.status, cad::clip(&r.body))),
            Err(e) => return Outcome::Fail(e),
        }
    }
    Outcome::Pass("/healthz, /-/healthy and /-/ready answer 200 ok".into())
}

/// stormd reports the cadvisor process running.
pub async fn stormd(env: &Env) -> Outcome {
    let Some(addr) = &env.stormd else {
        return Outcome::Skip("no stormd for cadvisor here (CADVISOR_TEST_STORMD=none)".into());
    };
    match cad::stormd(addr).await {
        Ok(s) if s.running => Outcome::Pass(format!("stormd at {addr}: running, {} restarts, {} crashes", s.restarts, s.crashes)),
        Ok(s) => Outcome::Fail(format!("stormd at {addr} does not report cadvisor running ({s:?})")),
        Err(e) => Outcome::Skip(format!("stormd not reachable, so cadvisor is not under stormd here: {e}")),
    }
}

/// `/api/v2.0/version` is a non-empty string, and `/metrics` says the same.
pub async fn version(cad: &Cad) -> Outcome {
    let v = match cad.json("/api/v2.0/version").await {
        Ok(v) => v,
        Err(e) => return Outcome::Fail(e),
    };
    let Some(ver) = v.as_str().filter(|s| !s.is_empty()) else {
        return Outcome::Fail(format!("/api/v2.0/version is {v}, not a version string"));
    };
    let e = match cad.metrics().await.and_then(|(t, _)| expo::parse(&t)) {
        Ok(e) => e,
        Err(e) => return Outcome::Fail(e),
    };
    if e.find("cadvisor_version_info", "cadvisorVersion", ver).next().is_none() {
        return Outcome::Fail(format!("/metrics has no cadvisor_version_info{{cadvisorVersion={ver:?}}}"));
    }
    Outcome::Pass(format!("cadvisor {ver}"))
}

/// `/api/v2.0/machine` has cores and memory, and `/metrics` agrees.
pub async fn machine(cad: &Cad) -> Outcome {
    let m = match cad.json("/api/v2.0/machine").await {
        Ok(v) => v,
        Err(e) => return Outcome::Fail(e),
    };
    let cores = m["num_cores"].as_u64().unwrap_or(0);
    let mem = m["memory_capacity"].as_u64().unwrap_or(0);
    if cores == 0 || mem == 0 {
        return Outcome::Fail(format!("machine: num_cores {cores}, memory_capacity {mem}"));
    }
    let e = match cad.metrics().await.and_then(|(t, _)| expo::parse(&t)) {
        Ok(e) => e,
        Err(e) => return Outcome::Fail(e),
    };
    let first = |n: &str| e.samples.iter().find(|s| s.name == n).map(|s| s.value);
    if first("machine_cpu_cores") != Some(cores as f64) {
        return Outcome::Fail(format!("machine_cpu_cores {:?}, the API says {cores}", first("machine_cpu_cores")));
    }
    if first("machine_memory_bytes") != Some(mem as f64) {
        return Outcome::Fail(format!("machine_memory_bytes {:?}, the API says {mem}", first("machine_memory_bytes")));
    }
    Outcome::Pass(format!("{cores} cores, {} MiB", mem >> 20))
}

/// `/metrics` parses strictly, has the root container's core series, and
/// timestamps its container samples as v0.49.2 does.
pub async fn metrics(cad: &Cad) -> Outcome {
    let (text, dt) = match cad.metrics().await {
        Ok(x) => x,
        Err(e) => return Outcome::Fail(e),
    };
    let e = match expo::parse(&text) {
        Ok(e) => e,
        Err(e) => return Outcome::Fail(format!("/metrics does not parse: {e}")),
    };
    for f in ["container_cpu_usage_seconds_total", "container_memory_working_set_bytes", "container_last_seen", "container_start_time_seconds"] {
        if e.find(f, "id", "/").next().is_none() {
            return Outcome::Fail(format!("/metrics has no {f}{{id=\"/\"}}"));
        }
    }
    // Stats series carry the sample's time; spec series and
    // container_scrape_error do not, in v0.49.2 either.
    let stat = |n: &str| matches!(n, "container_cpu_usage_seconds_total" | "container_memory_working_set_bytes" | "container_memory_usage_bytes");
    if let Some(s) = e.samples.iter().find(|s| stat(&s.name) && s.ts.is_none()) {
        return Outcome::Fail(format!("{} {:?} has no timestamp", s.name, s.labels));
    }
    Outcome::Pass(format!("{} families, {} samples, {} ms", e.families(), e.samples.len(), dt.as_millis()))
}

/// The root container's samples keep coming: timestamps rise, CPU never
/// goes backwards.
pub async fn housekeeping(cad: &Cad) -> Outcome {
    let a = match cad.stats("/", 64).await {
        Ok(s) => s,
        Err(e) => return Outcome::Fail(e),
    };
    tokio::time::sleep(Duration::from_secs(3)).await;
    let b = match cad.stats("/", 64).await {
        Ok(s) => s,
        Err(e) => return Outcome::Fail(e),
    };
    if b.len() < 2 {
        return Outcome::Fail(format!("{} samples of / after 3 s", b.len()));
    }
    if b.last().map(|s| s.ts) <= a.last().map(|s| s.ts) {
        return Outcome::Fail("no new sample of / in 3 s".into());
    }
    if let Some(w) = b.windows(2).find(|w| w[1].ts <= w[0].ts || w[1].cpu_total < w[0].cpu_total) {
        return Outcome::Fail(format!("samples out of order or CPU going backwards: {:?} then {:?}", w[0], w[1]));
    }
    Outcome::Pass(format!("{} samples of /, newest {} ms after the earlier read's", b.len(), (b.last().unwrap().ts - a.last().unwrap().ts) / 1_000_000))
}
