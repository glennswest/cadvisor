//! #40: the optional metric groups (`tcp`, `udp`, `sched`, `hugetlb`,
//! `percpu`) and the delimited protobuf exposition, against the real binary
//! reading this machine's cgroup tree and /proc.
//!
//! The root container `/` is the one every machine has: its network is pid
//! 1's, where this test's cadvisor listens (so `tcp_state="listen"` is at
//! least 1), and its processes are every process (so the schedstat run time
//! is above 0). Hugepages depend on the machine, so only the flag is checked.

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

/// HTTP/1.0 GET: (status, head, body bytes).
fn get(port: u16, path: &str, accept: &str) -> std::io::Result<(u16, String, Vec<u8>)> {
    let mut tcp = TcpStream::connect(("127.0.0.1", port))?;
    tcp.set_read_timeout(Some(Duration::from_secs(15)))?;
    write!(tcp, "GET {path} HTTP/1.0\r\nHost: localhost\r\nAccept: {accept}\r\n\r\n")?;
    let mut buf = Vec::new();
    let _ = tcp.read_to_end(&mut buf);
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("no header end"))?;
    let head = String::from_utf8_lossy(&buf[..split]).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::other(format!("no status line in {head:?}")))?;
    Ok((status, head, buf[split + 4..].to_vec()))
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

fn start(args: &[&str]) -> (Kill, u16) {
    let port = free_port();
    let cad = Kill(
        Command::new(env!("CARGO_BIN_EXE_cadvisor"))
            .args(["--listen-ip", "127.0.0.1", "--port", &port.to_string()])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    wait_until("/healthz", 30, || get(port, "/healthz", "*/*").map(|r| r.0).ok() == Some(200));
    (cad, port)
}

fn text(port: u16) -> String {
    let (status, _, body) = get(port, "/metrics", "text/plain;version=0.0.4").unwrap();
    assert_eq!(status, 200);
    String::from_utf8(body).unwrap()
}

/// The root container's sample of `family` with `label` (`k="v"`), if any.
fn root_value(body: &str, family: &str, label: &str) -> Option<f64> {
    body.lines()
        .filter(|l| l.starts_with(&format!("{family}{{")))
        .filter(|l| l.contains(r#"id="/""#))
        .filter(|l| label.is_empty() || l.contains(label))
        .find_map(|l| l.rsplit_once('}')?.1.split_whitespace().next()?.parse().ok())
}

fn varint(b: &[u8], i: &mut usize) -> u64 {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let byte = b[*i];
        *i += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return v;
        }
        shift += 7;
    }
}

/// Family names from a delimited `MetricFamily` stream (name is field 1,
/// written first) and how many metrics (field 4) each has.
fn proto_families(b: &[u8]) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let len = varint(b, &mut i) as usize;
        let fam = &b[i..i + len];
        i += len;
        let (mut j, mut name, mut metrics) = (0, String::new(), 0);
        while j < fam.len() {
            let key = varint(fam, &mut j);
            match key & 7 {
                0 => {
                    varint(fam, &mut j);
                }
                2 => {
                    let l = varint(fam, &mut j) as usize;
                    if key >> 3 == 1 {
                        name = String::from_utf8(fam[j..j + l].to_vec()).unwrap();
                    }
                    if key >> 3 == 4 {
                        metrics += 1;
                    }
                    j += l;
                }
                w => panic!("unexpected wire type {w} in {name}"),
            }
        }
        out.push((name, metrics));
    }
    out
}

#[test]
fn the_optional_groups_are_off_by_default() {
    let (_cad, port) = start(&[]);
    wait_until("root CPU", 30, || root_value(&text(port), "container_cpu_usage_seconds_total", "").is_some());
    let body = text(port);
    for fam in [
        "container_network_tcp_usage_total",
        "container_network_udp_usage_total",
        "container_cpu_schedstat_run_seconds_total",
        "container_hugetlb_usage_bytes",
    ] {
        assert!(!body.contains(&format!("# TYPE {fam} ")), "{fam} is on by default");
    }
    // Nor collected: the JSON sample carries no schedstat or connections.
    let (_, _, json) = get(port, "/api/v2.0/stats?count=1", "*/*").unwrap();
    let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    let s = &v["/"][0];
    assert_eq!(s["cpu"]["schedstat"]["run_time"], 0, "{s}");
}

