//! `long` (the night window): waves of workload containers at the machine's
//! capacity, measured across waves for what the standard asks — start
//! (here: discovery) latency per wave, and what is left behind.
//!
//! A wave: ramp to a size read from the machine (cadvisor's own
//! `/api/v2.0/machine`: the runner's Role reads no nodes), find every
//! container in cadvisor, hold while scraping `/metrics`, drain, and check
//! cadvisor let every container go. Measured per wave: slowest discovery,
//! mean scrape time, drain time, containers left over, and cadvisor's own
//! resident memory and open fds (from `/api/v2.0/ps` of its container).
//! A wave slower than the first, or residue that grows, fails the trend
//! even when every wave passed.
//!
//! VM waves are not run here: cadvisor sees VMs as cgroups like any other,
//! and starting VMs is stormvm's suite's job.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::cad::Cad;
use crate::ctx::{mib, Ctx};
use crate::env::Env;
use crate::expo;
use crate::report::{Outcome, Report};
use crate::short;
use crate::workload::Load;

/// Per-pod load: a little CPU and memory, so the containers have stats that
/// move, and a wave is sized by count rather than by what each one burns.
const POD_MILLIS: u32 = 20;
const POD_MIB: u32 = 16;
/// Time kept back for the last drain and the cleanup.
const RESERVE: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Default)]
pub struct Wave {
    pub n: usize,
    pub pods: usize,
    pub discover_ms: u128,
    pub scrape_ms: u128,
    pub drain_ms: u128,
    pub residue: usize,
    pub rss: Option<u64>,
    pub fds: Option<u64>,
}

