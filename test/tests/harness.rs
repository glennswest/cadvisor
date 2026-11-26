//! The suites against a real cadvisor, on whatever Linux host `cargo test`
//! runs on (a build VM, under `sc-build`): everything that needs no
//! apiserver. Workload tests report skip here (`CADVISOR_TEST_NO_PODS=1`) and
//! run for real only on a node.
//!
//! cadvisor runs as the stormcos golden runs it since stormcos#143: https
//! with a certificate from a CA of the harness's own (made with `openssl`,
//! as stormcos's build-VM tests make theirs) and a bearer it checks. The
//! golden reviews bearers at the apiserver (`--auth-mode review`); here it
//! takes them from `--bearer-token-file`, the same middleware with no
//! apiserver. Plain http, cadvisor's default off stormcos, runs too.
//!
//! Needs the cadvisor binary: `CADVISOR_BIN=<path>`. Without it the harness
//! says so and passes, so a plain `cargo test` still runs the unit tests.

use std::collections::HashMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cadvisor_test::env::{CadAuth, Env};
use cadvisor_test::report::Report;

struct Cadvisor(Child);

impl Drop for Cadvisor {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cadvt-harness-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn openssl(dir: &Path, args: &[&str]) {
    let out = Command::new("openssl")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("openssl {args:?}: {e} (the harness makes its certificates with the openssl CLI)"));
    assert!(out.status.success(), "openssl {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A CA `ca.crt` and a server pair `s.crt`/`s.key` it signs for 127.0.0.1
/// and localhost, as stormcos mints cadvisor's from the node CA.
fn certs(dir: &Path) {
    openssl(dir, &["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "ca.key", "-out", "ca.crt", "-days", "2", "-subj", "/CN=harness-ca"]);
    openssl(dir, &["req", "-newkey", "rsa:2048", "-nodes", "-keyout", "s.key", "-out", "s.csr", "-subj", "/CN=127.0.0.1"]);
    std::fs::write(
        dir.join("s.ext"),
        "subjectAltName=IP:127.0.0.1,DNS:localhost\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n",
    )
    .unwrap();
    openssl(dir, &["x509", "-req", "-in", "s.csr", "-CA", "ca.crt", "-CAkey", "ca.key", "-CAcreateserial", "-days", "2", "-extfile", "s.ext", "-out", "s.crt"]);
}

/// A secured cadvisor: its files, under one scratch directory.
struct Secured {
    dir: PathBuf,
}

impl Secured {
    fn new(name: &str) -> Secured {
        let dir = scratch(name);
        certs(&dir);
        std::fs::write(dir.join("token"), format!("harness-EXAMPLE-{}\n", std::process::id())).unwrap(); // notsecret
        Secured { dir }
    }

    fn path(&self, f: &str) -> String {
        self.dir.join(f).to_str().unwrap().to_string()
    }

    fn args(&self) -> Vec<String> {
        ["--tls-cert-file", &self.path("s.crt"), "--tls-key-file", &self.path("s.key"), "--bearer-token-file", &self.path("token")]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }
}

fn start(extra: &[String]) -> Option<(Cadvisor, String)> {
    let bin = std::env::var("CADVISOR_BIN").ok().filter(|b| !b.is_empty())?;
    assert!(Path::new(&bin).exists(), "CADVISOR_BIN={bin} does not exist");
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let child = Command::new(&bin)
        .args(["--listen-ip", "127.0.0.1", "--port", &port.to_string()])
        .args(extra)
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

/// The Env a Job would read, from these variables only (no process
/// environment, so the tests run side by side) and no ServiceAccount.
fn env(addr: &str, suite: &str, extra: &[(&str, String)]) -> Env {
    let mut vars: HashMap<String, String> = [
        ("CADVISOR_TEST_ADDR", addr.to_string()),
        ("CADVISOR_TEST_STORMD", "none".into()),
        ("CADVISOR_TEST_NO_PODS", "1".into()),
        ("STORM_RUN_ID", format!("harness-{suite}")),
        ("STORM_NODE", "127.0.0.1".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for (k, v) in extra {
        vars.insert(k.to_string(), v.clone());
    }
    let e = Env::from_vars(Some(suite.to_string()), |k| vars.get(k).cloned(), "/nonexistent/serviceaccount");
    assert!(e.missing().is_empty(), "{:?}", e.missing());
    e
}

fn run(e: &Env) -> (i32, Report) {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let mut r = Report::new();
    rt.block_on(cadvisor_test::run(e, &mut r));
    (r.finish(), r)
}

fn outcome<'a>(r: &'a Report, name: &str) -> Option<&'a str> {
    r.outcomes.iter().find(|(t, _)| t == name).map(|(_, s)| *s)
}

fn all_pass(suite: &str, code: i32, r: &Report, want: &[&str]) {
    let failed: Vec<&(String, &str)> = r.outcomes.iter().filter(|(_, s)| *s == "fail").collect();
    assert!(failed.is_empty(), "{suite}: failed {failed:?}");
    assert_eq!(code, 0, "{suite}");
    for n in want {
        assert_eq!(outcome(r, n), Some("pass"), "{suite}: {n} did not pass: {:?}", r.outcomes);
    }
    // Pod tests are skipped, never passed, without an apiserver.
    assert!(r.outcomes.iter().any(|(t, s)| (t == "workload-discovered" || t == "events") && *s == "skip"));
}

const CHECKS: [&str; 5] = ["health", "version", "machine", "metrics", "housekeeping"];

#[test]
fn short_and_medium_over_https_with_a_bearer() {
    let sec = Secured::new("https");
    let Some((_cadvisor, addr)) = start(&sec.args()) else {
        eprintln!("CADVISOR_BIN is not set: the harness did not run (unit tests still did)");
        return;
    };
    let vars = [("CADVISOR_TEST_CA", sec.path("ca.crt")), ("CADVISOR_TEST_TOKEN_FILE", sec.path("token"))];
    let e = env(&addr, "short", &vars);
    assert_eq!(e.cadvisor_url(), format!("https://{addr}"));
    let (code, r) = run(&e);
    all_pass("short", code, &r, &CHECKS);

    let (code, r) = run(&env(&addr, "medium", &vars));
    let mut want = CHECKS.to_vec();
    want.extend(["api-listings", "v1-endpoints", "v2-endpoints", "error-contract", "auth", "metrics-concurrent"]);
    all_pass("medium", code, &r, &want);
}

#[test]
fn short_over_plain_http_with_no_bearer() {
    let Some((_cadvisor, addr)) = start(&[]) else { return };
    let vars = [("CADVISOR_TEST_SCHEME", "http".to_string()), ("CADVISOR_TEST_TOKEN_FILE", "none".into())];
    let (code, r) = run(&env(&addr, "short", &vars));
    all_pass("short", code, &r, &CHECKS);
}

#[test]
fn a_run_with_no_bearer_is_told_why() {
    let sec = Secured::new("nobearer");
    let Some((_cadvisor, addr)) = start(&sec.args()) else { return };
    let vars = [("CADVISOR_TEST_CA", sec.path("ca.crt")), ("CADVISOR_TEST_TOKEN_FILE", "none".into())];
    let (code, r) = run(&env(&addr, "short", &vars));
    assert_eq!(code, 1, "{:?}", r.outcomes);
    // The health paths stay anonymous; everything else is refused, and says why.
    assert_eq!(outcome(&r, "health"), Some("pass"), "{:?}", r.outcomes);
    assert_eq!(outcome(&r, "version"), Some("fail"), "{:?}", r.outcomes);
    let why = r.details.iter().find(|(t, _)| t == "version").map(|(_, d)| d.clone()).unwrap();
    assert!(why.contains("401") && why.contains("CADVISOR_TEST_TOKEN_FILE"), "{why}");
}

#[test]
fn a_certificate_from_another_ca_is_refused() {
    let sec = Secured::new("served");
    let other = scratch("otherca");
    certs(&other);
    let Some((_cadvisor, addr)) = start(&sec.args()) else { return };
    let vars = [("CADVISOR_TEST_CA", other.join("ca.crt").to_str().unwrap().to_string()), ("CADVISOR_TEST_TOKEN_FILE", sec.path("token"))];
    let (code, r) = run(&env(&addr, "short", &vars));
    // No stormd here, so an unverifiable cadvisor is could-not-run, never a pass.
    assert_eq!(code, 2, "{:?}", r.outcomes);
    assert_eq!(r.outcomes, vec![("cadvisor-up".to_string(), "fail")]);
    let why = r.details[0].1.to_ascii_lowercase();
    assert!(why.starts_with("could not run") && why.contains("certificate"), "{why}");
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
        cad_auth: CadAuth { scheme: "https".into(), ca: None, token: None, error: None },
        timeout: Duration::from_secs(60),
        started: Instant::now(),
        no_pods: true,
    };
    let (code, r) = run(&e);
    assert_eq!(code, 2);
    assert_eq!(r.outcomes, vec![("cadvisor-up".to_string(), "fail")]);
}
