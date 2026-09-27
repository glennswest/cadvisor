//! `medium` (< 30 min): every endpoint cadvisor serves and its error
//! contract, events (history and stream), OOM kills, the accuracy of what it
//! reports against a known load, many containers at once, concurrent scrapes,
//! and that stormd saw no restart while all that happened.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cad::{self, cpu_cores, enc, Cad};
use crate::checks::{self, check};
use crate::ctx::{mib, Ctx};
use crate::env::Env;
use crate::expo;
use crate::report::{Outcome, Report};
use crate::short;
use crate::workload::{Load, NO_LIMIT};

pub async fn run(env: &Env, r: &mut Report) {
    let ctx = Ctx::setup(env).await;
    let stormd_before = match &env.stormd {
        Some(a) => cad::stormd(a).await.ok(),
        None => None,
    };
    checks::all(env, &ctx.cad, r).await;
    check(r, "api-listings", listings(&ctx.cad)).await;
    check(r, "v1-endpoints", v1(&ctx.cad)).await;
    check(r, "v2-endpoints", v2(&ctx.cad)).await;
    check(r, "error-contract", errors(env, &ctx.cad)).await;
    check(r, "metrics-concurrent", concurrent(&ctx.cad)).await;
    with_pods(r, &ctx, "events", events(env, &ctx)).await;
    with_pods(r, &ctx, "oom", oom(env, &ctx)).await;
    with_pods(r, &ctx, "accuracy", accuracy(env, &ctx)).await;
    with_pods(r, &ctx, "many-containers", many(env, &ctx)).await;
    check(r, "stormd-steady", steady(env, stormd_before)).await;
    short::cleanup(&ctx, r).await;
}

/// A test that starts workloads, or the reason it cannot here.
async fn with_pods(r: &mut Report, ctx: &Ctx, name: &str, f: impl std::future::Future<Output = Outcome>) {
    match &ctx.pods {
        Err(why) => {
            r.record(name, why.outcome(), 0, None);
        }
        Ok(_) => {
            r.run(name, f).await;
        }
    }
}

/// `GET path` must answer `status` with a text/plain body containing `want`.
async fn expect_text(cad: &Cad, path: &str, status: u16, want: &str) -> Result<(), String> {
    let r = cad.get(path).await?;
    if r.status != status || !r.body.contains(want) {
        return Err(format!("{path}: {} {:?}, want {status} containing {want:?}", r.status, cad::clip(&r.body)));
    }
    if !r.content_type.starts_with("text/plain") {
        return Err(format!("{path}: content-type {:?}, want text/plain", r.content_type));
    }
    Ok(())
}

/// The two "Supported …" listings are `400`s, as upstream.
async fn listings(cad: &Cad) -> Outcome {
    let cases = [
        ("/api", "Supported API versions: v1.0,v1.1,v1.2,v1.3,v2.0,v2.1"),
        ("/api/", "Supported API versions: v1.0,v1.1,v1.2,v1.3,v2.0,v2.1"),
        ("/api/v1.3/", "Supported request types: \"containers\",\"docker\",\"events\",\"machine\",\"subcontainers\""),
        ("/api/v2.1/", "Supported request types: \"attributes\",\"events\""),
    ];
    for (p, want) in cases {
        if let Err(e) = expect_text(cad, p, 400, want).await {
            return Outcome::Fail(e);
        }
    }
    Outcome::Pass(format!("{} listings", cases.len()))
}

/// Every v1.x request type answers JSON of the right shape.
async fn v1(cad: &Cad) -> Outcome {
    let res: Result<(), String> = async {
        let m = cad.json("/api/v1.0/machine").await?;
        if m["num_cores"].as_u64().unwrap_or(0) == 0 {
            return Err("v1.0/machine has no num_cores".into());
        }
        for v in ["v1.0", "v1.1", "v1.2", "v1.3"] {
            let c = cad.json(&format!("/api/{v}/containers/")).await?;
            if c["name"] != "/" {
                return Err(format!("{v}/containers/ is named {}", c["name"]));
            }
        }
        let s = cad.json("/api/v1.1/subcontainers/").await?;
        let names: Vec<&str> = s.as_array().ok_or("v1.1/subcontainers is not a list")?.iter().filter_map(|c| c["name"].as_str()).collect();
        if !names.contains(&"/") || names.len() < 2 {
            return Err(format!("v1.1/subcontainers/ lists {} containers, with / {}", names.len(), names.contains(&"/")));
        }
        if !cad.json("/api/v1.2/docker/").await?.is_object() {
            return Err("v1.2/docker/ is not an object".into());
        }
        if !cad.json("/api/v1.3/events/?all_events=true").await?.is_array() {
            return Err("v1.3/events/ is not a list".into());
        }
        Ok(())
    }
    .await;
    match res {
        Ok(()) => Outcome::Pass("machine, containers (v1.0–v1.3), subcontainers, docker, events".into()),
        Err(e) => Outcome::Fail(e),
    }
}

