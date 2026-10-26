//! stack-stack — single-binary homelab control plane.
//!
//! Phase 1 surface: host metrics collected straight from `/proc`, fanned out
//! over a WebSocket, plus the embedded frontend shell.

mod alerts;
mod audit;
mod auth;
mod bootstrap;
mod ca;
mod config;
mod containers;
mod error;
mod files;
mod jobs;
mod networking;
mod notifications;
mod notify;
mod proc;
mod proxy;
mod rand;
mod runner;
mod services;
mod state;
mod system;
mod tls;
mod virtualization;
mod web;

use config::Config;
use state::AppState;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    // Explicit runtime sizing. Workers match the CPU count; the blocking pool is
    // raised above the default so concurrent PTY sessions and Docker CLI calls
    // cannot starve each other.
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .max_blocking_threads(256)
        .thread_name("stack-stack")
        .enable_all()
        .build()?;

    tracing::info!(workers, blocking_threads = 256, "tokio runtime started");
    runtime.block_on(run())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load();
    let addr = cfg.addr();

    // Create the audit trail (mode 0600) and record this boot, so `tail -f` works
    // before the first destructive action.
    audit::init(&cfg.audit_path);

    // Seed any missing configuration document before the state is built, so a
    // fresh box has a complete /etc/stack-stack tree and the Developer page
    // never reports a document as missing. Best effort: a read-only box must
    // still boot, it just logs.
    match bootstrap::ensure_config_files(&cfg) {
        Ok(written) if !written.is_empty() => {
            tracing::info!(documents = ?written, "seeded default configuration documents");
        }
        Ok(_) => {}
        Err(err) => tracing::warn!(%err, "could not seed default configuration documents"),
    }

    let app_state = AppState::new(cfg.clone());
    web::ws::spawn_sampler(app_state.clone(), cfg.clone());
    web::auth::spawn_session_sweeper(app_state.clone());
    notify::spawn_threshold_watcher(app_state.notify.clone(), app_state.telemetry.subscribe());
    web::alerts::spawn_evaluator(app_state.clone());
    web::healthchecks::spawn_evaluator(app_state.clone());
    web::backup::spawn_scheduler(app_state.clone());
    web::smart::spawn_scheduler(app_state.clone());
    web::ddns::spawn_scheduler(app_state.clone());
    web::network::spawn_blocklist_scheduler(app_state.clone());
    web::updates::spawn_scheduler(app_state.clone());
    web::tasks::spawn_scheduler(app_state.clone());

    if cfg.tls_enabled {
        // The admin hostname needs a certificate before the first handshake, and
        // the resolver map must be populated before the listener accepts. Any
        // CA-managed leaf inside the renewal window is re-issued here too.
        if let Err(err) = tls::ensure_admin_cert(&app_state) {
            tracing::warn!(%err, "could not issue the admin certificate; HTTPS will use whatever is available");
        }
        let renewed = tls::renew_due_routes(&app_state);
        if !renewed.is_empty() {
            tracing::info!(?renewed, "renewed CA-managed certificates");
        }
        if let Err(err) = tls::refresh(&app_state) {
            tracing::warn!(%err, "could not build the TLS certificate map");
        }
        match app_state.tls.supervisor.start(app_state.clone()).await {
            Ok(()) => tracing::info!(
                http = cfg.tls_http_port,
                https = cfg.tls_https_port,
                admin_host = %cfg.tls_admin_hostname,
                "TLS front door listening"
            ),
            Err(err) => tracing::warn!(%err, "TLS front door could not start"),
        }
    }
    if !app_state.auth.configured() {
        tracing::warn!(
            path = %cfg.auth_path,
            "no admin password is configured; the UI is in first-run setup mode"
        );
    }

    let app = web::router(app_state);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!(
        %addr,
        version = env!("CARGO_PKG_VERSION"),
        profile = env!("STACK_STACK_BUILD_PROFILE"),
        assets = web::assets::FINGERPRINT,
        sample_ms = cfg.sample_interval_ms,
        "stack-stack listening"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("stack-stack stopped");
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

/// systemd stops us with SIGTERM; ctrl-c covers local runs.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::warn!(%err, "cannot install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("ctrl-c received, shutting down"),
        _ = terminate => tracing::info!("SIGTERM received, shutting down"),
    }
}
