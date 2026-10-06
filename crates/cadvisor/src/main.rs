//! cadvisor-rs server binary: wires the subsystems together and owns the
//! process lifecycle.
//!
//! Defaults and flag names mirror google/cadvisor v0.49.2 (`-listen_ip`,
//! `-housekeeping_interval`, `-containerd-namespace`, …). Each flag also
//! answers to the other spelling (`--listen-ip`, `--containerd_namespace`),
//! which stormcos's golden uses (#9). Go-style single-dash long flags
//! (`-port 8080`) are accepted via an argv preprocessor, and a boolean flag
//! given bare (`-store_container_labels`) means `true`, as in Go.

use clap::Parser;

#[cfg(target_os = "linux")]
mod secure;

pub const CADVISOR_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug, Clone)]
#[command(name = "cadvisor", version, about = "cAdvisor-compatible container monitoring daemon")]
pub struct Args {
    /// IP to listen on (empty = all interfaces)
    #[arg(long = "listen_ip", alias = "listen-ip", default_value = "")]
    pub listen_ip: String,

    /// Port to listen on
    #[arg(long, default_value_t = 8080)]
    pub port: u16,

    /// Interval between container housekeepings
    #[arg(long = "housekeeping_interval", alias = "housekeeping-interval", default_value = "1s")]
    pub housekeeping_interval: String,

    /// Largest interval to allow between container housekeepings
    #[arg(long = "max_housekeeping_interval", alias = "max-housekeeping-interval", default_value = "60s")]
    pub max_housekeeping_interval: String,

    /// Whether to allow the housekeeping interval to be dynamic
    #[arg(long = "allow_dynamic_housekeeping", alias = "allow-dynamic-housekeeping", default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub allow_dynamic_housekeeping: bool,

    /// Interval between global housekeepings (container discovery sweep)
    #[arg(long = "global_housekeeping_interval", alias = "global-housekeeping-interval", default_value = "1m0s")]
    pub global_housekeeping_interval: String,

    /// Interval between machine info updates (disk map, filesystems, memory)
    #[arg(long = "update_machine_info_interval", alias = "update-machine-info-interval", default_value = "5m0s")]
    pub update_machine_info_interval: String,

    /// How long to keep data stored
    #[arg(long = "storage_duration", alias = "storage-duration", default_value = "2m0s")]
    pub storage_duration: String,

    /// Comma-separated list of metric groups to disable
    #[arg(long = "disable_metrics", alias = "disable-metrics", default_value = "")]
    pub disable_metrics: String,

    /// Comma-separated list of metric groups to enable (overrides disable)
    #[arg(long = "enable_metrics", alias = "enable-metrics", default_value = "")]
    pub enable_metrics: String,

    /// Whether to convert container labels and env vars to prometheus labels
    #[arg(long = "store_container_labels", alias = "store-container-labels", default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub store_container_labels: bool,

    /// Comma-separated container labels to export when store_container_labels
    /// is false
    #[arg(long = "whitelisted_container_labels", alias = "whitelisted-container-labels", default_value = "")]
    pub whitelisted_container_labels: String,

    /// Comma-separated environment variable keys to export
    #[arg(long = "env_metadata_whitelist", alias = "env-metadata-whitelist", default_value = "")]
    pub env_metadata_whitelist: String,

    /// containerd endpoint
    #[arg(long, default_value = "/run/containerd/containerd.sock")]
    pub containerd: String,

    /// containerd namespace
    #[arg(long = "containerd-namespace", alias = "containerd_namespace", default_value = "k8s.io")]
    pub containerd_namespace: String,

    /// CRI-O endpoint
    #[arg(long, default_value = "/var/run/crio/crio.sock")]
    pub crio: String,

    /// PEM certificate chain (leaf first) to serve HTTPS with; needs
    /// --tls-key-file. Re-read when the file is replaced. Empty = plain HTTP
    #[arg(long = "tls-cert-file", alias = "tls_cert_file", default_value = "")]
    pub tls_cert_file: String,

    /// PEM private key for --tls-cert-file
    #[arg(long = "tls-key-file", alias = "tls_key_file", default_value = "")]
    pub tls_key_file: String,

    /// File of accepted bearer tokens, one per line (`#` comments). When set,
    /// every path except /healthz, /-/healthy and /-/ready needs
    /// `Authorization: Bearer <token>`. Re-read when the file changes.
    /// Empty = no auth
    #[arg(long = "bearer-token-file", alias = "bearer_token_file", default_value = "")]
    pub bearer_token_file: String,
}

