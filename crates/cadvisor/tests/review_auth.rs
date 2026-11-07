//! #27: the real binary with `--auth-mode review` against a fake apiserver.
//! A caller's own token is checked by TokenReview, then SubjectAccessReview
//! for `get nodes/metrics` on the node: 401 for an unknown token, 403 for one
//! without the permission, 200 with it. Tokens in `--bearer-token-file` pass
//! without a review, the health paths need nothing, and an apiserver that
//! can't be reached is a 503.

#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode};
use serde_json::{Value, json};

const OWN: &str = "cadvisor-own-sa-token";
const NODE: &str = "test-node";

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cadvisor-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Starts a fake apiserver on its own thread; returns (review count, port).
fn fake_apiserver() -> (Arc<AtomicUsize>, u16) {
    let reviews = Arc::new(AtomicUsize::new(0));
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let port = l.local_addr().unwrap().port();
    let count = reviews.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let own = |h: &HeaderMap| h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {OWN}"));
            let c = count.clone();
            let app = axum::Router::new()
                .route(
                    "/apis/authentication.k8s.io/v1/tokenreviews",
                    axum::routing::post(move |h: HeaderMap, axum::Json(b): axum::Json<Value>| {
                        let c = c.clone();
                        async move {
                            if !own(&h) {
                                return (StatusCode::UNAUTHORIZED, axum::Json(json!({})));
                            }
                            c.fetch_add(1, Ordering::SeqCst);
                            let user = match b["spec"]["token"].as_str() {
                                Some("prom-token") => Some("system:serviceaccount:monitoring:prom"),
                                Some("nobody-token") => Some("system:serviceaccount:default:nobody"),
                                _ => None,
                            };
                            let status = match user {
                                Some(u) => json!({"authenticated": true, "user": {"username": u}}),
                                None => json!({"authenticated": false}),
                            };
                            (StatusCode::CREATED, axum::Json(json!({"status": status})))
                        }
                    }),
                )
                .route(
                    "/apis/authorization.k8s.io/v1/subjectaccessreviews",
                    axum::routing::post(move |h: HeaderMap, axum::Json(b): axum::Json<Value>| async move {
                        if !own(&h) {
                            return (StatusCode::UNAUTHORIZED, axum::Json(json!({})));
                        }
                        let a = &b["spec"]["resourceAttributes"];
                        let allowed = b["spec"]["user"] == "system:serviceaccount:monitoring:prom"
                            && a["verb"] == "get"
                            && a["resource"] == "nodes"
                            && a["subresource"] == "metrics"
                            && a["name"] == NODE;
                        (StatusCode::CREATED, axum::Json(json!({"status": {"allowed": allowed}})))
                    }),
                );
            let l = tokio::net::TcpListener::from_std(l).unwrap();
            axum::serve(l, app).await.unwrap();
        });
    });
    (reviews, port)
}

/// Plain-HTTP GET: the status code.
fn get(port: u16, path: &str, token: Option<&str>) -> std::io::Result<u16> {
    let mut tcp = TcpStream::connect(("127.0.0.1", port))?;
    tcp.set_read_timeout(Some(Duration::from_secs(15)))?;
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(tcp, "GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n")?;
    let mut buf = Vec::new();
    let _ = tcp.read_to_end(&mut buf);
    let head = String::from_utf8_lossy(&buf);
    head.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::other(format!("no status line in {head:?}")))
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

fn cadvisor(port: u16, apiserver: &str, own: &std::path::Path, extra: &[&std::ffi::OsStr]) -> Kill {
    Kill(
        Command::new(env!("CARGO_BIN_EXE_cadvisor"))
            .args(["--listen-ip", "127.0.0.1", "--port", &port.to_string()])
            .args(["--auth-mode", "review", "--kube-apiserver", apiserver, "--kube-node-name", NODE])
            .arg("--kube-token-file")
            .arg(own)
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}

#[test]
fn review_mode_401_403_200_with_a_token_file_beside_it() {
    let dir = scratch("review");
    let own = dir.join("own-token");
    std::fs::write(&own, format!("{OWN}\n")).unwrap();
    let tokens = dir.join("tokens");
    std::fs::write(&tokens, "test-file-token\n").unwrap();
    let (reviews, api) = fake_apiserver();

    let port = free_port();
    let _cad = cadvisor(port, &format!("http://127.0.0.1:{api}"), &own, &[std::ffi::OsStr::new("--bearer-token-file"), tokens.as_os_str()]);
    wait_until("/healthz", 30, || get(port, "/healthz", None).ok() == Some(200));

    for p in ["/healthz", "/-/healthy", "/-/ready"] {
        assert_eq!(get(port, p, None).unwrap(), 200, "{p} without a token");
    }
    for p in ["/metrics", "/api/v2.0/version", "/api/v1.3/subcontainers/"] {
        assert_eq!(get(port, p, None).unwrap(), 401, "{p} without a token");
        assert_eq!(get(port, p, Some("forged")).unwrap(), 401, "{p} with an unknown token");
        assert_eq!(get(port, p, Some("nobody-token")).unwrap(), 403, "{p} without get nodes/metrics");
        assert_eq!(get(port, p, Some("prom-token")).unwrap(), 200, "{p} with get nodes/metrics");
        assert_eq!(get(port, p, Some("test-file-token")).unwrap(), 200, "{p} with the file's token");
    }
    // Three tokens reviewed once each; the rest came from the cache, and
    // the file's token never reached the apiserver.
    assert_eq!(reviews.load(Ordering::SeqCst), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unreachable_apiserver_is_a_503_and_bad_flags_will_not_start() {
    let dir = scratch("review-down");
    let own = dir.join("own-token");
    std::fs::write(&own, OWN).unwrap();
    let dead = free_port();
    let port = free_port();
    let _cad = cadvisor(port, &format!("http://127.0.0.1:{dead}"), &own, &[]);
    wait_until("/healthz", 30, || get(port, "/healthz", None).ok() == Some(200));
    assert_eq!(get(port, "/metrics", Some("prom-token")).unwrap(), 503);
    assert_eq!(get(port, "/metrics", None).unwrap(), 401);

    let refuse = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_cadvisor"))
            .args(["--listen-ip", "127.0.0.1", "--port", &free_port().to_string()])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?} started");
        String::from_utf8_lossy(&out.stderr).to_string()
    };
    let own = own.to_str().unwrap();
    assert!(refuse(&["--auth-mode", "review"]).contains("--kube-apiserver"));
    assert!(refuse(&["--auth-mode", "webhook"]).contains("--auth-mode"));
    assert!(refuse(&["--auth-mode", "review", "--kube-apiserver", "https://127.0.0.1:6443", "--kube-token-file", own])
        .contains("--kube-ca-file"));
    let _ = std::fs::remove_dir_all(&dir);
}
