//! Shared runtime state.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use sqlx::SqlitePool;
use tokio::sync::{mpsc, watch, Mutex};

use crate::config::Config;
use crate::events::EventBus;

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub cfg: Config,
    pub bus: EventBus,
    pub notify: mpsc::Sender<()>,
    pub schedule_lock: Arc<Mutex<()>>,
    pub git_lock: Arc<Mutex<()>>,
    /// Set once the process starts shutting down. Long-lived streams and the
    /// scheduler watch this flag so they can stop instead of keeping the server
    /// alive.
    shutdown: watch::Sender<bool>,
    /// Job directories of agents started by this process. Shutdown uses it to
    /// terminate child processes without waiting for the database.
    active_jobs: Arc<StdMutex<HashSet<PathBuf>>>,
}

impl AppState {
    pub fn new(db: SqlitePool, cfg: Config, bus: EventBus, notify: mpsc::Sender<()>) -> Self {
        let (shutdown, _) = watch::channel(false);
        Self {
            db,
            cfg,
            bus,
            notify,
            schedule_lock: Arc::new(Mutex::new(())),
            git_lock: Arc::new(Mutex::new(())),
            shutdown,
            active_jobs: Arc::new(StdMutex::new(HashSet::new())),
        }
    }

    pub fn notify_scheduler(&self) {
        let _ = self.notify.try_send(());
    }

    pub fn begin_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    pub fn is_shutting_down(&self) -> bool {
        *self.shutdown.borrow()
    }

    pub fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    /// Waits until shutdown begins. Returns immediately when it already has.
    pub async fn wait_for_shutdown(&self) {
        let mut receiver = self.shutdown.subscribe();
        let _ = receiver.wait_for(|stopping| *stopping).await;
    }

    pub fn register_job(&self, job_dir: PathBuf) {
        self.active_jobs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(job_dir);
    }

    pub fn unregister_job(&self, job_dir: &Path) {
        self.active_jobs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(job_dir);
    }

    /// Kills every agent process group started by this process. Entries are
    /// validated against the recorded start time, so stale pid files are safe.
    pub fn kill_active_jobs(&self) {
        let job_dirs: Vec<PathBuf> = self
            .active_jobs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .cloned()
            .collect();
        for job_dir in job_dirs {
            crate::agents::kill_job_dir(&job_dir);
        }
    }
}
