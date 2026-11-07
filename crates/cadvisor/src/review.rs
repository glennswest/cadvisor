//! Bearer auth by TokenReview + SubjectAccessReview (#27; owner's decision
//! (A) on stormcos#143).
//!
//! A caller presents its own ServiceAccount token. cadvisor asks the node's
//! apiserver who it is (`TokenReview`), then whether that user may `get` the
//! node's `nodes/metrics` subresource (`SubjectAccessReview`). That is the
//! attribute upstream's kubelet checks for `/metrics/cadvisor`, so the RBAC
//! a Prometheus already has for the kubelet works here too. One attribute
//! covers every path: `/metrics` and the REST API alike (rustkube's
//! `metrics.k8s.io` reads `/api/v1.3/subcontainers/`).
//!
//! cadvisor calls the reviews with its own token (`--kube-token-file`), and
//! checks the apiserver's certificate against `--kube-ca-file`. Both files
//! are re-read when replaced (projected tokens rotate). A decision is cached
//! for [`TTL`] per (token, verb), as upstream's webhook authorizer caches
//! one. An apiserver that can't be reached or refuses cadvisor is an error,
//! which is not cached: the caller gets a 503, not a 401 it would believe.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use serde_json::{Value, json};

use crate::secure::Watched;

/// How long a review's outcome is reused.
pub const TTL: Duration = Duration::from_secs(30);
/// Cache entries kept before expired ones are dropped.
const CACHE_MAX: usize = 4096;
/// A whole review (connect, TLS, both requests).
const TIMEOUT: Duration = Duration::from_secs(10);

/// What the apiserver said about a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    /// Not a valid token: 401.
    Unauthenticated,
    /// A valid token without `get nodes/metrics`: 403.
    Forbidden,
}

/// `scheme://host[:port]` of the apiserver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub tls: bool,
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    pub fn parse(url: &str) -> anyhow::Result<Self> {
        let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = url.strip_prefix("http://") {
            (false, r)
        } else {
            bail!("--kube-apiserver {url:?}: want https://host[:port]");
        };
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() || rest.contains('/') {
            bail!("--kube-apiserver {url:?}: want https://host[:port], no path");
        }
        let default_port = if tls { 443 } else { 80 };
        let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
            let (h, after) = v6.split_once(']').context("--kube-apiserver: unclosed [")?;
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse().context("--kube-apiserver: bad port")?,
                None if after.is_empty() => default_port,
                None => bail!("--kube-apiserver {url:?}: bad host"),
            };
            (h.to_string(), port)
        } else {
            match rest.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p.parse().context("--kube-apiserver: bad port")?),
                None => (rest.to_string(), default_port),
            }
        };
        if host.is_empty() {
            bail!("--kube-apiserver {url:?}: no host");
        }
        Ok(Endpoint { tls, host, port })
    }

    fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn load_connector(paths: &[PathBuf]) -> anyhow::Result<tokio_rustls::TlsConnector> {
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_file_iter(&paths[0])
        .with_context(|| format!("read {}", paths[0].display()))?
    {
        roots.add(c.with_context(|| format!("parse {}", paths[0].display()))?)?;
    }
    if roots.is_empty() {
        bail!("{}: no CA certificate", paths[0].display());
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("tls protocol versions")?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(tokio_rustls::TlsConnector::from(Arc::new(cfg)))
}

fn load_own_token(paths: &[PathBuf]) -> anyhow::Result<Arc<str>> {
    let t = std::fs::read_to_string(&paths[0]).with_context(|| format!("read {}", paths[0].display()))?;
    let t = t.trim();
    if t.is_empty() {
        bail!("{}: empty token", paths[0].display());
    }
    Ok(Arc::from(t))
}

/// The kubelet's verb for a request method.
pub fn verb(method: &hyper::Method) -> &'static str {
    match *method {
        hyper::Method::GET | hyper::Method::HEAD => "get",
        hyper::Method::POST => "create",
        hyper::Method::PUT => "update",
        hyper::Method::PATCH => "patch",
        hyper::Method::DELETE => "delete",
        _ => "get",
    }
}

type Cache = HashMap<(Vec<u8>, &'static str), (Instant, Decision)>;

pub struct Reviewer {
    endpoint: Endpoint,
    tls: Option<Watched<tokio_rustls::TlsConnector>>,
    token: Watched<Arc<str>>,
    node: String,
    ttl: Duration,
    cache: Mutex<Cache>,
}

impl std::fmt::Debug for Reviewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reviewer").field("endpoint", &self.endpoint).field("node", &self.node).finish()
    }
}

