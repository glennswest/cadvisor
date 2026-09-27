//! The suites against a real cadvisor, on whatever Linux host `cargo test`
//! runs on (the build box, under `sc-build`): everything that needs no
//! apiserver. Workload tests report skip here (`CADVISOR_TEST_NO_PODS=1`) and
//! run for real only on a node.
//!
//! Needs the cadvisor binary: `CADVISOR_BIN=<path>`. Without it the harness
//! says so and passes, so a plain `cargo test` still runs the unit tests.

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cadvisor_test::env::Env;
use cadvisor_test::report::Report;

struct Cadvisor(Child);

impl Drop for Cadvisor {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start() -> Option<(Cadvisor, String)> {
    let bin = std::env::var("CADVISOR_BIN").ok().filter(|b| !b.is_empty())?;
    assert!(std::path::Path::new(&bin).exists(), "CADVISOR_BIN={bin} does not exist");
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let child = Command::new(&bin)
        .args(["--listen-ip", "127.0.0.1", "--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| panic!("starting {bin}: {e}"));
    let addr = format!("127.0.0.1:{port}");
    let t = Instant::now();
    while std::net::TcpStream::connect(&addr).is_err() {
        assert!(t.elapsed() < Duration::from_secs(30), "cadvisor did not listen on {addr} within 30 s");
        std::thread::sleep(Duration::from_millis(200));
    }
    Some((Cadvisor(child), addr))
}

fn env(addr: &str, suite: &str) -> Env {
    // One test function sets these, so there is no race between tests.
    std::env::set_var("CADVISOR_TEST_ADDR", addr);
    std::env::set_var("CADVISOR_TEST_STORMD", "none");
    std::env::set_var("CADVISOR_TEST_NO_PODS", "1");
    std::env::set_var("STORM_RUN_ID", format!("harness-{suite}"));
    std::env::set_var("STORM_NODE", "127.0.0.1");
    Env::read(Some(suite.to_string()))
}

#[test]
fn short_and_medium_against_a_real_cadvisor() {
    let Some((_cadvisor, addr)) = start() else {
        eprintln!("CADVISOR_BIN is not set: the harness did not run (unit tests still did)");
        return;
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    for suite in ["short", "medium"] {
        let e = env(&addr, suite);
        assert!(e.missing().is_empty(), "{:?}", e.missing());
        let mut r = Report::new();
        rt.block_on(cadvisor_test::run(&e, &mut r));
        let code = r.finish();
        let failed: Vec<&(String, &str)> = r.outcomes.iter().filter(|(_, s)| *s == "fail").collect();
        assert!(failed.is_empty(), "{suite}: failed {failed:?}");
        assert_eq!(code, 0, "{suite}");
        let passed = |n: &str| r.outcomes.iter().any(|(t, s)| t == n && *s == "pass");
        for n in ["health", "version", "machine", "metrics", "housekeeping"] {
            assert!(passed(n), "{suite}: {n} did not pass: {:?}", r.outcomes);
        }
        if suite == "medium" {
            for n in ["api-listings", "v1-endpoints", "v2-endpoints", "error-contract", "metrics-concurrent"] {
                assert!(passed(n), "medium: {n} did not pass: {:?}", r.outcomes);
            }
        }
        // Pod tests are skipped, never passed, without an apiserver.
        assert!(r.outcomes.iter().any(|(t, s)| (t == "workload-discovered" || t == "events") && *s == "skip"));
    }
}

#[test]
fn a_node_with_nothing_listening_is_could_not_run() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let e = Env {
        suite: "short".into(),
        run_id: "harness-down".into(),
        namespace: String::new(),
        api: String::new(),
        node: "127.0.0.1".into(),
        cadvisor: format!("127.0.0.1:{port}"),
        stormd: None,
        pod: String::new(),
        token: None,
        ca: None,
        timeout: Duration::from_secs(60),
        started: Instant::now(),
        no_pods: true,
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let mut r = Report::new();
    rt.block_on(cadvisor_test::run(&e, &mut r));
    assert_eq!(r.finish(), 2);
    assert_eq!(r.outcomes, vec![("cadvisor-up".to_string(), "fail")]);
}