/// Every v2.x request type answers JSON of the right shape.
async fn v2(cad: &Cad) -> Outcome {
    let res: Result<(), String> = async {
        let ck = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_string()) };
        ck(cad.json("/api/v2.0/version").await?.is_string(), "v2.0/version is not a string")?;
        ck(cad.json("/api/v2.0/machine").await?["num_cores"].as_u64().unwrap_or(0) > 0, "v2.0/machine has no num_cores")?;
        ck(cad.json("/api/v2.0/attributes").await?["num_cores"].as_u64().unwrap_or(0) > 0, "v2.0/attributes has no num_cores")?;
        ck(cad.json("/api/v2.0/stats/").await?.get("/").is_some(), "v2.0/stats/ has no /")?;
        let s21 = cad.json("/api/v2.1/stats/?recursive=true").await?;
        ck(s21.is_object() && s21.get("/").is_none(), "v2.1/stats/ should leave / to machinestats")?;
        ck(cad.json("/api/v2.0/spec/").await?.get("/").is_some(), "v2.0/spec/ has no /")?;
        ck(cad.json("/api/v2.0/summary/").await?.get("/").is_some(), "v2.0/summary/ has no /")?;
        ck(cad.json("/api/v2.0/ps/").await?.is_array(), "v2.0/ps/ is not a list")?;
        ck(cad.json("/api/v2.0/storage").await?.as_array().is_some_and(|a| !a.is_empty()), "v2.0/storage lists no filesystem")?;
        ck(cad.json("/api/v2.0/events/?all_events=true").await?.is_array(), "v2.0/events/ is not a list")?;
        ck(cad.json("/api/v2.0/appmetrics/").await? == serde_json::json!({}), "v2.0/appmetrics/ is not {}")?;
        ck(cad.json("/api/v2.1/machinestats").await?.as_array().is_some_and(|a| !a.is_empty()), "v2.1/machinestats is empty")?;
        Ok(())
    }
    .await;
    match res {
        Ok(()) => Outcome::Pass("version, machine, attributes, stats (2.0, 2.1), spec, summary, ps, storage, events, appmetrics, machinestats".into()),
        Err(e) => Outcome::Fail(e),
    }
}

/// Lookup failures and bad requests are plain-text `500`s worded as upstream.
async fn errors(env: &Env, cad: &Cad) -> Outcome {
    let none = format!("/cadvt-none-{}", env.slug());
    let cases = [
        ("/api/v9.9/machine".to_string(), "unsupported API version \"v9.9\"".to_string()),
        (format!("/api/v1.0/containers{none}"), format!("failed to get container \"{none}\" with error:")),
        (format!("/api/v1.1/subcontainers{none}"), format!("failed to get subcontainers for container \"{none}\"")),
        (format!("/api/v2.0/stats{none}"), format!("could not get stats for \"{none}\"")),
        (format!("/api/v2.0/spec{none}"), format!("could not get spec for \"{none}\"")),
        ("/api/v1.0/subcontainers/".into(), "unknown request type \"subcontainers\"".into()),
        ("/api/v1.3/bogus".into(), "unknown request type \"bogus\"".into()),
        ("/api/v2.0/bogus".into(), "unknown request type \"bogus\"".into()),
        ("/api/v2.0/stats/?type=bogus".into(), "unknown 'type' \"bogus\"".into()),
        ("/api/v2.0/stats/?count=abc".into(), "invalid 'count' option".into()),
    ];
    for (p, want) in &cases {
        if let Err(e) = expect_text(cad, p, 500, want).await {
            return Outcome::Fail(e);
        }
    }
    // Still up and answering after all of that.
    match checks::health(cad).await {
        Outcome::Pass(_) => Outcome::Pass(format!("{} error cases worded as upstream; still healthy", cases.len())),
        other => other,
    }
}

