//! #4: the real binary with `--tls-cert-file`, `--tls-key-file` and
//! `--bearer-token-file`. Speaks HTTPS only, refuses a missing or wrong
//! token with 401, answers the health paths without one, and picks up a
//! rotated token file and a renewed certificate without a restart.

#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::{CertificateDer, ServerName};

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

/// A self-signed certificate for `localhost`/127.0.0.1: (cert PEM, key PEM, DER).
fn cert(cn: &str) -> (String, String, CertificateDer<'static>) {
    let mut params = rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    params.distinguished_name.push(rcgen::DnType::CommonName, cn);
    let key = rcgen::KeyPair::generate().unwrap();
    let c = params.self_signed(&key).unwrap();
    (c.pem(), key.serialize_pem(), c.der().clone())
}

/// Replace a file by rename, as stormcert-agent does.
fn replace(path: &Path, text: &str) {
    let tmp = path.with_extension("new");
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Accepts any certificate and records the one the server presented.
#[derive(Debug)]
struct Record(Arc<std::sync::Mutex<Option<CertificateDer<'static>>>>, Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for Record {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        *self.0.lock().unwrap() = Some(end_entity.clone().into_owned());
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.1.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.1.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.1.signature_verification_algorithms.supported_schemes()
    }
}

/// GET `path` over TLS: (status, the server's certificate).
fn get(port: u16, path: &str, token: Option<&str>) -> std::io::Result<(u16, CertificateDer<'static>)> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let seen = Arc::new(std::sync::Mutex::new(None));
    let cfg = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Record(seen.clone(), provider)))
        .with_no_client_auth();
    let mut conn = rustls::ClientConnection::new(Arc::new(cfg), ServerName::try_from("localhost").unwrap()).unwrap();
    let mut tcp = TcpStream::connect(("127.0.0.1", port))?;
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(tls, "GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n")?;
    let mut buf = Vec::new();
    let _ = tls.read_to_end(&mut buf);
    let head = String::from_utf8_lossy(&buf);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::other(format!("no status line in {head:?}")))?;
    let cert = seen.lock().unwrap().clone().expect("a certificate");
    Ok((status, cert))
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
fn tls_and_bearer_tokens_with_rotation() {
    let dir = scratch("tls");
    let (cert_pem, key_pem, first) = cert("cadvisor-1");
    let (crt, key, tokens) = (dir.join("cadvisor.crt"), dir.join("cadvisor.key"), dir.join("tokens"));
    std::fs::write(&crt, &cert_pem).unwrap();
    std::fs::write(&key, &key_pem).unwrap();
    std::fs::write(&tokens, "# scraper\nfirst-token\n").unwrap();

    let port = free_port();
    let _cad = Kill(
        Command::new(env!("CARGO_BIN_EXE_cadvisor"))
            .args(["--listen-ip", "127.0.0.1", "--port", &port.to_string()])
            .arg("--tls-cert-file")
            .arg(&crt)
            .arg("--tls-key-file")
            .arg(&key)
            .arg("--bearer-token-file")
            .arg(&tokens)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    wait_until("https /healthz", 30, || matches!(get(port, "/healthz", None), Ok((200, _))));

    // Plain HTTP on the TLS port gets no HTTP answer.
    let mut plain = TcpStream::connect(("127.0.0.1", port)).unwrap();
    plain.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    plain.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut b = Vec::new();
    let _ = plain.read_to_end(&mut b);
    assert!(!b.starts_with(b"HTTP/"), "plain HTTP was answered: {:?}", String::from_utf8_lossy(&b));

    for p in ["/healthz", "/-/healthy", "/-/ready"] {
        assert_eq!(get(port, p, None).unwrap().0, 200, "{p} without a token");
    }
    for p in ["/metrics", "/api/v2.0/version", "/api/v1.3/machine", "/api/", "/nope"] {
        assert_eq!(get(port, p, None).unwrap().0, 401, "{p} without a token");
        assert_eq!(get(port, p, Some("wrong")).unwrap().0, 401, "{p} with a wrong token");
    }
    let (s, served) = get(port, "/metrics", Some("first-token")).unwrap();
    assert_eq!(s, 200);
    assert_eq!(served, first);
    assert_eq!(get(port, "/api/v2.0/version", Some("first-token")).unwrap().0, 200);

    // Rotate the token file: the new token is accepted, the old one refused.
    replace(&tokens, "second-token\n");
    wait_until("the rotated token", 15, || get(port, "/metrics", Some("second-token")).unwrap().0 == 200);
    assert_eq!(get(port, "/metrics", Some("first-token")).unwrap().0, 401);

    // Renew the certificate the way stormcert-agent does, key first: the
    // mismatched pair in between is not applied, the renewed one is.
    let (cert2, key2, second) = cert("cadvisor-2");
    replace(&key, &key2);
    std::thread::sleep(Duration::from_secs(6));
    let (s, mid) = get(port, "/healthz", None).unwrap();
    assert_eq!(s, 200, "a mismatched pair must not break serving");
    assert_eq!(mid, first, "the mismatched pair was applied");
    replace(&crt, &cert2);
    wait_until("the renewed certificate", 15, || get(port, "/healthz", None).map(|(_, c)| c == second).unwrap_or(false));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn half_a_tls_pair_or_an_empty_token_file_will_not_start() {
    let dir = scratch("tls-bad");
    let (cert_pem, _, _) = cert("x");
    let crt = dir.join("c.crt");
    std::fs::write(&crt, cert_pem).unwrap();
    let empty = dir.join("tokens");
    std::fs::write(&empty, "# none\n").unwrap();

    let run = |args: &[&std::ffi::OsStr]| {
        Command::new(env!("CARGO_BIN_EXE_cadvisor"))
            .args(["--listen-ip", "127.0.0.1", "--port", &free_port().to_string()])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
    };
    assert!(!run(&["--tls-cert-file".as_ref(), crt.as_os_str()]).success());
    assert!(!run(&["--bearer-token-file".as_ref(), empty.as_os_str()]).success());
    let _ = std::fs::remove_dir_all(&dir);
}
