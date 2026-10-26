//! Shared runtime state.

use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::{mpsc, Mutex};

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
}

impl AppState {
    pub fn notify_scheduler(&self) {
        let _ = self.notify.try_send(());
    }
}
