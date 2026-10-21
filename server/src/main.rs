mod agents;
mod api;
mod db;
mod events;
mod host;
mod knowledge;
mod models;
mod recovery;
mod replan;
mod pool;
mod report;
mod sandbox;
mod scheduler;
mod state;
mod verification;

use std::sync::Arc;
use state::App;

/// 停机信号：Ctrl-C 或 SIGTERM（systemd / 容器 / 冒烟脚本清理）。
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[tokio::main]
async fn main() {
    // runtime logs: stdout + data/domo.log (append). DOMO_LOG 可覆盖。
    let log_path = std::env::var("DOMO_LOG").unwrap_or_else(|_| {
        std::path::Path::new(&std::env::var("DOMO_DB").unwrap_or_else(|_| "data/domo.db".into()))
            .parent()
            .map(|d| d.join("domo.log"))
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "data/domo.log".into())
    });
    let fmt_stdout = tracing_subscriber::fmt::layer().with_target(false).compact();
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    // 默认 info，避免 hyper/tower 的 TRACE 连接噪音；DOMO_LOG_LEVEL 可覆盖（RUST_LOG 兼容）
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            std::env::var("DOMO_LOG_LEVEL").unwrap_or_else(|_| "info".into()),
        )
    });
    let std_layer = tracing_subscriber::registry().with(filter).with(fmt_stdout);
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => {
            let file_layer = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_target(false)
                .compact()
                .with_writer(std::sync::Mutex::new(f));
            std_layer.with(file_layer).init();
        }
        Err(e) => {
            std_layer.init();
            eprintln!("warning: cannot open log file {log_path}: {e} (stdout only)");
        }
    }
    tracing::info!(%log_path, "domo-server starting");

    let db_path = std::env::var("DOMO_DB").unwrap_or_else(|_| "data/domo.db".into());
    let conn = db::open(&db_path);
    let port: u16 = std::env::var("DOMO_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8787);
    let (tx, _) = tokio::sync::broadcast::channel::<(String, serde_json::Value)>(1024);
    let app = Arc::new(App {
        db: std::sync::Mutex::new(conn),
        tx,
        handles: Default::default(),
        circuits: Default::default(),
        handle_seq: Default::default(),
        wake: Default::default(),
        sessions: Default::default(),
        stopping: Default::default(),
    });

    // ARCH §8.3：先对账（重复收件箱 / 遗留宿主），再启动调度器。
    scheduler::reconcile(&app);
    tokio::spawn(scheduler::run(app.clone()));

    let mut router = api::router(app.clone());
    // static SPA: resolve web/dist relative to cwd or the server crate dir (via executable path)
    let exe_up = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("../../../web/dist")));
    let dist = ["web/dist", "../web/dist"]
        .iter()
        .map(std::path::PathBuf::from)
        .chain(exe_up.into_iter())
        .find(|p| p.join("index.html").exists());
    if let Some(dist) = dist {
        use tower_http::services::{ServeDir, ServeFile};
        let index = dist.join("index.html");
        router = router.fallback_service(
            ServeDir::new(dist)
                .append_index_html_on_directories(true)
                .fallback(ServeFile::new(index)),
        );
    } else {
        router = router.fallback_service(axum::routing::get(|| async {
            axum::response::Html(
                "DOMO backend is running. The web frontend is not built — run `npm run build` in web/ and restart to enable static hosting.",
            )
        }));
    }

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind port");
    // Ctrl-C 优雅停机（ARCH §8.4）：冻结派发 → 取消 worker → 回收宿主 → WAL 检查点。
    let shutdown_state = app.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("shutdown requested; freezing scheduler");
        shutdown_state.begin_shutdown();
        shutdown_state.abort_all_handles();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        crate::host::stop_all();
        {
            let c = shutdown_state.db.lock().unwrap();
            db::checkpoint(&c);
        }
        tracing::info!("graceful shutdown complete");
        std::process::exit(0);
    });
    println!("domo-server listening on http://{addr} (db: {db_path})");
    axum::serve(listener, router).await.expect("server");
}
