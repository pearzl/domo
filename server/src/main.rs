mod agents;
mod api;
mod config;
mod db;
mod domain;
mod events;
mod git;
mod models;
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
    let state = AppState {
        db,
        cfg: cfg.clone(),
        bus,
        notify,
        schedule_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        git_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
    };

    {
        let state = state.clone();
        tokio::spawn(async move {
            scheduler::run(state, notify_rx).await;
        });
    }

    let web_dist = cfg.web_dist.clone();
    let index = web_dist.join("index.html");
    let app = api::router(state)
        .fallback_service(ServeDir::new(&web_dist).fallback(ServeFile::new(index)));
    let address = format!("{}:{}", cfg.bind, cfg.port);
    let listener = tokio::net::TcpListener::bind(&address).await?;
    tracing::info!(
        address = %address,
        data = %cfg.data_root.display(),
        workspace = %cfg.workspace_root.display(),
        log = %cfg.log_root.display(),
        web = %web_dist.display(),
        "DOMO 已启动"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
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
        _ = terminate => {}
    }
    tracing::info!("DOMO 正在关闭");
    tokio::time::sleep(Duration::from_millis(100)).await;
}
