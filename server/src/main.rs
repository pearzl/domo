mod activity;
mod agents;
mod api;
mod artifacts;
mod config;
mod db;
mod domain;
mod event_preview;
mod events;
mod failure;
mod git;
mod models;
mod opencode;
mod paths;
mod platform;
mod prompts;
mod replan;
mod runner;
mod scheduler;
mod state;
mod usage;
mod version;

use std::time::Duration;

use tower_http::services::{ServeDir, ServeFile};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::events::EventBus;
use crate::state::AppState;

/// Upper bound for draining in-flight connections after shutdown begins. Agents
/// are already terminated at that point, so only HTTP responses can still be
/// finishing.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::from_env()?;
    tokio::fs::create_dir_all(&cfg.data_root).await?;
    tokio::fs::create_dir_all(&cfg.workspace_root).await?;
    tokio::fs::create_dir_all(&cfg.log_root).await?;

    let file_appender = tracing_appender::rolling::daily(&cfg.log_root, "domo.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("domo_server=info,tower_http=warn")),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false),
        )
        .init();

    let db = db::connect(&cfg.db_path()).await?;
    let bus = EventBus::new();
    let (notify, notify_rx) = tokio::sync::mpsc::channel(64);
    let state = AppState::new(db, cfg.clone(), bus, notify);

    {
        let state = state.clone();
        tokio::spawn(async move {
            scheduler::run(state, notify_rx).await;
        });
    }

    {
        let state = state.clone();
        tokio::spawn(async move {
            wait_for_shutdown_signal().await;
            tracing::info!("DOMO 正在关闭");
            state.begin_shutdown();
            state.kill_active_jobs();
            // A second signal skips the grace period and always wins, so an
            // unresponsive connection cannot trap the process.
            wait_for_shutdown_signal().await;
            tracing::warn!("再次收到关闭信号，强制退出");
            state.kill_active_jobs();
            std::process::exit(130);
        });
    }

    let web_dist = cfg.web_dist.clone();
    let index = web_dist.join("index.html");
    let app = api::router(state.clone())
        .fallback_service(ServeDir::new(&web_dist).fallback(ServeFile::new(index)));
    let (listener, port) = bind_listener(&cfg.bind, cfg.port, !cfg.port_explicit).await?;
    let address = format!("{}:{}", cfg.bind, port);
    tracing::info!(
        address = %address,
        data = %cfg.data_root.display(),
        workspace = %cfg.workspace_root.display(),
        log = %cfg.log_root.display(),
        web = %web_dist.display(),
        "DOMO 已启动"
    );
    let serve = axum::serve(listener, app).with_graceful_shutdown({
        let state = state.clone();
        async move { state.wait_for_shutdown().await }
    });
    let drain_timeout = {
        let state = state.clone();
        async move {
            state.wait_for_shutdown().await;
            tokio::time::sleep(SHUTDOWN_TIMEOUT).await;
        }
    };
    let serve_result = tokio::select! {
        result = serve => Some(result),
        _ = drain_timeout => None,
    };
    state.kill_active_jobs();
    match serve_result {
        Some(result) => result?,
        None => {
            tracing::warn!(timeout = ?SHUTDOWN_TIMEOUT, "关闭超时，强制退出");
            std::process::exit(0);
        }
    }
    tracing::info!("DOMO 已关闭");
    Ok(())
}

/// How many ports above the default are tried before the OS assigns one.
const PORT_FALLBACK_WINDOW: u16 = 32;

/// Binds the requested port. When `fallback` is set and the port is taken, a
/// nearby free port is used instead so a stale instance cannot block startup.
async fn bind_listener(
    bind: &str,
    port: u16,
    fallback: bool,
) -> anyhow::Result<(tokio::net::TcpListener, u16)> {
    match tokio::net::TcpListener::bind((bind, port)).await {
        Ok(listener) => Ok((listener, port)),
        Err(err) if fallback && err.kind() == std::io::ErrorKind::AddrInUse => {
            let (listener, actual) = bind_free_port(bind, port).await?;
            tracing::warn!(requested = port, actual, "默认端口已被占用，改用空闲端口");
            Ok((listener, actual))
        }
        Err(err) => Err(err.into()),
    }
}

async fn bind_free_port(bind: &str, port: u16) -> anyhow::Result<(tokio::net::TcpListener, u16)> {
    let start = port.saturating_add(1);
    let end = start.saturating_add(PORT_FALLBACK_WINDOW);
    for candidate in start..=end {
        if let Ok(listener) = tokio::net::TcpListener::bind((bind, candidate)).await {
            return Ok((listener, candidate));
        }
    }
    let listener = tokio::net::TcpListener::bind((bind, 0)).await?;
    let actual = listener.local_addr()?.port();
    Ok((listener, actual))
}

async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(windows)]
    let ctrl_break = async {
        if let Ok(mut signal) = tokio::signal::windows::ctrl_break() {
            signal.recv().await;
        }
    };
    #[cfg(not(windows))]
    let ctrl_break = std::future::pending::<()>();
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = ctrl_break => {}
        _ = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn occupied_port() -> (tokio::net::TcpListener, u16) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    #[tokio::test]
    async fn binds_requested_port_when_free() {
        let (occupied, port) = occupied_port().await;
        drop(occupied);
        let (_listener, actual) = bind_listener("127.0.0.1", port, false).await.unwrap();
        assert_eq!(actual, port);
    }

    #[tokio::test]
    async fn uses_free_port_when_default_is_taken() {
        let (_occupied, port) = occupied_port().await;
        let (_listener, actual) = bind_listener("127.0.0.1", port, true).await.unwrap();
        assert_ne!(actual, port);
    }

    #[tokio::test]
    async fn fails_when_explicit_port_is_taken() {
        let (_occupied, port) = occupied_port().await;
        assert!(bind_listener("127.0.0.1", port, false).await.is_err());
    }
}
