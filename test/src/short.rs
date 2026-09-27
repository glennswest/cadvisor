//! `short` (< 2 min): cadvisor is up on the node and does its main job — it
//! sees a new container, reports what that container uses, and lets it go
//! when it is deleted.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::cad::{cpu_cores, enc};
use crate::checks;
use crate::ctx::{mib, Ctx, Work};
use crate::env::Env;
use crate::expo;
use crate::report::{Outcome, Report};
use crate::workload::Load;

/// The workload: a quarter of a core and 64 MiB.
const CPU_MILLIS: u32 = 250;
const MEM_MIB: u32 = 64;

pub async fn run(env: &Env, r: &mut Report) {
    let ctx = Ctx::setup(env).await;
    checks::all(env, &ctx.cad, r).await;
    workload(env, &ctx, r).await;
}

/// One workload pod, seen, measured and let go.
pub async fn workload(env: &Env, ctx: &Ctx, r: &mut Report) {
    let names = ["workload-discovered", "workload-stats", "workload-removed", "cleanup"];
    if let Err(why) = &ctx.pods {
        for n in names {
            r.record(n, why.outcome(), 0, None);
        }
        return;
    }
    let t = Instant::now();
    let before: HashSet<String> = match ctx.cad.names().await {
        Ok(n) => n,
        Err(e) => {
            r.record(names[0], Outcome::Fail(e), 0, None);
            return;
        }
    };
    let load = Load { cpu_millis: CPU_MILLIS, mem_mib: MEM_MIB, secs: env.remaining().as_secs() as u32 + 60, oom_after: None, limit_mib: None };
    let run_wait = Duration::from_secs(60).min(env.remaining().saturating_sub(Duration::from_secs(40)));
    let works = ctx.launch(env, &[("a".into(), load)], &before, run_wait, Duration::from_secs(20)).await;
    let w: Work = match works {
        Ok(mut v) => v.remove(0),
        Err(e) => {
            r.record(names[0], Outcome::Infra(e), t.elapsed().as_millis(), None);
            cleanup(ctx, r).await;
            return;
        }
    };
    let discovered = match (&w.error, &w.container) {
        (Some(e), _) => Outcome::Infra(e.clone()),
        (None, None) => Outcome::Fail(format!("pod {} is running, and cadvisor lists no new container running {}", w.pod, w.marker)),
        (None, Some(c)) => Outcome::Pass(format!("pod {} is container {c}, found {} ms after it was running", w.pod, w.discovery().unwrap_or_default().as_millis())),
    };
    let ok = r.record(names[0], discovered, t.elapsed().as_millis(), None);
    if !ok {
        r.record(names[1], Outcome::Skip("no container to measure".into()), 0, None);
        r.record(names[2], Outcome::Skip("no container to watch go".into()), 0, None);
        cleanup(ctx, r).await;
        return;
    }
    let name = w.container.clone().unwrap();

    let t = Instant::now();
    let o = stats(ctx, &name).await;
    r.record(names[1], o, t.elapsed().as_millis(), None);

    let t = Instant::now();
    let o = removed(env, ctx, &w, &name).await;
    r.record(names[2], o, t.elapsed().as_millis(), None);
    cleanup(ctx, r).await;
}

/// What cadvisor reports for the workload is what it does, within what a
/// busy machine allows: CPU between 40% and 240% of the quarter core (a
/// scheduling hiccup on either side), memory at least the 64 MiB it wrote.
async fn stats(ctx: &Ctx, name: &str) -> Outcome {
    let s = match ctx.cad.stats_over(name, Duration::from_secs(5), Duration::from_secs(20)).await {
        Ok(s) => s,
        Err(e) => return Outcome::Fail(e),
    };
    let cores = cpu_cores(&s).unwrap_or(0.0);
    let want = CPU_MILLIS as f64 / 1000.0;
    if cores < want * 0.4 || cores > want * 2.4 {
        return Outcome::Fail(format!("{name}: {cores:.3} cores over {} samples, the workload burns {want}", s.len()));
    }
    let ws = s.last().map(|x| x.working_set).unwrap_or(0);
    if ws < mib(MEM_MIB as u64) {
        return Outcome::Fail(format!("{name}: working set {} MiB, the workload holds {MEM_MIB} MiB", ws >> 20));
    }
    let e = match ctx.cad.metrics().await.and_then(|(t, _)| expo::parse(&t)) {
        Ok(e) => e,
        Err(e) => return Outcome::Fail(e),
    };
    if e.find("container_memory_working_set_bytes", "id", name).next().is_none() {
        return Outcome::Fail(format!("/metrics has no container_memory_working_set_bytes{{id={name:?}}}"));
    }
    Outcome::Pass(format!("{name}: {cores:.3} cores (burns {want}), working set {} MiB (holds {MEM_MIB}), in /metrics", ws >> 20))
}

/// Delete the pod; cadvisor stops listing its container.
async fn removed(env: &Env, ctx: &Ctx, w: &Work, name: &str) -> Outcome {
    let (api, _) = ctx.pods.as_ref().expect("checked by the caller");
    if let Err(e) = api.delete_pod(&w.pod).await {
        return Outcome::Infra(e);
    }
    let t = Instant::now();
    let wait = Duration::from_secs(75).min(env.remaining().saturating_sub(Duration::from_secs(5)));
    match ctx.cad.gone(&[name.to_string()], Instant::now() + wait).await {
        Ok(left) if left.is_empty() => {}
        Ok(_) => return Outcome::Fail(format!("{name} still listed {} s after its pod was deleted", wait.as_secs())),
        Err(e) => return Outcome::Fail(e),
    }
    match ctx.cad.get(&format!("/api/v2.0/stats{}", enc(name))).await {
        Ok(r) if r.status == 500 => Outcome::Pass(format!("{name} gone {} ms after the delete; its stats are a 500", t.elapsed().as_millis())),
        Ok(r) => Outcome::Fail(format!("{name} is gone from spec, but its stats answer {} {}", r.status, crate::cad::clip(&r.body))),
        Err(e) => Outcome::Fail(e),
    }
}

/// Delete every workload pod of the run, and check none is left.
pub async fn cleanup(ctx: &Ctx, r: &mut Report) {
    let t = Instant::now();
    let o = match &ctx.pods {
        Err(why) => why.outcome(),
        Ok((api, _)) => match api.cleanup().await {
            Err(e) => Outcome::Infra(e),
            Ok(n) => {
                let mut left = 0;
                for _ in 0..20 {
                    left = api.leftovers().await.unwrap_or(usize::MAX);
                    if left == 0 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                if left == 0 { Outcome::Pass(format!("deleted {n} workload pods; none left")) } else { Outcome::Fail(format!("{left} workload pods still listed")) }
            }
        },
    };
    r.record("cleanup", o, t.elapsed().as_millis(), None);
}
