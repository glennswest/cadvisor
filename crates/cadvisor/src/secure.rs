//! TLS and bearer-token auth for the main listener (#4).
//!
//! Both are off by default, as in upstream cadvisor. On stormcos the golden
//! is to turn both on (stormcos#143, not done yet): a stormcert-issued
//! certificate (`--tls-cert-file`, `--tls-key-file`) and a token file
//! (`--bearer-token-file`). With a token
//! file, every path needs `Authorization: Bearer <token>` except the health
//! paths, which stormd probes without one.
//!
//! The certificate, key and tokens are files that get replaced (renewal,
//! rotation). Each is re-read when its mtime, size or inode changes, checked
//! at most every [`RECHECK`]. A replacement that fails to load is logged and
//! the previous one stays in use, and it is retried: stormcert-agent renames
//! the new key into place before the new certificate, so for a moment the
//! pair does not match. At startup a bad file is fatal.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

/// How often a watched file's metadata is looked at.
pub const RECHECK: Duration = Duration::from_secs(5);

type Stamp = Vec<Option<(i64, i64, u64, u64)>>;

fn stamp(paths: &[PathBuf]) -> Stamp {
    paths
        .iter()
        .map(|p| std::fs::metadata(p).ok().map(|m| (m.mtime(), m.mtime_nsec(), m.len(), m.ino())))
        .collect()
}

/// A value loaded from files, reloaded when they change.
pub struct Watched<T> {
    what: &'static str,
    paths: Vec<PathBuf>,
    load: fn(&[PathBuf]) -> anyhow::Result<T>,
    state: Mutex<(T, Stamp, Instant)>,
}

impl<T: Clone> Watched<T> {
    /// Loads now; an error here is the caller's startup error.
    pub fn new(
        what: &'static str,
        paths: Vec<PathBuf>,
        load: fn(&[PathBuf]) -> anyhow::Result<T>,
    ) -> anyhow::Result<Self> {
        let st = stamp(&paths);
        let value = load(&paths)?;
        Ok(Self { what, paths, load, state: Mutex::new((value, st, Instant::now())) })
    }

    pub fn get(&self) -> T {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if g.2.elapsed() >= RECHECK {
            g.2 = Instant::now();
            let st = stamp(&self.paths);
            if st != g.1 {
                match (self.load)(&self.paths) {
                    Ok(v) => {
                        tracing::info!(what = self.what, "reloaded");
                        g.0 = v;
                        g.1 = st;
                    }
                    // The stamp is not updated, so it is retried next check:
                    // a certificate and key replaced one after the other can
                    // briefly disagree.
                    Err(e) => tracing::warn!(what = self.what, error = %format!("{e:#}"), "reload failed; keeping the previous one"),
                }
            }
        }
        g.0.clone()
    }
}

impl<T> std::fmt::Debug for Watched<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watched").field("what", &self.what).field("paths", &self.paths).finish()
    }
}

// ── TLS ────────────────────────────────────────────────────────────────────

fn load_cert(paths: &[PathBuf]) -> anyhow::Result<Arc<CertifiedKey>> {
    let (cert, key) = (&paths[0], &paths[1]);
    let chain = CertificateDer::pem_file_iter(cert)
        .with_context(|| format!("read {}", cert.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("parse {}", cert.display()))?;
    if chain.is_empty() {
        bail!("{}: no certificate", cert.display());
    }
    let key = PrivateKeyDer::from_pem_file(key).with_context(|| format!("read a private key from {}", key.display()))?;
    let signing = rustls::crypto::ring::sign::any_supported_type(&key).context("unsupported private key type")?;
    let ck = CertifiedKey::new(chain, signing);
    ck.keys_match().context("the private key does not match the certificate")?;
    Ok(Arc::new(ck))
}

#[derive(Debug)]
struct Resolver(Watched<Arc<CertifiedKey>>);

impl ResolvesServerCert for Resolver {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.get())
    }
}

/// A TLS acceptor serving the certificate in `cert` (PEM chain, leaf first)
/// with the key in `key`, both re-read when replaced.
pub fn acceptor(cert: &Path, key: &Path) -> anyhow::Result<tokio_rustls::TlsAcceptor> {
    let watched = Watched::new("tls certificate", vec![cert.to_path_buf(), key.to_path_buf()], load_cert)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("tls protocol versions")?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(Resolver(watched)));
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(cfg)))
}

// ── bearer tokens ──────────────────────────────────────────────────────────

pub type Tokens = Arc<Vec<Vec<u8>>>;

/// One token per line; blank lines and lines starting with `#` are skipped.
pub fn parse_tokens(text: &str) -> Vec<Vec<u8>> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.as_bytes().to_vec())
        .collect()
}

fn load_tokens(paths: &[PathBuf]) -> anyhow::Result<Tokens> {
    let text = std::fs::read_to_string(&paths[0]).with_context(|| format!("read {}", paths[0].display()))?;
    let tokens = parse_tokens(&text);
    if tokens.is_empty() {
        bail!("{}: no tokens (every request would be refused)", paths[0].display());
    }
    Ok(Arc::new(tokens))
}

