//! cadvisor-rs server binary: wires the subsystems together and owns the
//! process lifecycle.
//!
//! Flag names and defaults mirror google/cadvisor v0.49.2 so it is a drop-in
//! replacement. Go-style single-dash long flags (`-port 8080`) are accepted
//! via an argv preprocessor.

use clap::Parser;

pub const CADVISOR_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug, Clone)]
#[command(name = "cadvisor", version, about = "cAdvisor-compatible container monitoring daemon")]
pub struct Args {
    /// IP to listen on (empty = all interfaces)
    #[arg(long, default_value = "")]
    pub listen_ip: String,

    /// Port to listen on
    #[arg(long, default_value_t = 8080)]
    pub port: u16,

    /// Interval between container housekeepings
    #[arg(long, default_value = "1s")]
    pub housekeeping_interval: String,

    /// Largest interval to allow between container housekeepings
    #[arg(long, default_value = "60s")]
    pub max_housekeeping_interval: String,

    /// Whether to allow the housekeeping interval to be dynamic
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub allow_dynamic_housekeeping: bool,

    /// Interval between global housekeepings (container discovery sweep)
    #[arg(long, default_value = "1m0s")]
    pub global_housekeeping_interval: String,

    /// How long to keep data stored
    #[arg(long, default_value = "2m0s")]
    pub storage_duration: String,

    /// Comma-separated list of metric groups to disable
    #[arg(long, default_value = "")]
    pub disable_metrics: String,

    /// Comma-separated list of metric groups to enable (overrides disable)
    #[arg(long, default_value = "")]
    pub enable_metrics: String,

    /// Whether to convert container labels and env vars to prometheus labels
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub store_container_labels: bool,

    /// Comma-separated container labels to export when store_container_labels
    /// is false
    #[arg(long, default_value = "")]
    pub whitelisted_container_labels: String,

    /// Comma-separated environment variable keys to export
    #[arg(long, default_value = "")]
    pub env_metadata_whitelist: String,

    /// containerd endpoint
    #[arg(long, default_value = "/run/containerd/containerd.sock")]
    pub containerd: String,

    /// containerd namespace
    #[arg(long, default_value = "k8s.io")]
    pub containerd_namespace: String,

    /// CRI-O endpoint
    #[arg(long, default_value = "/var/run/crio/crio.sock")]
    pub crio: String,
}

/// Accepts Go-style single-dash long flags: `-port 8080` -> `--port 8080`.
#[cfg(target_os = "linux")]
fn go_style_argv() -> Vec<String> {
    std::env::args()
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

    let args = Args::parse_from(go_style_argv());

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
        storage_duration: parse_duration(&args.storage_duration, "storage_duration")?,
        cadvisor_version: CADVISOR_VERSION.to_string(),
        containerd_socket: args.containerd.clone(),
        containerd_namespace: args.containerd_namespace.clone(),
        crio_socket: args.crio.clone(),
        ..Default::default()
    };

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

            let app = axum::Router::new()
                .route("/healthz", axum::routing::get(|| async { "ok" }))
                .route("/-/healthy", axum::routing::get(|| async { "ok" }))
                .route("/-/ready", axum::routing::get(|| async { "ok" }))
                .merge(cadvisor_metrics::router(manager.clone(), metrics_opts))
                .merge(cadvisor_api::router(manager.clone()));

            let ip = if args.listen_ip.is_empty() { "0.0.0.0" } else { &args.listen_ip };
            let addr = format!("{ip}:{}", args.port);
            let listener = tokio::net::TcpListener::bind(&addr)
                .await
                .with_context(|| format!("bind {addr}"))?;
            tracing::info!(addr, "serving");
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                    tracing::info!("shutting down");
                })
                .await?;
            Ok(())
        })
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cadvisor-rs {CADVISOR_VERSION}: Linux only (cgroup v2 required)");
    std::process::exit(1);
}
