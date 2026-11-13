//! #25 (stormcentral#596): the APIs cadvisor declares in the component
//! registry for stormd's API health probes. Each probe must be a cheap *real*
//! read, not `/healthz`, so this runs the real binary and asks exactly the
//! declared paths:
//!
//! - `stats`: `/api/v2.0/stats?count=1` — the root container's newest
//!   housekeeping sample, read through the manager's container map and the
//!   root's sample store (the locks housekeeping writes under).
//! - `metrics`: `/metrics` — the Prometheus scrape, which collects every
//!   container's stats.
//!
//! Keep these paths in step with the `apis` declaration in README.md
//! ("API health probes").

#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// HTTP/1.0 GET (no chunking): (status, body, how long it took).
fn get(port: u16, path: &str) -> std::io::Result<(u16, String, Duration)> {
    let t = Instant::now();
    let mut tcp = TcpStream::connect(("127.0.0.1", port))?;
    tcp.set_read_timeout(Some(Duration::from_secs(15)))?;
    write!(tcp, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let mut buf = Vec::new();
    let _ = tcp.read_to_end(&mut buf);
    let took = t.elapsed();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::other(format!("no status line in {head:?}")))?;
    Ok((status, body.to_string(), took))
}

fn wait_until(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn the_declared_probes_are_real_reads_that_answer() {
    let port = free_port();
    let _cad = Kill(
        Command::new(env!("CARGO_BIN_EXE_cadvisor"))
            .args(["--listen-ip", "127.0.0.1", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    wait_until("/healthz", 30, || get(port, "/healthz").map(|r| r.0).ok() == Some(200));

    // stats: once housekeeping has sampled the root, the probe returns that
    // one sample. Before the first sample it is still a 200 (an empty list),
    // so the probe never reads `down` while cadvisor starts.
    let mut sample = serde_json::Value::Null;
    wait_until("a root sample from /api/v2.0/stats?count=1", 30, || {
        let Ok((status, body, _)) = get(port, "/api/v2.0/stats?count=1") else { return false };
        assert_eq!(status, 200, "stats probe: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).expect("stats probe answers JSON");
        match v["/"].as_array() {
            Some(a) if a.len() == 1 => {
                sample = a[0].clone();
                true
            }
            Some(a) => {
                assert!(a.is_empty(), "count=1 gave {} samples", a.len());
                false
            }
            None => panic!("stats probe has no root container: {body}"),
        }
    });
    assert!(sample["timestamp"].is_string(), "root sample has a timestamp: {sample}");
    assert!(sample["cpu"].is_object() || sample["memory"].is_object(), "root sample has stats: {sample}");

    // metrics: the scrape carries the root's CPU, the work cadvisor exists for.
    let (status, body, _) = get(port, "/metrics").unwrap();
    assert_eq!(status, 200);
    assert!(body.contains("container_cpu_usage_seconds_total{"), "scrape has container CPU");

    // Both are cheap: well inside the declared p50 budget (100 ms) here.
    for p in ["/api/v2.0/stats?count=1", "/metrics"] {
        let mut took: Vec<Duration> = (0..20).map(|_| get(port, p).unwrap().2).collect();
        took.sort();
        assert!(took[10] < Duration::from_millis(100), "{p}: p50 {:?}", took[10]);
    }
}