impl Reviewer {
    /// Loads the CA and token now; a bad file is the caller's startup error.
    pub fn new(apiserver: &str, ca_file: Option<&Path>, token_file: &Path, node: &str) -> anyhow::Result<Self> {
        let endpoint = Endpoint::parse(apiserver)?;
        let tls = match (endpoint.tls, ca_file) {
            (true, Some(ca)) => Some(Watched::new("kube ca", vec![ca.to_path_buf()], load_connector)?),
            (true, None) => bail!("--kube-apiserver is https: --kube-ca-file is needed to check its certificate"),
            (false, _) => None,
        };
        let token = Watched::new("kube token", vec![token_file.to_path_buf()], load_own_token)?;
        Ok(Reviewer { endpoint, tls, token, node: node.to_string(), ttl: TTL, cache: Mutex::new(HashMap::new()) })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The decision for `token` asking with `verb`, from the cache or the
    /// apiserver.
    pub async fn decide(&self, token: &[u8], verb: &'static str) -> anyhow::Result<Decision> {
        let key = (token.to_vec(), verb);
        if let Some((at, d)) = self.cache.lock().unwrap().get(&key)
            && at.elapsed() < self.ttl
        {
            return Ok(*d);
        }
        let token_str = std::str::from_utf8(token).map_err(|_| anyhow::anyhow!("token is not utf-8"));
        let d = match token_str {
            Ok(t) => tokio::time::timeout(TIMEOUT, self.review(t, verb))
                .await
                .map_err(|_| anyhow::anyhow!("review timed out after {TIMEOUT:?}"))??,
            Err(_) => Decision::Unauthenticated,
        };
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= CACHE_MAX {
            let ttl = self.ttl;
            cache.retain(|_, (at, _)| at.elapsed() < ttl);
            if cache.len() >= CACHE_MAX {
                cache.clear();
            }
        }
        cache.insert(key, (Instant::now(), d));
        Ok(d)
    }

    async fn review(&self, token: &str, verb: &'static str) -> anyhow::Result<Decision> {
        let tr = json!({
            "apiVersion": "authentication.k8s.io/v1",
            "kind": "TokenReview",
            "spec": {"token": token},
        });
        let tr = self.post("/apis/authentication.k8s.io/v1/tokenreviews", &tr).await?;
        let status = &tr["status"];
        if status["authenticated"] != Value::Bool(true) {
            return Ok(Decision::Unauthenticated);
        }
        let user = &status["user"];
        let mut attrs = json!({"verb": verb, "resource": "nodes", "subresource": "metrics"});
        if !self.node.is_empty() {
            attrs["name"] = json!(self.node);
        }
        let mut spec = json!({"user": user["username"], "resourceAttributes": attrs});
        for k in ["uid", "groups", "extra"] {
            if !user[k].is_null() {
                spec[k] = user[k].clone();
            }
        }
        let sar = json!({
            "apiVersion": "authorization.k8s.io/v1",
            "kind": "SubjectAccessReview",
            "spec": spec,
        });
        let sar = self.post("/apis/authorization.k8s.io/v1/subjectaccessreviews", &sar).await?;
        Ok(if sar["status"]["allowed"] == Value::Bool(true) { Decision::Allowed } else { Decision::Forbidden })
    }

    async fn post(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        let tcp = tokio::net::TcpStream::connect((self.endpoint.host.as_str(), self.endpoint.port))
            .await
            .with_context(|| format!("connect {}", self.endpoint.authority()))?;
        let body = serde_json::to_vec(body)?;
        let (status, bytes) = match &self.tls {
            Some(connector) => {
                let name = ServerName::try_from(self.endpoint.host.clone())
                    .with_context(|| format!("server name {:?}", self.endpoint.host))?;
                let io = connector.get().connect(name, tcp).await.context("tls to the apiserver")?;
                self.send(io, path, body).await?
            }
            None => self.send(tcp, path, body).await?,
        };
        if !status.is_success() {
            bail!(
                "apiserver POST {path}: HTTP {status}: {}",
                String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).trim()
            );
        }
        serde_json::from_slice(&bytes).with_context(|| format!("apiserver POST {path}: not JSON"))
    }

    async fn send<S>(&self, io: S, path: &str, body: Vec<u8>) -> anyhow::Result<(hyper::StatusCode, Bytes)>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (mut tx, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let req = hyper::Request::post(path)
            .header(hyper::header::HOST, self.endpoint.authority())
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .header(hyper::header::ACCEPT, "application/json")
            .header(hyper::header::AUTHORIZATION, format!("Bearer {}", self.token.get()))
            .body(Full::new(Bytes::from(body)))?;
        let resp = tx.send_request(req).await?;
        let status = resp.status();
        let bytes = resp.into_body().collect().await?.to_bytes();
        Ok((status, bytes))
    }
}