pub fn tokens(path: &Path) -> anyhow::Result<Arc<Watched<Tokens>>> {
    Ok(Arc::new(Watched::new("bearer tokens", vec![path.to_path_buf()], load_tokens)?))
}

/// Equal without an early exit on the first differing byte.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The token in an `Authorization: Bearer <token>` header (scheme is
/// case-insensitive, per RFC 7235).
pub fn bearer(h: Option<&HeaderValue>) -> Option<&[u8]> {
    let v = h?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token.as_bytes())
}

pub fn accepted(tokens: &[Vec<u8>], presented: Option<&[u8]>) -> bool {
    presented.is_some_and(|p| tokens.iter().any(|t| ct_eq(t, p)))
}

/// Answered without a token: stormd's liveness probe sends none (it speaks
/// https, unverified), as with the kubelet's and stormblock's health paths.
pub const HEALTH_PATHS: [&str; 3] = ["/healthz", "/-/healthy", "/-/ready"];

/// Middleware: `401` with `WWW-Authenticate: Bearer` unless the request is
/// for a health path or carries one of the tokens. Unknown paths get `401`
/// too, so an anonymous client learns nothing about what is served.
pub async fn require_bearer(State(tokens): State<Arc<Watched<Tokens>>>, req: Request, next: Next) -> Response {
    if HEALTH_PATHS.contains(&req.uri().path())
        || accepted(&tokens.get(), bearer(req.headers().get(header::AUTHORIZATION)))
    {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")], "Unauthorized\n").into_response()
    }
}

// ── serving ────────────────────────────────────────────────────────────────

/// Serves `app` over TLS on `listener` until `shutdown` resolves. In-flight
/// connections are not drained (as with SIGTERM on the plain listener).
pub async fn serve_tls(
    listener: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    app: axum::Router,
    shutdown: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder;
    use hyper_util::service::TowerToHyperService;

    tokio::pin!(shutdown);
    loop {
        let (tcp, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(c) => c,
                Err(e) => {
                    // EMFILE and friends: back off rather than spin.
                    tracing::warn!(error = %e, "accept");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut shutdown => return Ok(()),
        };
        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            let tls = match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => return tracing::debug!(%peer, error = %e, "tls handshake"),
                Err(_) => return tracing::debug!(%peer, "tls handshake timed out"),
            };
            let svc = TowerToHyperService::new(app);
            if let Err(e) = Builder::new(TokioExecutor::new()).serve_connection_with_upgrades(TokioIo::new(tls), svc).await {
                tracing::debug!(%peer, error = %e, "connection");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_skip_blanks_and_comments() {
        assert_eq!(parse_tokens("# scraper\nabc\n\n  def  \n"), vec![b"abc".to_vec(), b"def".to_vec()]);
    }

    #[test]
    fn bearer_scheme_is_case_insensitive_and_needs_a_token() {
        let h = |s: &str| HeaderValue::from_str(s).unwrap();
        assert_eq!(bearer(Some(&h("Bearer abc"))), Some(&b"abc"[..]));
        assert_eq!(bearer(Some(&h("bearer  abc "))), Some(&b"abc"[..]));
        assert_eq!(bearer(Some(&h("Basic abc"))), None);
        assert_eq!(bearer(Some(&h("Bearer "))), None);
        assert_eq!(bearer(None), None);
    }

    #[test]
    fn only_a_listed_token_is_accepted() {
        let t = vec![b"abc".to_vec(), b"defg".to_vec()];
        assert!(accepted(&t, Some(b"defg")));
        assert!(!accepted(&t, Some(b"ab")));
        assert!(!accepted(&t, Some(b"abd")));
        assert!(!accepted(&t, None));
    }

    #[test]
    fn a_replaced_token_file_is_reread_and_a_bad_one_is_not_used() {
        let dir = std::env::temp_dir().join(format!("cadvisor-tokens-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("tokens");
        std::fs::write(&p, "one\n").unwrap();
        let w = Watched::new("t", vec![p.clone()], load_tokens).unwrap();
        assert_eq!(*w.get(), vec![b"one".to_vec()]);

        std::fs::write(&p, "two-rotated\n").unwrap();
        w.state.lock().unwrap().2 -= RECHECK;
        assert_eq!(*w.get(), vec![b"two-rotated".to_vec()]);

        std::fs::write(&p, "# emptied\n").unwrap();
        w.state.lock().unwrap().2 -= RECHECK;
        assert_eq!(*w.get(), vec![b"two-rotated".to_vec()], "an empty file keeps the previous tokens");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_empty_token_file_is_a_startup_error() {
        let dir = std::env::temp_dir().join(format!("cadvisor-tokens-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("tokens");
        std::fs::write(&p, "\n# nothing\n").unwrap();
        assert!(tokens(&p).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
