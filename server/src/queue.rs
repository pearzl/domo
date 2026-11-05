//! Durable global dispatch order. Queue entries represent dispatch intentions,
//! not running jobs; pausing removes eligibility without losing their order.
use crate::{
    domain::{new_id, now},
    models::*,
};
use anyhow::Result;
use serde_json::{json, Value};
use sqlx::SqlitePool;

pub async fn initialize(pool: &SqlitePool) -> crate::db::DbResult<()> {
    sqlx::raw_sql(r#"
CREATE TABLE IF NOT EXISTS dispatch_queue (
 id TEXT PRIMARY KEY, scope_key TEXT NOT NULL, project_id TEXT NOT NULL REFERENCES projects(id),
 task_id TEXT, role TEXT NOT NULL, agent_id TEXT, position INTEGER NOT NULL,
 generation INTEGER NOT NULL, state TEXT NOT NULL DEFAULT 'waiting', reason TEXT,
 forced INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, dispatched_job_id TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_queue_scope ON dispatch_queue(scope_key) WHERE state = 'waiting';
CREATE INDEX IF NOT EXISTS idx_queue_agent ON dispatch_queue(agent_id, state, position);
CREATE TABLE IF NOT EXISTS retry_requests (
 task_id TEXT PRIMARY KEY REFERENCES tasks(id), job_id TEXT NOT NULL REFERENCES jobs(id),
 agent_id TEXT NOT NULL, role TEXT NOT NULL, requested_at TEXT NOT NULL
);
"#).execute(pool).await?;
    Ok(())
}

pub fn key(project: &ProjectRow, task: Option<&TaskRow>, role: &str) -> String {
    task.map(|task| format!("task:{}:{role}", task.id))
        .unwrap_or_else(|| {
            format!(
                "project:{}:{}:{}:{role}",
                project.id, project.stage, project.replan_generation
            )
        })
}

pub async fn enter(
    pool: &SqlitePool,
    project: &ProjectRow,
    task: Option<&TaskRow>,
    role: &str,
) -> Result<String> {
    let scope = key(project, task, role);
    sqlx::query("INSERT OR IGNORE INTO dispatch_queue(id, scope_key, project_id, task_id, role, position, generation, created_at) \
        VALUES(?, ?, ?, ?, ?, (SELECT COALESCE(MAX(position), 0) + 1 FROM dispatch_queue), ?, ?)")
        .bind(new_id("q")).bind(&scope).bind(&project.id).bind(task.map(|t| &t.id)).bind(role)
        .bind(project.replan_generation).bind(now()).execute(pool).await?;
    Ok(scope)
}

pub async fn candidate(
    pool: &SqlitePool,
    project: &ProjectRow,
    task: Option<&TaskRow>,
    role: &str,
    agent: &AgentRow,
    forced: bool,
) -> Result<bool> {
    let scope = enter(pool, project, task, role).await?;
    sqlx::query("UPDATE dispatch_queue SET agent_id = ?, forced = ? WHERE scope_key = ? AND state = 'waiting'")
        .bind(&agent.id).bind(forced).bind(&scope).execute(pool).await?;
    let earlier: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM dispatch_queue q JOIN projects p ON p.id = q.project_id \
         LEFT JOIN tasks t ON t.id = q.task_id WHERE q.agent_id = ? AND q.state = 'waiting' \
         AND q.position < (SELECT position FROM dispatch_queue WHERE scope_key = ? AND state = 'waiting') \
         AND (p.status = 'running' OR (q.role = 'merge_conflict' AND p.stage = 'completed' AND p.status IS NULL)) \
          AND (q.task_id IS NULL OR t.status = 'queued' OR (q.role = 'replan_review' AND t.status = 'blocked' AND EXISTS (SELECT 1 FROM replan_requests r WHERE r.task_id = t.id AND r.state = 'pending')))",
    ).bind(&agent.id).bind(&scope).fetch_one(pool).await?;
    Ok(earlier == 0)
}

pub async fn waiting(
    pool: &SqlitePool,
    project: &ProjectRow,
    task: Option<&TaskRow>,
    role: &str,
    reason: &str,
) -> Result<()> {
    let scope = enter(pool, project, task, role).await?;
    sqlx::query("UPDATE dispatch_queue SET reason = ? WHERE scope_key = ? AND state = 'waiting'")
        .bind(reason)
        .bind(scope)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn dispatched(
    pool: &SqlitePool,
    project: &ProjectRow,
    task: Option<&TaskRow>,
    role: &str,
    job: &str,
) -> Result<()> {
    sqlx::query("UPDATE dispatch_queue SET state = 'dispatched', dispatched_job_id = ? WHERE scope_key = ? AND state = 'waiting'")
        .bind(job).bind(key(project, task, role)).execute(pool).await?;
    Ok(())
}

pub async fn reconcile(pool: &SqlitePool) -> Result<()> {
    sqlx::query("UPDATE dispatch_queue SET state = 'obsolete' WHERE state = 'waiting' AND EXISTS \
        (SELECT 1 FROM projects p LEFT JOIN tasks t ON t.id = dispatch_queue.task_id \
         WHERE p.id = dispatch_queue.project_id AND (p.status = 'archived' OR (p.stage = 'completed' AND dispatch_queue.role != 'merge_conflict') \
         OR (dispatch_queue.task_id IS NULL AND (p.replan_generation != dispatch_queue.generation \
            OR dispatch_queue.scope_key NOT LIKE 'project:' || p.id || ':' || p.stage || ':%')) \
         OR (dispatch_queue.task_id IS NOT NULL AND (t.stage = 'completed' OR t.status = 'canceled' \
            OR (t.stage = 'verifying' AND dispatch_queue.role = 'executor') \
             OR (t.stage = 'executing' AND dispatch_queue.role = 'verifier')
             OR (dispatch_queue.role = 'replan_review' AND NOT EXISTS (SELECT 1 FROM replan_requests r WHERE r.task_id = t.id AND r.state = 'pending'))))))")
        .execute(pool).await?;
    Ok(())
}