/// The node's name for the review: the kernel's hostname (cadvisor shares
/// the host's uts namespace on stormcos).
pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_string()).unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// cadvisor's own token, which the fake apiserver insists on.
    pub const OWN: &str = "cadvisor-sa-token";
    pub const READER: &str = "reader-token";
    pub const NOPERM: &str = "noperm-token";
    pub const NODE: &str = "node-a";

    #[derive(Default)]
    pub struct Fake {
        pub reviews: AtomicUsize,
        pub last_sar: Mutex<Value>,
    }

    fn own_ok(h: &HeaderMap) -> bool {
        h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {OWN}"))
    }

    /// A fake apiserver: READER is `system:serviceaccount:monitoring:prom`
    /// with `get nodes/metrics` on NODE; NOPERM authenticates but has none.
    pub fn fake_router(fake: Arc<Fake>) -> axum::Router {
        async fn tokenreview(
            State(f): State<Arc<Fake>>,
            h: HeaderMap,
            axum::Json(b): axum::Json<Value>,
        ) -> (StatusCode, axum::Json<Value>) {
            if !own_ok(&h) {
                return (StatusCode::UNAUTHORIZED, axum::Json(json!({"kind": "Status"})));
            }
            f.reviews.fetch_add(1, Ordering::SeqCst);
            let user = match b["spec"]["token"].as_str() {
                Some(READER) => Some("system:serviceaccount:monitoring:prom"),
                Some(NOPERM) => Some("system:serviceaccount:default:nobody"),
                _ => None,
            };
            let status = match user {
                Some(u) => json!({"authenticated": true, "user": {"username": u, "uid": "u-1",
                                  "groups": ["system:serviceaccounts", "system:authenticated"]}}),
                None => json!({"authenticated": false, "error": "invalid bearer token"}),
            };
            (StatusCode::CREATED, axum::Json(json!({"kind": "TokenReview", "status": status})))
        }
        async fn sar(
            State(f): State<Arc<Fake>>,
            h: HeaderMap,
            axum::Json(b): axum::Json<Value>,
        ) -> (StatusCode, axum::Json<Value>) {
            if !own_ok(&h) {
                return (StatusCode::UNAUTHORIZED, axum::Json(json!({"kind": "Status"})));
            }
            *f.last_sar.lock().unwrap() = b.clone();
            let s = &b["spec"];
            let a = &s["resourceAttributes"];
            let allowed = s["user"] == "system:serviceaccount:monitoring:prom"
                && a["verb"] == "get"
                && a["resource"] == "nodes"
                && a["subresource"] == "metrics"
                && a["name"] == NODE;
            (StatusCode::CREATED, axum::Json(json!({"kind": "SubjectAccessReview", "status": {"allowed": allowed}})))
        }
        axum::Router::new()
            .route("/apis/authentication.k8s.io/v1/tokenreviews", axum::routing::post(tokenreview))
            .route("/apis/authorization.k8s.io/v1/subjectaccessreviews", axum::routing::post(sar))
            .with_state(fake)
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cadv-review-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    async fn plain_fake() -> (Arc<Fake>, u16) {
        let fake = Arc::new(Fake::default());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let app = fake_router(fake.clone());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (fake, port)
    }

    #[test]
    fn endpoints_parse() {
        let e = Endpoint::parse("https://10.0.0.1:6443").unwrap();
        assert_eq!(e, Endpoint { tls: true, host: "10.0.0.1".into(), port: 6443 });
        assert_eq!(Endpoint::parse("https://api.lo/").unwrap().port, 443);
        assert_eq!(Endpoint::parse("http://h").unwrap().port, 80);
        let v6 = Endpoint::parse("https://[fd00::1]:6443").unwrap();
        assert_eq!((v6.host.as_str(), v6.port, v6.authority()), ("fd00::1", 6443, "[fd00::1]:6443".to_string()));
        for bad in ["api:6443", "ftp://x", "https://", "https://h/api", "https://h:x", "https://[::1"] {
            assert!(Endpoint::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn methods_map_to_the_kubelets_verbs() {
        assert_eq!(verb(&hyper::Method::GET), "get");
        assert_eq!(verb(&hyper::Method::HEAD), "get");
        assert_eq!(verb(&hyper::Method::POST), "create");
        assert_eq!(verb(&hyper::Method::DELETE), "delete");
    }

    #[tokio::test]
    async fn reviews_decide_and_are_cached() {
        let (fake, port) = plain_fake().await;
        let d = scratch("plain");
        std::fs::write(d.join("token"), format!("{OWN}\n")).unwrap();
        let mut r = Reviewer::new(&format!("http://127.0.0.1:{port}"), None, &d.join("token"), NODE).unwrap();

        assert_eq!(r.decide(READER.as_bytes(), "get").await.unwrap(), Decision::Allowed);
        let sar = fake.last_sar.lock().unwrap().clone();
        assert_eq!(sar["spec"]["uid"], "u-1");
        assert_eq!(sar["spec"]["groups"][1], "system:authenticated");
        assert_eq!(sar["spec"]["resourceAttributes"]["name"], NODE);
        assert_eq!(r.decide(NOPERM.as_bytes(), "get").await.unwrap(), Decision::Forbidden);
        assert_eq!(r.decide(b"forged", "get").await.unwrap(), Decision::Unauthenticated);
        assert_eq!(r.decide(&[0xff, 0xfe], "get").await.unwrap(), Decision::Unauthenticated);
        // Only `get` is granted.
        assert_eq!(r.decide(READER.as_bytes(), "create").await.unwrap(), Decision::Forbidden);
        assert_eq!(fake.reviews.load(Ordering::SeqCst), 4);

        // Cached: no new review within the TTL, for every outcome.
        for t in [READER, NOPERM, "forged"] {
            r.decide(t.as_bytes(), "get").await.unwrap();
        }
        assert_eq!(fake.reviews.load(Ordering::SeqCst), 4);

        // Expired: reviewed again.
        r.ttl = Duration::ZERO;
        assert_eq!(r.decide(READER.as_bytes(), "get").await.unwrap(), Decision::Allowed);
        assert_eq!(fake.reviews.load(Ordering::SeqCst), 5);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn an_apiserver_that_refuses_cadvisor_or_is_down_is_an_error_not_a_401() {
        let (fake, port) = plain_fake().await;
        let d = scratch("refuse");
        std::fs::write(d.join("token"), "not-cadvisors-token").unwrap();
        let r = Reviewer::new(&format!("http://127.0.0.1:{port}"), None, &d.join("token"), NODE).unwrap();
        let e = r.decide(READER.as_bytes(), "get").await.unwrap_err();
        assert!(format!("{e:#}").contains("401"), "{e:#}");
        // Errors are not cached: fixed token, next call succeeds.
        std::fs::write(d.join("token"), OWN).unwrap();
        let r = Reviewer::new(&format!("http://127.0.0.1:{port}"), None, &d.join("token"), NODE).unwrap();
        assert_eq!(r.decide(READER.as_bytes(), "get").await.unwrap(), Decision::Allowed);
        assert_eq!(fake.reviews.load(Ordering::SeqCst), 1);

        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let r = Reviewer::new(&format!("http://127.0.0.1:{dead}"), None, &d.join("token"), NODE).unwrap();
        assert!(r.decide(READER.as_bytes(), "get").await.is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn https_needs_a_ca_and_an_empty_token_will_not_start() {
        let d = scratch("startup");
        std::fs::write(d.join("token"), OWN).unwrap();
        std::fs::write(d.join("empty"), "\n").unwrap();
        assert!(Reviewer::new("https://127.0.0.1:6443", None, &d.join("token"), NODE).is_err());
        assert!(Reviewer::new("http://127.0.0.1:6443", None, &d.join("empty"), NODE).is_err());
        assert!(Reviewer::new("http://127.0.0.1:6443", None, &d.join("missing"), NODE).is_err());
        std::fs::write(d.join("ca"), "not a pem").unwrap();
        assert!(Reviewer::new("https://127.0.0.1:6443", Some(&d.join("ca")), &d.join("token"), NODE).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A self-signed certificate for 127.0.0.1: (cert PEM, key PEM).
    fn cert() -> (String, String) {
        let params = rcgen::CertificateParams::new(vec!["127.0.0.1".into(), "localhost".into()]).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let c = params.self_signed(&key).unwrap();
        (c.pem(), key.serialize_pem())
    }

    #[tokio::test]
    async fn https_checks_the_apiserver_against_the_ca() {
        let d = scratch("https");
        let (pem, key) = cert();
        std::fs::write(d.join("tls.crt"), &pem).unwrap();
        std::fs::write(d.join("tls.key"), &key).unwrap();
        std::fs::write(d.join("token"), OWN).unwrap();
        let acceptor = crate::secure::acceptor(&d.join("tls.crt"), &d.join("tls.key")).unwrap();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let fake = Arc::new(Fake::default());
        let app = fake_router(fake.clone());
        tokio::spawn(crate::secure::serve_tls(l, acceptor, app, std::future::pending()));

        let url = format!("https://127.0.0.1:{port}");
        let r = Reviewer::new(&url, Some(&d.join("tls.crt")), &d.join("token"), NODE).unwrap();
        assert_eq!(r.decide(READER.as_bytes(), "get").await.unwrap(), Decision::Allowed);

        // Another CA: the apiserver's certificate is refused.
        let (other, _) = cert();
        std::fs::write(d.join("other.crt"), other).unwrap();
        let r = Reviewer::new(&url, Some(&d.join("other.crt")), &d.join("token"), NODE).unwrap();
        let e = r.decide(READER.as_bytes(), "get").await.unwrap_err();
        assert!(format!("{e:#}").contains("tls"), "{e:#}");
        assert_eq!(fake.reviews.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
