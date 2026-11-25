//! #43: the root container `/` has a filesystem, as upstream's raw handler
//! says for the root cgroup: its spec carries `has_filesystem: true` on v1
//! and v2, and `/api/v2.0/stats` carries the root sample's filesystems (the
//! partitions housekeeping read), against the real binary on this machine.

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

fn get_json(port: u16, path: &str) -> serde_json::Value {
    let (status, body, _) = get(port, path).unwrap();
    assert_eq!(status, 200, "{path}: {body}");
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{path}: {e}: {body}"))
}

#[test]
fn the_root_container_has_a_filesystem() {
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

    let spec = get_json(port, "/api/v2.0/spec");
    assert_eq!(spec["/"]["has_filesystem"], true, "v2 root spec: {}", spec["/"]);
    let v1 = get_json(port, "/api/v1.3/containers/");
    assert_eq!(v1["spec"]["has_filesystem"], true, "v1 root spec: {}", v1["spec"]);

    let mut sample = serde_json::Value::Null;
    wait_until("a root sample from /api/v2.0/stats?count=1", 30, || {
        let v = get_json(port, "/api/v2.0/stats?count=1");
        match v["/"].as_array() {
            Some(a) if !a.is_empty() => {
                sample = a[0].clone();
                true
            }
            _ => false,
        }
    });
    assert_eq!(sample["has_filesystem"], true, "root sample: {sample}");
    let fs = sample["filesystem"].as_array().unwrap_or_else(|| panic!("root sample has no filesystem: {sample}"));
    assert!(!fs.is_empty(), "root sample lists no partition: {sample}");
    for f in fs {
        assert!(f["device"].as_str().is_some_and(|d| !d.is_empty()), "partition without a device: {f}");
        assert!(f["capacity"].as_u64().is_some_and(|c| c > 0), "partition without a capacity: {f}");
    }
}
