//! stack-stack — single-binary homelab control plane.
//!
//! Phase 1 surface: host metrics collected straight from `/proc`, fanned out
//! over a WebSocket, plus the embedded frontend shell.

mod config;
mod containers;
mod error;
mod networking;
mod proc;
mod runner;
mod state;
mod system;
mod virtualization;
mod web;

use config::Config;
use state::AppState;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let cfg = Config::load();
    let addr = cfg.addr();

    let app_state = AppState::new(cfg.clone());
    web::ws::spawn_sampler(app_state.clone(), cfg.clone());

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