pub async fn view(pool: &SqlitePool) -> Result<Value> {
    reconcile(pool).await?;
    type QueueRow = (
        String,
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        i64,
        Option<String>,
        i64,
        String,
    );
    let queued: Vec<QueueRow> = sqlx::query_as("SELECT q.id, q.project_id, p.name, q.task_id, q.role, q.agent_id, q.position, q.reason, q.forced, q.created_at \
        FROM dispatch_queue q JOIN projects p ON p.id = q.project_id WHERE q.state = 'waiting' AND COALESCE(p.status, '') != 'archived' ORDER BY q.position, q.id")
        .fetch_all(pool).await?;
    let mut entries = Vec::new();
    for (id, project_id, project_name, task_id, role, agent_id, position, reason, forced, at) in
        queued
    {
        let tasks = sqlx::query_as::<_, TaskRow>(
            "SELECT * FROM tasks WHERE project_id = ? AND COALESCE(status, '') != 'canceled'",
        )
        .bind(&project_id)
        .fetch_all(pool)
        .await?;
        let mut affected = std::collections::HashSet::new();
        if let Some(task) = tasks.iter().find(|task| Some(&task.id) == task_id.as_ref()) {
            affected.insert(task.key.clone());
        }
        loop {
            let before = affected.len();
            for task in &tasks {
                if parse_string_list(&task.deps)
                    .iter()
                    .any(|dep| affected.contains(dep))
                {
                    affected.insert(task.key.clone());
                }
            }
            if before == affected.len() {
                break;
            }
        }
        entries.push(json!({"id": id, "project_id": project_id, "project_name": project_name, "task_id": task_id,
            "role": role, "agent_id": agent_id, "position": position, "reason": reason,
            "forced": forced != 0, "created_at": at, "affected_tasks": affected.len().saturating_sub(1),
            "blocks_milestone": tasks.iter().any(|t| t.milestone != 0 && affected.contains(&t.key)), "blocks_summary": true}));
    }
    type RunningRow = (
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
    );
    let running: Vec<RunningRow> = sqlx::query_as(
        "SELECT id, project_id, task_id, role, agent_id, started_at FROM jobs WHERE status = 'running' ORDER BY created_at",
    ).fetch_all(pool).await?;
    Ok(
        json!({"queued": entries, "running": running.into_iter().map(|(id, project, task, role, agent, started)| json!({
        "id": id, "project_id": project, "task_id": task, "role": role, "agent_id": agent, "started_at": started
    })).collect::<Vec<_>>()}),
    )
}

pub async fn reorder(pool: &SqlitePool, id: &str, position: usize) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let (agent,): (Option<String>,) =
        sqlx::query_as("SELECT agent_id FROM dispatch_queue WHERE id = ? AND state = 'waiting'")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    let mut ids: Vec<String> = sqlx::query_scalar("SELECT id FROM dispatch_queue WHERE agent_id IS ? AND state = 'waiting' ORDER BY position, id")
        .bind(&agent).fetch_all(&mut *tx).await?;
    let index = ids
        .iter()
        .position(|v| v == id)
        .ok_or_else(|| anyhow::anyhow!("排队项已失效"))?;
    let selected = ids.remove(index);
    ids.insert(position.min(ids.len()), selected);
    let slots: Vec<i64> = sqlx::query_scalar("SELECT position FROM dispatch_queue WHERE agent_id IS ? AND state = 'waiting' ORDER BY position, id")
        .bind(&agent).fetch_all(&mut *tx).await?;
    for (entry, slot) in ids.iter().zip(slots) {
        sqlx::query("UPDATE dispatch_queue SET position = ? WHERE id = ?")
            .bind(slot)
            .bind(entry)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("INSERT INTO events(kind, message, payload, created_at) VALUES('global_queue_reordered', '人工调整 Agent 全局队列', ?, ?)")
        .bind(json!({"version": 1, "entry": id, "position": position, "agent_id": agent}).to_string()).bind(now()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