/// Accepts Go-style single-dash long flags: `-port 8080` -> `--port 8080`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn go_style_argv(argv: impl IntoIterator<Item = String>) -> Vec<String> {
    argv.into_iter()
        .enumerate()
        .map(|(i, a)| {
            if i > 0
                && a.len() > 2
                && a.starts_with('-')
                && !a.starts_with("--")
                && !a[1..2].chars().all(|c| c.is_ascii_digit())
            {
                format!("-{a}")
            } else {
                a
            }
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn parse_duration(s: &str, flag: &str) -> anyhow::Result<std::time::Duration> {
    humantime::parse_duration(s).map_err(|e| anyhow::anyhow!("invalid {flag} {s:?}: {e}"))
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use anyhow::Context;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse_from(go_style_argv(std::env::args()));

    let cfg = cadvisor_manager::ManagerConfig {
        housekeeping_interval: parse_duration(&args.housekeeping_interval, "housekeeping_interval")?,
        max_housekeeping_interval: parse_duration(
            &args.max_housekeeping_interval,
            "max_housekeeping_interval",
        )?,
        allow_dynamic_housekeeping: args.allow_dynamic_housekeeping,
        global_housekeeping_interval: parse_duration(
            &args.global_housekeeping_interval,
            "global_housekeeping_interval",
        )?,
        update_machine_info_interval: parse_duration(
            &args.update_machine_info_interval,
            "update_machine_info_interval",
        )?,
        storage_duration: parse_duration(&args.storage_duration, "storage_duration")?,
        cadvisor_version: CADVISOR_VERSION.to_string(),
        containerd_socket: args.containerd.clone(),
        containerd_namespace: args.containerd_namespace.clone(),
        crio_socket: args.crio.clone(),
        ..Default::default()
    };

    // Checked before anything starts, so a bad certificate or token file is a
    // startup error rather than a server that refuses everyone.
    let tls = match (args.tls_cert_file.is_empty(), args.tls_key_file.is_empty()) {
        (true, true) => None,
        (false, false) => Some((
            std::path::PathBuf::from(&args.tls_cert_file),
            std::path::PathBuf::from(&args.tls_key_file),
        )),
        _ => anyhow::bail!("--tls-cert-file and --tls-key-file go together"),
    };
    let tokens = if args.bearer_token_file.is_empty() {
        None
    } else {
        Some(secure::tokens(std::path::Path::new(&args.bearer_token_file))?)
    };
    if tokens.is_some() && tls.is_none() {
        tracing::warn!("--bearer-token-file without TLS: tokens cross the network in clear");
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let manager =
                std::sync::Arc::new(cadvisor_manager::Manager::new(cfg).context("init manager")?);
            manager.start().await.context("start manager")?;
            tracing::info!(version = CADVISOR_VERSION, "cadvisor-rs started");

            // Upstream metric-group flags: enable overrides disable when set.
            let all_default_enabled = ["cpu", "percpu", "memory", "cpuLoad", "disk", "diskIO", "network", "app", "perf_event", "oom_event", "pressure"];
            let disabled_groups: std::collections::BTreeSet<String> =
                if args.enable_metrics.is_empty() {
                    args.disable_metrics
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                } else {
                    let enabled: Vec<&str> =
                        args.enable_metrics.split(',').map(str::trim).collect();
                    all_default_enabled
                        .iter()
                        .filter(|g| !enabled.contains(*g))
                        .map(|g| g.to_string())
                        .collect()
                };
            let metrics_opts = cadvisor_metrics::MetricsOpts {
                store_container_labels: args.store_container_labels,
                whitelisted_container_labels: args
                    .whitelisted_container_labels
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                disabled_groups,
            };

            let mut app = axum::Router::new()
                .route("/healthz", axum::routing::get(|| async { "ok" }))
                .route("/-/healthy", axum::routing::get(|| async { "ok" }))
                .route("/-/ready", axum::routing::get(|| async { "ok" }))
                .merge(cadvisor_metrics::router(manager.clone(), metrics_opts))
                .merge(cadvisor_api::router(manager.clone()));
            if let Some(tokens) = tokens {
                app = app.layer(axum::middleware::from_fn_with_state(tokens, secure::require_bearer));
            }

            let ip = if args.listen_ip.is_empty() { "0.0.0.0" } else { &args.listen_ip };
            let addr = format!("{ip}:{}", args.port);
            let listener = tokio::net::TcpListener::bind(&addr)
                .await
                .with_context(|| format!("bind {addr}"))?;
            let shutdown = async {
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("shutting down");
            };
            match tls {
                Some((cert, key)) => {
                    let acceptor = secure::acceptor(&cert, &key)?;
                    tracing::info!(addr, "serving https");
                    secure::serve_tls(listener, acceptor, app, shutdown).await?;
                }
                None => {
                    tracing::info!(addr, "serving");
                    axum::serve(listener, app).with_graceful_shutdown(shutdown).await?;
                }
            }
            Ok(())
        })
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cadvisor-rs {CADVISOR_VERSION}: Linux only (cgroup v2 required)");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Args {
        let argv = std::iter::once("cadvisor").chain(argv.iter().copied()).map(String::from);
        Args::try_parse_from(go_style_argv(argv)).unwrap()
    }

    #[test]
    fn upstream_argv() {
        let a = parse(&[
            "-listen_ip", "0.0.0.0",
            "-port", "9096",
            "-housekeeping_interval", "10s",
            "-max_housekeeping_interval=15s",
            "-allow_dynamic_housekeeping=false",
            "-global_housekeeping_interval", "2m",
            "-storage_duration", "5m",
            "-update_machine_info_interval", "30s",
            "-disable_metrics", "percpu,disk",
            "-enable_metrics=cpu",
            "-store_container_labels=false",
            "-whitelisted_container_labels", "io.kubernetes.pod.name",
            "-env_metadata_whitelist", "FOO",
            "-containerd", "/run/c.sock",
            "-containerd-namespace", "moby",
            "-crio", "/run/crio.sock",
        ]);
        assert_eq!(a.listen_ip, "0.0.0.0");
        assert_eq!(a.port, 9096);
        assert_eq!(a.housekeeping_interval, "10s");
        assert_eq!(a.max_housekeeping_interval, "15s");
        assert!(!a.allow_dynamic_housekeeping);
        assert_eq!(a.global_housekeeping_interval, "2m");
        assert_eq!(a.storage_duration, "5m");
        assert_eq!(a.update_machine_info_interval, "30s");
        assert_eq!(a.disable_metrics, "percpu,disk");
        assert_eq!(a.enable_metrics, "cpu");
        assert!(!a.store_container_labels);
        assert_eq!(a.whitelisted_container_labels, "io.kubernetes.pod.name");
        assert_eq!(a.env_metadata_whitelist, "FOO");
        assert_eq!(a.containerd, "/run/c.sock");
        assert_eq!(a.containerd_namespace, "moby");
        assert_eq!(a.crio, "/run/crio.sock");
    }

    #[test]
    fn kebab_aliases_still_work() {
        // stormcos's golden argv.
        let a = parse(&["--port", "9096", "--listen-ip", "0.0.0.0"]);
        assert_eq!((a.port, a.listen_ip.as_str()), (9096, "0.0.0.0"));
        let a = parse(&[
            "--housekeeping-interval", "2s",
            "--allow-dynamic-housekeeping", "false",
            "--containerd_namespace", "x",
            "--tls_cert_file", "c",
            "--tls-key-file", "k",
        ]);
        assert_eq!(a.housekeeping_interval, "2s");
        assert!(!a.allow_dynamic_housekeeping);
        assert_eq!(a.containerd_namespace, "x");
        assert_eq!((a.tls_cert_file.as_str(), a.tls_key_file.as_str()), ("c", "k"));
    }

    #[test]
    fn bare_bool_means_true() {
        let a = parse(&["-store_container_labels", "-allow_dynamic_housekeeping", "-port", "1"]);
        assert!(a.store_container_labels && a.allow_dynamic_housekeeping);
        assert_eq!(a.port, 1);
    }

    #[test]
    fn defaults() {
        let a = parse(&[]);
        assert_eq!(a.listen_ip, "");
        assert_eq!(a.port, 8080);
        assert_eq!(a.containerd_namespace, "k8s.io");
        assert_eq!(a.update_machine_info_interval, "5m0s");
        assert!(a.store_container_labels && a.allow_dynamic_housekeeping);
    }
}
