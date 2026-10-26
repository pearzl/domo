//! SQLite connection setup and embedded schema.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::models::{
    string_list_json, AgentRow, AGENT_OPENCODE, ROLE_EXECUTOR, ROLE_PLANNER, ROLE_VERIFIER,
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS agents (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    model TEXT NOT NULL DEFAULT '',
    roles TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    max_concurrency INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1,
    command_json TEXT,
    model_args_json TEXT,
    cooldown_until TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    goal TEXT NOT NULL,
    repo_path TEXT NOT NULL,
    base_branch TEXT NOT NULL,
    branch TEXT NOT NULL,
    workspace TEXT NOT NULL,
    stage TEXT NOT NULL,
    status TEXT,
    blocked_reason TEXT,
    report TEXT,
    report_feedback TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    key TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    acceptance TEXT NOT NULL,
    deps TEXT NOT NULL,
    stage TEXT NOT NULL,
    status TEXT,
    agent_id TEXT,
    last_agent_id TEXT,
    worktree TEXT,
    branch TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    rejections INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    result_summary TEXT,
    verifier_issues TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(project_id, key)
);
CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project_id, seq);

CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    agent_id TEXT,
    status TEXT NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 0,
    prompt TEXT NOT NULL,
    result TEXT,
    stdout TEXT,
    stderr TEXT,
    error TEXT,
    exit_code INTEGER,
    pid INTEGER,
    started_at TEXT,
    finished_at TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_jobs_project ON jobs(project_id, created_at);
CREATE INDEX IF NOT EXISTS idx_jobs_active ON jobs(task_id, role, status);

CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_project ON messages(project_id, created_at);

CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT,
    task_id TEXT,
    kind TEXT NOT NULL,
    message TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_project ON events(project_id, id);
"#;

pub async fn connect(path: &Path) -> anyhow::Result<SqlitePool> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(10))
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    sqlx::raw_sql(SCHEMA).execute(&pool).await?;
    seed_default_agent(&pool).await?;
    Ok(pool)
}

async fn seed_default_agent(pool: &SqlitePool) -> anyhow::Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agents")
        .fetch_one(pool)
        .await?;
    if count > 0 {
        return Ok(());
    }
    let now = crate::domain::now();
    let id = format!("agt_{}", Uuid::new_v4().simple());
    let roles = string_list_json(&[
        ROLE_PLANNER.to_string(),
        ROLE_EXECUTOR.to_string(),
        ROLE_VERIFIER.to_string(),
    ]);
    let model = std::env::var("DOMO_DEFAULT_MODEL").unwrap_or_default();
    sqlx::query(
        "INSERT INTO agents (id, name, kind, model, roles, priority, max_concurrency, enabled, \
         command_json, model_args_json, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 0, 2, 1, NULL, NULL, ?, ?)",
    )
    .bind(&id)
    .bind("OpenCode 默认团队")
    .bind(AGENT_OPENCODE)
    .bind(&model)
    .bind(&roles)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn load_agent(pool: &SqlitePool, id: &str) -> anyhow::Result<Option<AgentRow>> {
    Ok(
        sqlx::query_as::<_, AgentRow>("SELECT * FROM agents WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}
