//! Audit events are persisted and broadcast to connected browsers.

use tokio::sync::broadcast;

use crate::domain::now;
use crate::models::EventRow;

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<EventRow>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(512);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventRow> {
        self.tx.subscribe()
    }

    pub fn publish(&self, event: &EventRow) {
        let _ = self.tx.send(event.clone());
    }
}

pub async fn emit(
    pool: &sqlx::SqlitePool,
    bus: &EventBus,
    project_id: Option<&str>,
    task_id: Option<&str>,
    kind: &str,
    message: impl Into<String>,
) {
    let created_at = now();
    let message = message.into();
    let result = sqlx::query(
        "INSERT INTO events (project_id, task_id, kind, message, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(task_id)
    .bind(kind)
    .bind(&message)
    .bind(&created_at)
    .execute(pool)
    .await;
    if let Ok(result) = result {
        let event = EventRow {
            id: result.last_insert_rowid(),
            project_id: project_id.map(|v| v.to_string()),
            task_id: task_id.map(|v| v.to_string()),
            kind: kind.to_string(),
            message,
            created_at,
        };
        bus.publish(&event);
    }
}
