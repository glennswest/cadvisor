//! cadvisor, as the suites see it: its REST API and `/metrics` over plain
//! HTTP at `STORM_NODE:9096`, and stormd's view of the process.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::gotime;

pub struct Resp {
    pub status: u16,
    pub content_type: String,
    pub body: String,
}

#[derive(Clone)]
pub struct Cad {
    pub addr: String,
    http: reqwest::Client,
}

impl Cad {
    pub fn new(addr: &str) -> Cad {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("an HTTP client with no TLS settings builds");
        Cad { addr: addr.to_string(), http }
    }

    /// Any path; the status is returned, not judged.
    pub async fn get(&self, path: &str) -> Result<Resp, String> {
        let url = format!("http://{}{path}", self.addr);
        let r = self.http.get(&url).send().await.map_err(|e| format!("GET {url}: {e}"))?;
        let status = r.status().as_u16();
        let content_type = r
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = r.text().await.map_err(|e| format!("GET {url}: body: {e}"))?;
        Ok(Resp { status, content_type, body })
    }

    /// A JSON `200`, parsed; anything else is an error naming what came back.
    pub async fn json(&self, path: &str) -> Result<Value, String> {
        let r = self.get(path).await?;
        if r.status != 200 {
            return Err(format!("GET {path}: {} {}", r.status, clip(&r.body)));
        }
        if !r.content_type.starts_with("application/json") {
            return Err(format!("GET {path}: content-type {:?}", r.content_type));
        }
        serde_json::from_str(&r.body).map_err(|e| format!("GET {path}: not JSON: {e}"))
    }

    pub async fn metrics(&self) -> Result<(String, Duration), String> {
        let t = Instant::now();
        let r = self.get("/metrics").await?;
        if r.status != 200 {
            return Err(format!("GET /metrics: {} {}", r.status, clip(&r.body)));
        }
        Ok((r.body, t.elapsed()))
    }

    /// Every container cadvisor knows, by name.
    pub async fn names(&self) -> Result<HashSet<String>, String> {
        let v = self.json("/api/v2.0/spec/?recursive=true").await?;
        Ok(v.as_object().ok_or("spec is not an object")?.keys().cloned().collect())
    }