pub async fn run(env: &Env, r: &mut Report) {
    let ctx = Ctx::setup(env).await;
    if let Err(why) = &ctx.pods {
        r.record("waves", why.outcome(), 0, None);
        return;
    }
    r.record("vm-waves", Outcome::Skip("VM waves belong to stormvm's suite; cadvisor sees VMs as cgroups, which the container waves cover".into()), 0, None);
    let machine = match ctx.cad.json("/api/v2.0/machine").await {
        Ok(m) => m,
        Err(e) => {
            r.record("waves", Outcome::Fail(e), 0, None);
            return;
        }
    };
    let cores = machine["num_cores"].as_u64().unwrap_or(1);
    let mem = machine["memory_capacity"].as_u64().unwrap_or(mib(1024));
    let full = size(cores, mem);
    let me = find_self(&ctx.cad).await;
    let baseline = ctx.cad.names().await.map(|n| n.len()).unwrap_or(0);

    let mut waves: Vec<Wave> = Vec::new();
    // Vary the size: full, half, three quarters, full, …
    let mix = [1.0, 0.5, 0.75, 1.0];
    let mut i = 0;
    while env.remaining() > RESERVE {
        let n = ((full as f64 * mix[i % mix.len()]) as usize).max(2);
        let t = Instant::now();
        let (o, w) = wave(env, &ctx, i + 1, n, me.as_deref(), baseline).await;
        let extra = format!(
            "\"wave\": {}, \"pods\": {}, \"discover_ms\": {}, \"scrape_ms\": {}, \"drain_ms\": {}, \"residue\": {}, \"cadvisor_rss\": {}, \"cadvisor_fds\": {}",
            w.n, w.pods, w.discover_ms, w.scrape_ms, w.drain_ms, w.residue, opt(w.rss), opt(w.fds)
        );
        let failed = matches!(o, Outcome::Fail(_) | Outcome::Infra(_));
        r.record(&format!("wave-{}", i + 1), o, t.elapsed().as_millis(), Some(&extra));
        waves.push(w);
        i += 1;
        if failed {
            // One cleanup between waves so a stuck wave does not poison the next.
            if let Ok((api, _)) = &ctx.pods {
                let _ = api.cleanup().await;
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    }
    let t = Instant::now();
    let o = trend(&waves);
    r.record("trend", o, t.elapsed().as_millis(), Some(&format!("\"waves\": {}, \"full_wave\": {full}", waves.len())));
    short::cleanup(&ctx, r).await;
}

/// The full wave for a machine: two containers per core, at most 1/4 of
/// memory, between 4 and 128.
pub fn size(cores: u64, mem: u64) -> usize {
    let by_mem = (mem / 4 / mib(POD_MIB as u64 + 16)) as usize;
    (cores as usize * 2).min(by_mem).clamp(4, 128)
}

fn opt(v: Option<u64>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "null".into())
}

async fn wave(env: &Env, ctx: &Ctx, n: usize, pods: usize, me: Option<&str>, baseline: usize) -> (Outcome, Wave) {
    let mut w = Wave { n, pods, ..Wave::default() };
    let before: HashSet<String> = match ctx.cad.names().await {
        Ok(b) => b,
        Err(e) => return (Outcome::Fail(e), w),
    };
    let secs = (env.remaining().as_secs() as u32).max(600);
    let loads: Vec<(String, Load)> = (0..pods)
        .map(|k| (format!("v{n}-{k}"), Load { cpu_millis: POD_MILLIS, mem_mib: POD_MIB, secs, oom_after: None, limit_mib: None }))
        .collect();
    let works = match ctx.launch(env, &loads, &before, Duration::from_secs(600), Duration::from_secs(120)).await {
        Ok(x) => x,
        Err(e) => return (Outcome::Infra(e), w),
    };
    let (api, _) = ctx.pods.as_ref().expect("checked by run");
    let not_running = works.iter().filter(|x| x.error.is_some()).count();
    let names: Vec<String> = works.iter().filter_map(|x| x.container.clone()).collect();
    w.discover_ms = works.iter().filter_map(|x| x.discovery()).max().unwrap_or_default().as_millis();

    // Hold: scrape a few times, every container in every scrape.
    let mut scrape = Vec::new();
    let mut hold_err = None;
    for _ in 0..6 {
        match ctx.cad.metrics().await.and_then(|(t, dt)| Ok((expo::parse(&t)?, dt))) {
            Ok((e, dt)) => {
                scrape.push(dt.as_millis());
                if let Some(m) = names.iter().find(|c| e.find("container_cpu_usage_seconds_total", "id", c).next().is_none()) {
                    hold_err.get_or_insert(format!("{m} missing from /metrics while its pod runs"));
                }
            }
            Err(e) => {
                hold_err.get_or_insert(e);
            }
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
    w.scrape_ms = if scrape.is_empty() { 0 } else { scrape.iter().sum::<u128>() / scrape.len() as u128 };

    // Drain.
    for x in &works {
        let _ = api.delete_pod(&x.pod).await;
    }
    let t = Instant::now();
    let left = ctx.cad.gone(&names, Instant::now() + Duration::from_secs(180)).await.unwrap_or_else(|_| names.clone());
    w.drain_ms = t.elapsed().as_millis();
    // Let the API side finish too, then count what cadvisor still lists.
    for _ in 0..60 {
        if api.leftovers().await.map(|l| l == 0).unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    w.residue = ctx.cad.names().await.map(|now| now.len().saturating_sub(baseline)).unwrap_or(0);
    if let Some(c) = me {
        if let Ok(p) = ctx.cad.ps(c).await {
            if let Some(p) = p.iter().find(|p| is_cadvisor(p["cmd"].as_str().unwrap_or(""))) {
                w.rss = p["rss"].as_u64();
                w.fds = p["fd_count"].as_u64();
            }
        }
    }

    let o = if not_running > 0 {
        Outcome::Infra(format!("{not_running} of {pods} pods did not run"))
    } else if names.len() < pods {
        Outcome::Fail(format!("{} of {pods} running pods not found in cadvisor within 120 s", pods - names.len()))
    } else if let Some(e) = hold_err {
        Outcome::Fail(e)
    } else if !left.is_empty() {
        Outcome::Fail(format!("{} containers still listed 180 s after the drain: {:?}", left.len(), &left[..left.len().min(5)]))
    } else {
        Outcome::Pass(format!(
            "{pods} containers: slowest found {} ms after running; scrape {} ms; drained in {} ms",
            w.discover_ms, w.scrape_ms, w.drain_ms
        ))
    };
    (o, w)
}

fn is_cadvisor(cmd: &str) -> bool {
    cmd.split_whitespace().next().is_some_and(|a| a.ends_with("/cadvisor") || a == "cadvisor")
}

/// cadvisor's own container: the one whose `ps` shows the cadvisor binary.
/// Looked for once; `None` (no rss/fd trend) if it is not visible.
async fn find_self(cad: &Cad) -> Option<String> {
    let names = cad.names().await.ok()?;
    let mut v: Vec<String> = names.into_iter().collect();
    v.sort();
    for n in v {
        if let Ok(p) = cad.ps(&n).await {
            if p.iter().any(|p| is_cadvisor(p["cmd"].as_str().unwrap_or(""))) {
                return Some(n);
            }
        }
    }
    None
}

/// Across waves: a full-size wave may not be twice as slow as the first to
/// discover (above a 5 s floor) or to scrape (above 50 ms); residue may not
/// stay above zero for two waves running; cadvisor's memory may not grow by
/// half (and 32 MiB) or its fds by 64 from the first wave.
pub fn trend(waves: &[Wave]) -> Outcome {
    let Some(first) = waves.first() else {
        return Outcome::Infra("no wave ran: the window was shorter than one wave plus the reserve".into());
    };
    let mut bad = Vec::new();
    for w in waves.iter().skip(1) {
        let same = w.pods >= first.pods;
        if same && w.discover_ms > 5000 && w.discover_ms > first.discover_ms * 2 {
            bad.push(format!("wave {}: discovery {} ms vs {} ms in wave 1", w.n, w.discover_ms, first.discover_ms));
        }
        if same && w.scrape_ms > 50 && w.scrape_ms > first.scrape_ms * 2 {
            bad.push(format!("wave {}: scrape {} ms vs {} ms in wave 1", w.n, w.scrape_ms, first.scrape_ms));
        }
        if let (Some(a), Some(b)) = (first.rss, w.rss) {
            if b > a + a / 2 && b > a + mib(32) {
                bad.push(format!("wave {}: cadvisor rss {} MiB vs {} MiB after wave 1", w.n, b >> 20, a >> 20));
            }
        }
        if let (Some(a), Some(b)) = (first.fds, w.fds) {
            if b > a + 64 {
                bad.push(format!("wave {}: cadvisor fds {b} vs {a} after wave 1", w.n));
            }
        }
    }
    if let Some(w) = waves.windows(2).find(|p| p[0].residue > 0 && p[1].residue > 0) {
        bad.push(format!("waves {} and {}: {} and {} containers left over", w[0].n, w[1].n, w[0].residue, w[1].residue));
    }
    if bad.is_empty() {
        Outcome::Pass(format!("{} waves, no slowdown or residue trend", waves.len()))
    } else {
        Outcome::Fail(format!("first regression: {}", bad.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(n: usize, pods: usize, discover: u128, scrape: u128, residue: usize, rss: u64) -> Wave {
        Wave { n, pods, discover_ms: discover, scrape_ms: scrape, drain_ms: 0, residue, rss: Some(mib(rss)), fds: Some(20) }
    }

    #[test]
    fn waves_are_sized_from_the_machine() {
        assert_eq!(size(1, mib(512)), 4);
        assert_eq!(size(8, mib(64 * 1024)), 16);
        assert_eq!(size(64, mib(512 * 1024)), 128);
        assert_eq!(size(32, mib(1024)), 8);
    }

    #[test]
    fn a_steady_night_passes() {
        let ws = [w(1, 16, 900, 12, 0, 40), w(2, 8, 700, 9, 0, 41), w(3, 16, 1100, 14, 1, 42), w(4, 16, 950, 13, 0, 42)];
        assert!(matches!(trend(&ws), Outcome::Pass(_)));
    }

    #[test]
    fn slowdown_and_residue_and_growth_fail_the_trend() {
        let slow = [w(1, 16, 900, 12, 0, 40), w(2, 16, 9000, 12, 0, 40)];
        assert!(matches!(trend(&slow), Outcome::Fail(d) if d.contains("discovery")));
        let left = [w(1, 16, 900, 12, 0, 40), w(2, 16, 900, 12, 3, 40), w(3, 16, 900, 12, 3, 40)];
        assert!(matches!(trend(&left), Outcome::Fail(d) if d.contains("left over")));
        let leak = [w(1, 16, 900, 12, 0, 40), w(2, 16, 900, 12, 0, 120)];
        assert!(matches!(trend(&leak), Outcome::Fail(d) if d.contains("rss")));
        assert!(matches!(trend(&[]), Outcome::Infra(_)));
    }
}