/// Sixteen scrapes at once all succeed and all parse.
async fn concurrent(cad: &Cad) -> Outcome {
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let c = cad.clone();
        tasks.push(tokio::spawn(async move {
            let (t, dt) = c.metrics().await?;
            expo::parse(&t)?;
            Ok::<Duration, String>(dt)
        }));
    }
    let mut times = Vec::new();
    for t in tasks {
        match t.await {
            Ok(Ok(dt)) => times.push(dt),
            Ok(Err(e)) => return Outcome::Fail(e),
            Err(e) => return Outcome::Fail(format!("scrape task: {e}")),
        }
    }
    times.sort();
    Outcome::Pass(format!("16 concurrent scrapes: median {} ms, max {} ms", times[8].as_millis(), times[15].as_millis()))
}

fn named<'a>(name: &'a str, kind: &'a str) -> impl Fn(&Value) -> bool + 'a {
    move |e: &Value| e["container_name"] == name && e["event_type"] == kind
}

/// A new container's creation and deletion arrive on an open stream and are
/// kept in the history.
async fn events(env: &Env, ctx: &Ctx) -> Outcome {
    let kinds = "creation_events=true&deletion_events=true";
    let mut stream = match ctx.cad.stream(kinds).await {
        Ok(s) => s,
        Err(e) => return Outcome::Fail(e),
    };
    let before = match ctx.cad.names().await {
        Ok(b) => b,
        Err(e) => return Outcome::Fail(e),
    };
    let works = match ctx.launch(env, &[("ev".into(), Load::idle(900))], &before, Duration::from_secs(120), Duration::from_secs(30)).await {
        Ok(w) => w,
        Err(e) => return Outcome::Infra(e),
    };
    let w = &works[0];
    if let Some(e) = &w.error {
        return Outcome::Infra(e.clone());
    }
    let Some(name) = w.container.clone() else {
        return Outcome::Fail(format!("pod {} never appeared in cadvisor", w.pod));
    };
    let mut buf = Vec::new();
    if let Err(e) = cad::next_event(&mut stream, &mut buf, Duration::from_secs(30), named(&name, "containerCreation")).await {
        return Outcome::Fail(format!("stream, creation of {name}: {e}"));
    }
    match ctx.cad.events("creation_events=true").await {
        Ok(h) if h.iter().any(named(&name, "containerCreation")) => {}
        Ok(h) => return Outcome::Fail(format!("history ({} events) has no creation of {name}", h.len())),
        Err(e) => return Outcome::Fail(e),
    }
    let (api, _) = ctx.pods.as_ref().expect("checked by with_pods");
    if let Err(e) = api.delete_pod(&w.pod).await {
        return Outcome::Infra(e);
    }
    if let Err(e) = cad::next_event(&mut stream, &mut buf, Duration::from_secs(90), named(&name, "containerDeletion")).await {
        return Outcome::Fail(format!("stream, deletion of {name}: {e}"));
    }
    match ctx.cad.events("deletion_events=true").await {
        Ok(h) if h.iter().any(named(&name, "containerDeletion")) => Outcome::Pass(format!("{name}: creation and deletion, streamed and in the history")),
        Ok(h) => Outcome::Fail(format!("history ({} events) has no deletion of {name}", h.len())),
        Err(e) => Outcome::Fail(e),
    }
}