#[test]
fn enabled_groups_render_and_protobuf_matches_text() {
    let (_cad, port) = start(&[
        "--enable_metrics",
        "cpu,cpuLoad,memory,disk,diskIO,network,oom_event,percpu,sched,tcp,udp,hugetlb",
    ]);
    wait_until("root schedstat", 30, || {
        root_value(&text(port), "container_cpu_schedstat_run_seconds_total", "").is_some_and(|v| v > 0.0)
    });
    let body = text(port);
    let listen = root_value(&body, "container_network_tcp_usage_total", r#"tcp_state="listen""#);
    assert!(listen.is_some_and(|v| v >= 1.0), "root tcp listen = {listen:?} (cadvisor itself listens)");
    for (fam, label) in [
        ("container_network_tcp6_usage_total", r#"tcp_state="established""#),
        ("container_network_udp_usage_total", r#"udp_state="listen""#),
        ("container_network_udp6_usage_total", r#"udp_state="rxqueued""#),
        ("container_cpu_schedstat_runqueue_seconds_total", ""),
        ("container_cpu_schedstat_run_periods_total", ""),
    ] {
        assert!(root_value(&body, fam, label).is_some(), "no root {fam} {label}");
    }

    // The same scrape as protobuf, asked the way Prometheus asks.
    let accept = "application/vnd.google.protobuf;proto=io.prometheus.client.MetricFamily;encoding=delimited;q=0.7,text/plain;version=0.0.4;q=0.3,*/*;q=0.2";
    let (status, head, pb) = get(port, "/metrics", accept).unwrap();
    assert_eq!(status, 200);
    assert!(
        head.to_ascii_lowercase().contains(
            "content-type: application/vnd.google.protobuf; proto=io.prometheus.client.metricfamily; encoding=delimited"
        ),
        "{head}"
    );
    let fams = proto_families(&pb);
    let text_fams: Vec<&str> =
        body.lines().filter_map(|l| l.strip_prefix("# TYPE ")?.split(' ').next()).collect();
    let proto_names: Vec<&str> = fams.iter().map(|f| f.0.as_str()).collect();
    // Containers can come and go between the two scrapes, but the families
    // of the root and the machine are always there, in the same order.
    for f in ["container_cpu_usage_seconds_total", "container_network_tcp_usage_total", "machine_cpu_cores"] {
        assert!(proto_names.contains(&f), "protobuf has no {f}: {proto_names:?}");
    }
    let mut sorted = proto_names.clone();
    sorted.sort_unstable();
    assert_eq!(proto_names, sorted, "families in name order");
    let common: Vec<&&str> = text_fams.iter().filter(|f| proto_names.contains(f)).collect();
    assert!(common.len() + 2 >= text_fams.len(), "text {text_fams:?} vs protobuf {proto_names:?}");
    let tcp = fams.iter().find(|f| f.0 == "container_network_tcp_usage_total").unwrap();
    assert_eq!(tcp.1 % 11, 0, "11 tcp states per container");
}

#[test]
fn without_percpu_each_container_has_only_the_total() {
    // percpu off: upstream emits cpu="total" for every container, even at 0
    // (with it on, the default, only above 0; cgroup v2 has no per-CPU
    // usage either way).
    let (_cad, port) = start(&["--disable_metrics", "percpu"]);
    wait_until("root CPU", 30, || root_value(&text(port), "container_cpu_usage_seconds_total", "").is_some());
    let body = text(port);
    let cpu: Vec<&str> = body.lines().filter(|l| l.starts_with("container_cpu_usage_seconds_total{")).collect();
    assert!(!cpu.is_empty());
    assert!(cpu.iter().all(|l| l.contains(r#"cpu="total""#)), "{cpu:?}");
}