    /// The processes cadvisor lists in one container (its own `cgroup.procs`).
    pub async fn ps(&self, name: &str) -> Result<Vec<Value>, String> {
        let v = self.json(&format!("/api/v2.0/ps{}", enc(name))).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// Up to `count` samples of one container, oldest first.
    pub async fn stats(&self, name: &str, count: u32) -> Result<Vec<Stat>, String> {
        let v = self.json(&format!("/api/v2.0/stats{}?count={count}", enc(name))).await?;
        let arr = v.get(name).and_then(Value::as_array).ok_or_else(|| format!("stats for {name} not in the answer"))?;
        let mut out: Vec<Stat> = arr.iter().filter_map(Stat::from_json).collect();
        out.sort_by_key(|s| s.ts);
        Ok(out)
    }

    /// Wait for a container's stats to span at least `span`, and return them.
    pub async fn stats_over(&self, name: &str, span: Duration, wait: Duration) -> Result<Vec<Stat>, String> {
        let t = Instant::now();
        loop {
            let s = self.stats(name, 64).await?;
            if s.len() >= 2 && (s[s.len() - 1].ts - s[0].ts) as u128 >= span.as_nanos() {
                return Ok(s);
            }
            if t.elapsed() >= wait {
                return Err(format!("{name}: {} samples spanning less than {} s after {} s", s.len(), span.as_secs(), wait.as_secs()));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Find the containers whose processes carry these markers (a workload's
    /// argv token), among containers not in `before`. Returns what was found
    /// by `deadline`, marker → (container name, when it was found).
    pub async fn find(&self, before: &HashSet<String>, markers: &[String], deadline: Instant) -> HashMap<String, (String, Instant)> {
        let mut found: HashMap<String, (String, Instant)> = HashMap::new();
        let mut claimed: HashSet<String> = HashSet::new();
        loop {
            if let Ok(names) = self.names().await {
                let mut fresh: Vec<&String> = names.iter().filter(|n| !before.contains(*n) && !claimed.contains(*n)).collect();
                fresh.sort();
                for name in fresh {
                    if found.len() == markers.len() {
                        break;
                    }
                    let Ok(procs) = self.ps(name).await else { continue };
                    for p in &procs {
                        let cmd = p["cmd"].as_str().unwrap_or("");
                        if let Some(m) = markers.iter().find(|m| !found.contains_key(*m) && carries(cmd, m)) {
                            found.insert(m.clone(), (name.clone(), Instant::now()));
                            claimed.insert(name.clone());
                            break;
                        }
                    }
                }
            }
            if found.len() == markers.len() || Instant::now() >= deadline {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Wait until none of `names` is a container any more. Returns those
    /// still listed at `deadline`.
    pub async fn gone(&self, names: &[String], deadline: Instant) -> Result<Vec<String>, String> {
        loop {
            let now = self.names().await?;
            let left: Vec<String> = names.iter().filter(|n| now.contains(*n)).cloned().collect();
            if left.is_empty() || Instant::now() >= deadline {
                return Ok(left);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Events of these kinds for `/` and everything under it.
    pub async fn events(&self, kinds: &str) -> Result<Vec<Value>, String> {
        let v = self.json(&format!("/api/v1.3/events/?{kinds}&subcontainers=true&max_events=100000")).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// Open the event stream; `next_event` reads from it.
    pub async fn stream(&self, kinds: &str) -> Result<reqwest::Response, String> {
        let url = format!("http://{}/api/v1.3/events/?{kinds}&subcontainers=true&stream=true", self.addr);
        let c = reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).build().map_err(|e| e.to_string())?;
        let r = c.get(&url).send().await.map_err(|e| format!("GET {url}: {e}"))?;
        if r.status().as_u16() != 200 {
            return Err(format!("GET {url}: {}", r.status()));
        }
        Ok(r)
    }
}

/// Read streamed events until one satisfies `want` or `wait` passes.
pub async fn next_event(r: &mut reqwest::Response, buf: &mut Vec<u8>, wait: Duration, want: impl Fn(&Value) -> bool) -> Result<Value, String> {
    let end = tokio::time::Instant::now() + wait;
    loop {
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            let text = String::from_utf8_lossy(&line);
            if text.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("streamed event is not JSON ({e}): {}", clip(&text)))?;
            if want(&v) {
                return Ok(v);
            }
        }
        match tokio::time::timeout_at(end, r.chunk()).await {
            Err(_) => return Err(format!("no matching event within {} s", wait.as_secs())),
            Ok(Err(e)) => return Err(format!("event stream: {e}")),
            Ok(Ok(None)) => return Err("the event stream ended".into()),
            Ok(Ok(Some(b))) => buf.extend_from_slice(&b),
        }
    }
}

/// Whether a command line carries `marker` as a whole argument.
pub fn carries(cmd: &str, marker: &str) -> bool {
    cmd.split_whitespace().any(|w| w == marker)
}

/// One sample of a container, the fields the suites judge.
#[derive(Debug, Clone, PartialEq)]
pub struct Stat {
    pub ts: i128,
    pub cpu_total: u64,
    pub mem_usage: u64,
    pub working_set: u64,
}

impl Stat {
    pub fn from_json(v: &Value) -> Option<Stat> {
        Some(Stat {
            ts: gotime::nanos(v["timestamp"].as_str()?)?,
            cpu_total: v["cpu"]["usage"]["total"].as_u64()?,
            mem_usage: v["memory"]["usage"].as_u64().unwrap_or(0),
            working_set: v["memory"]["working_set"].as_u64().unwrap_or(0),
        })
    }
}

/// CPU used between the first and last sample, in cores.
pub fn cpu_cores(s: &[Stat]) -> Option<f64> {
    let (a, b) = (s.first()?, s.last()?);
    let dt = (b.ts - a.ts) as f64;
    if dt <= 0.0 || b.cpu_total < a.cpu_total {
        return None;
    }
    Some((b.cpu_total - a.cpu_total) as f64 / dt)
}

/// A container name as a URL path: it starts with `/`, and systemd escapes
/// (`\x2d`) or odd bytes are percent-encoded.
pub fn enc(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~@:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn clip(s: &str) -> String {
    let s = s.trim();
    match s.char_indices().nth(300) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// stormd's view of the `cadvisor` process, from its `/metrics`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Supervised {
    pub running: bool,
    pub restarts: u64,
    pub crashes: u64,
}

pub async fn stormd(addr: &str) -> Result<Supervised, String> {
    let url = format!("http://{addr}/metrics");
    let c = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().map_err(|e| e.to_string())?;
    let text = c
        .get(&url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("{url}: {e}"))?
        .text()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    parse_stormd(&text).ok_or_else(|| format!("{url} has no process=\"cadvisor\""))
}

pub fn parse_stormd(text: &str) -> Option<Supervised> {
    let mine = |l: &&str| l.contains("process=\"cadvisor\"");
    let value = |name: &str| {
        text.lines()
            .filter(mine)
            .find(|l| l.starts_with(&format!("{name}{{")))
            .and_then(|l| l.rsplit(' ').next()?.parse::<f64>().ok())
    };
    let running = text
        .lines()
        .filter(mine)
        .any(|l| l.starts_with("stormd_process_state{") && l.contains("state=\"running\"") && l.ends_with(" 1"));
    let restarts = value("stormd_process_restarts_total")?;
    Some(Supervised { running, restarts: restarts as u64, crashes: value("stormd_process_crashes_total").unwrap_or(0.0) as u64 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let p = format!("{}/../crates/cadvisor-model/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
    }

    #[test]
    fn stats_are_read_from_a_captured_v0_49_2_answer() {
        let v = fixture("v2_stats_deprecated.json");
        let (name, arr) = v.as_object().unwrap().iter().next().unwrap();
        assert!(name.starts_with("/machine.slice/libpod-"));
        let s: Vec<Stat> = arr.as_array().unwrap().iter().filter_map(Stat::from_json).collect();
        assert!(!s.is_empty());
        assert_eq!(s[0].cpu_total, 8787723000);
        assert_eq!((s[0].mem_usage, s[0].working_set), (1687552, 1683456));
    }

    #[test]
    fn cpu_is_a_rate_in_cores() {
        let st = |ts: i128, cpu: u64| Stat { ts, cpu_total: cpu, mem_usage: 0, working_set: 0 };
        assert_eq!(cpu_cores(&[st(0, 0), st(2_000_000_000, 1_000_000_000)]), Some(0.5));
        assert_eq!(cpu_cores(&[st(0, 5), st(0, 9)]), None);
        assert_eq!(cpu_cores(&[st(0, 9), st(1, 5)]), None);
    }

    #[test]
    fn markers_match_whole_arguments() {
        let ps = fixture("v2_ps.json");
        assert!(ps.as_array().unwrap().iter().any(|p| carries(p["cmd"].as_str().unwrap(), "systemd")));
        assert!(carries("/test workload cadvt-r-a cpu=100", "cadvt-r-a"));
        assert!(!carries("/test workload cadvt-r-ab cpu=100", "cadvt-r-a"));
    }

    #[test]
    fn container_names_are_url_safe() {
        assert_eq!(enc("/system.slice/a\\x2db.service"), "/system.slice/a%5Cx2db.service");
        assert_eq!(enc("/stormpump/w3-12"), "/stormpump/w3-12");
    }

    #[test]
    fn stormd_metrics_are_read_for_cadvisor_only() {
        let m = "stormd_process_state{container=\"cadvisor\",process=\"cadvisor\",state=\"running\"} 1\n\
                 stormd_process_restarts_total{container=\"cadvisor\",process=\"cadvisor\"} 2\n\
                 stormd_process_restarts_total{container=\"x\",process=\"other\"} 9\n";
        let s = parse_stormd(m).unwrap();
        assert!(s.running);
        assert_eq!((s.restarts, s.crashes), (2, 0));
        assert!(parse_stormd("stormd_up 1\n").is_none());
    }
}