/// A child OOM-killed under the pod's memory limit is an `oom` and an
/// `oomKill` event for the container, and counts in
/// `container_oom_events_total`. Skip when the runtime applies no limit.
async fn oom(env: &Env, ctx: &Ctx) -> Outcome {
    const LIMIT: u32 = 48;
    let before = match ctx.cad.names().await {
        Ok(b) => b,
        Err(e) => return Outcome::Fail(e),
    };
    let load = Load { cpu_millis: 0, mem_mib: 0, secs: 900, oom_after: Some(20), limit_mib: Some(LIMIT) };
    let works = match ctx.launch(env, &[("oom".into(), load)], &before, Duration::from_secs(120), Duration::from_secs(18)).await {
        Ok(w) => w,
        Err(e) => return Outcome::Infra(e),
    };
    let w = &works[0];
    if let Some(e) = &w.error {
        return Outcome::Infra(e.clone());
    }
    let Some(name) = w.container.clone() else {
        return Outcome::Fail(format!("pod {} never appeared in cadvisor", w.pod));
    };
    let limit = match ctx.cad.json(&format!("/api/v2.0/spec{}", enc(&name))).await {
        Ok(v) => v[&name]["memory"]["limit"].as_u64(),
        Err(e) => return Outcome::Fail(e),
    };
    let (api, _) = ctx.pods.as_ref().expect("checked by with_pods");
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if let Ok(Some((_, code))) = api.terminated(&w.pod).await {
            if code == NO_LIMIT as i64 {
                return Outcome::Skip(format!(
                    "the runtime applied no memory limit ({LIMIT}Mi asked; cadvisor's spec says {limit:?}): nothing to OOM"
                ));
            }
            return Outcome::Infra(format!("the workload ended with {code} before an OOM was seen"));
        }
        let got = match ctx.cad.events("oom_events=true&oom_kill_events=true").await {
            Ok(h) => h,
            Err(e) => return Outcome::Fail(e),
        };
        let oom = got.iter().any(named(&name, "oom"));
        let kill = got.iter().any(named(&name, "oomKill"));
        if oom && kill {
            if limit != Some(mib(LIMIT as u64)) {
                return Outcome::Fail(format!("{name}: OOM seen, but the spec's memory limit is {limit:?}, the pod's is {LIMIT} MiB"));
            }
            let e = match ctx.cad.metrics().await.and_then(|(t, _)| expo::parse(&t)) {
                Ok(e) => e,
                Err(e) => return Outcome::Fail(e),
            };
            let n = e.find("container_oom_events_total", "id", &name).next().map(|s| s.value).unwrap_or(0.0);
            if n < 1.0 {
                return Outcome::Fail(format!("{name}: oom events in the API, container_oom_events_total is {n}"));
            }
            return Outcome::Pass(format!("{name}: memory limit {LIMIT} MiB in the spec; oom + oomKill events; container_oom_events_total {n}"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Outcome::Fail(format!("{name}: no oom/oomKill event within 90 s of the workload starting to allocate past its {LIMIT} MiB limit"))
}

/// Against a known load — half a core and 128 MiB — the API and `/metrics`
/// agree with each other and with the load.
async fn accuracy(env: &Env, ctx: &Ctx) -> Outcome {
    let cores = match ctx.cad.json("/api/v2.0/machine").await {
        Ok(m) => m["num_cores"].as_u64().unwrap_or(0),
        Err(e) => return Outcome::Fail(e),
    };
    if cores < 2 {
        return Outcome::Skip(format!("requires min-cores: 2 (half a core of load must not be the whole machine); this one has {cores}"));
    }
    let before = match ctx.cad.names().await {
        Ok(b) => b,
        Err(e) => return Outcome::Fail(e),
    };
    let load = Load { cpu_millis: 500, mem_mib: 128, secs: 900, oom_after: None, limit_mib: None };
    let works = match ctx.launch(env, &[("acc".into(), load)], &before, Duration::from_secs(120), Duration::from_secs(30)).await {
        Ok(w) => w,
        Err(e) => return Outcome::Infra(e),
    };
    let w = &works[0];
    if let Some(e) = &w.error {
        return Outcome::Infra(e.clone());
    }
    let Some(name) = w.container.clone() else {
        return Outcome::Fail(format!("pod {} never appeared in cadvisor", w.pod));
    };
    tokio::time::sleep(Duration::from_secs(5)).await;
    let s = match ctx.cad.stats_over(&name, Duration::from_secs(15), Duration::from_secs(40)).await {
        Ok(s) => s,
        Err(e) => return Outcome::Fail(e),
    };
    let used = cpu_cores(&s).unwrap_or(0.0);
    if !(0.3..=0.75).contains(&used) {
        return Outcome::Fail(format!("{name}: {used:.3} cores over {} samples; the load is 0.5", s.len()));
    }
    let ws = s.last().unwrap().working_set;
    if ws < mib(128) || ws > mib(128 + 64) {
        return Outcome::Fail(format!("{name}: working set {} MiB; the load holds 128 MiB", ws >> 20));
    }
    // /metrics between two API reads of the same counter.
    let a = ctx.cad.stats(&name, 1).await.ok().and_then(|v| v.last().cloned());
    let m = ctx.cad.metrics().await.and_then(|(t, _)| expo::parse(&t));
    let b = ctx.cad.stats(&name, 1).await.ok().and_then(|v| v.last().cloned());
    let (Some(a), Ok(m), Some(b)) = (a, m, b) else {
        return Outcome::Fail(format!("{name}: could not read stats and /metrics back to back"));
    };
    let Some(sec) = m.find("container_cpu_usage_seconds_total", "id", &name).find(|s| s.label("cpu") == Some("total")).map(|s| s.value) else {
        return Outcome::Fail(format!("/metrics has no container_cpu_usage_seconds_total{{id={name:?},cpu=\"total\"}}"));
    };
    let ns = sec * 1e9;
    if ns < a.cpu_total as f64 * 0.999 || ns > b.cpu_total as f64 * 1.001 {
        return Outcome::Fail(format!("{name}: /metrics cpu {ns:.0} ns is outside the API's {}..{} ns", a.cpu_total, b.cpu_total));
    }
    let v1 = match ctx.cad.json(&format!("/api/v1.3/containers{}", enc(&name))).await {
        Ok(v) => v,
        Err(e) => return Outcome::Fail(e),
    };
    if v1["name"] != name.as_str() || v1["spec"]["has_memory"] != true {
        return Outcome::Fail(format!("v1.3/containers{name}: name {} has_memory {}", v1["name"], v1["spec"]["has_memory"]));
    }
    Outcome::Pass(format!("{name}: {used:.3} cores (load 0.5), working set {} MiB (load 128), /metrics agrees with the API", ws >> 20))
}

/// Many containers at once — one per core, 4 to 16 — are all found and all
/// let go.
async fn many(env: &Env, ctx: &Ctx) -> Outcome {
    let cores = match ctx.cad.json("/api/v2.0/machine").await {
        Ok(m) => m["num_cores"].as_u64().unwrap_or(4),
        Err(e) => return Outcome::Fail(e),
    };
    let n = cores.clamp(4, 16) as usize;
    let before = match ctx.cad.names().await {
        Ok(b) => b,
        Err(e) => return Outcome::Fail(e),
    };
    let loads: Vec<(String, Load)> = (0..n).map(|i| (format!("m{i}"), Load { cpu_millis: 0, mem_mib: 16, secs: 900, oom_after: None, limit_mib: None })).collect();
    let works = match ctx.launch(env, &loads, &before, Duration::from_secs(240), Duration::from_secs(60)).await {
        Ok(w) => w,
        Err(e) => return Outcome::Infra(e),
    };
    if let Some(w) = works.iter().find(|w| w.error.is_some()) {
        return Outcome::Infra(format!("{} of {n} pods did not run; first: {}", works.iter().filter(|w| w.error.is_some()).count(), w.error.as_ref().unwrap()));
    }
    let missing: Vec<&str> = works.iter().filter(|w| w.container.is_none()).map(|w| w.pod.as_str()).collect();
    if !missing.is_empty() {
        return Outcome::Fail(format!("{} of {n} running pods not in cadvisor after 60 s: {missing:?}", missing.len()));
    }
    let names: Vec<String> = works.iter().filter_map(|w| w.container.clone()).collect();
    if names.iter().collect::<HashSet<_>>().len() != n {
        return Outcome::Fail(format!("{n} pods mapped to fewer distinct containers: {names:?}"));
    }
    let slowest = works.iter().filter_map(|w| w.discovery()).max().unwrap_or_default();
    let (api, _) = ctx.pods.as_ref().expect("checked by with_pods");
    for w in &works {
        if let Err(e) = api.delete_pod(&w.pod).await {
            return Outcome::Infra(e);
        }
    }
    let t = Instant::now();
    match ctx.cad.gone(&names, Instant::now() + Duration::from_secs(90)).await {
        Ok(left) if left.is_empty() => {}
        Ok(left) => return Outcome::Fail(format!("{} of {n} containers still listed 90 s after their pods were deleted: {left:?}", left.len())),
        Err(e) => return Outcome::Fail(e),
    }
    Outcome::Pass(format!("{n} containers found (slowest {} ms after running), all gone {} ms after the deletes", slowest.as_millis(), t.elapsed().as_millis()))
}

/// stormd counted no restart or crash of cadvisor during the suite.
async fn steady(env: &Env, before: Option<cad::Supervised>) -> Outcome {
    let (Some(addr), Some(b)) = (&env.stormd, before) else {
        return Outcome::Skip("stormd was not reachable at the start".into());
    };
    match cad::stormd(addr).await {
        Ok(a) if a.running && a.restarts == b.restarts && a.crashes == b.crashes => Outcome::Pass(format!("running; restarts {} and crashes {} unchanged", a.restarts, a.crashes)),
        Ok(a) => Outcome::Fail(format!("before {b:?}, after {a:?}")),
        Err(e) => Outcome::Fail(e),
    }
}
