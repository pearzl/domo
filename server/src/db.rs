//! SQLite connection setup and embedded schema.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::models::{
    parse_string_list, string_list_json, AgentRow, ProjectAgentEntry, ProjectAgentPool,
    ProjectAgentRow, ProjectRolePool, AGENT_OPENCODE, ROLE_EXECUTOR, ROLE_PLANNER, ROLE_VERIFIER,
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
    failure_kind TEXT,
    transient_failures INTEGER NOT NULL DEFAULT 0,
    availability_json TEXT,
    usage_limit_json TEXT,
    usage_reset_at TEXT,
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
    replan_reason TEXT,
    replan_source TEXT,
    replan_requested_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS project_agents (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    agent_name TEXT NOT NULL DEFAULT '',
    priority INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (project_id, role, agent_id)
);
CREATE INDEX IF NOT EXISTS idx_project_agents_role
    ON project_agents(project_id, role, priority);

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    queue_order INTEGER NOT NULL DEFAULT 0,
    key TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    acceptance TEXT NOT NULL,
    deps TEXT NOT NULL,
    parent_id TEXT,
    milestone INTEGER NOT NULL DEFAULT 0,
    human_gate TEXT,
    forced_agent_id TEXT,
    forced_agent_role TEXT,
    executor_agent_id TEXT,
    verifier_agent_id TEXT,
    stage TEXT NOT NULL,
    status TEXT,
    agent_id TEXT,
    last_agent_id TEXT,
    worktree TEXT,
    branch TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    rejections INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    last_failure_kind TEXT,
    waiting_reason TEXT,
    result_summary TEXT,
    verifier_issues TEXT NOT NULL DEFAULT '[]',
    guidance TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project_id, seq);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tasks_active_key ON tasks(project_id, key)
    WHERE COALESCE(status, '') != 'canceled';

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
    failure_kind TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cost REAL,
    duration_ms INTEGER,
    usage_known INTEGER NOT NULL DEFAULT 0,
    started_at TEXT,
    finished_at TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_jobs_project ON jobs(project_id, created_at);
CREATE INDEX IF NOT EXISTS idx_jobs_active ON jobs(task_id, role, status);
CREATE INDEX IF NOT EXISTS idx_jobs_agent ON jobs(agent_id, created_at);

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
    migrate(&pool).await?;
    seed_default_agent(&pool).await?;
    Ok(pool)
}

async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    ensure_column(pool, "projects", "replan_reason", "TEXT").await?;
    ensure_column(pool, "projects", "replan_source", "TEXT").await?;
    ensure_column(pool, "projects", "replan_requested_at", "TEXT").await?;
    ensure_column(pool, "tasks", "guidance", "TEXT NOT NULL DEFAULT ''").await?;
    ensure_column(pool, "tasks", "parent_id", "TEXT").await?;
    ensure_column(pool, "tasks", "milestone", "INTEGER NOT NULL DEFAULT 0").await?;
    ensure_column(pool, "tasks", "human_gate", "TEXT").await?;
    ensure_column(pool, "tasks", "forced_agent_id", "TEXT").await?;
    ensure_column(pool, "tasks", "forced_agent_role", "TEXT").await?;
    ensure_column(pool, "tasks", "executor_agent_id", "TEXT").await?;
    ensure_column(pool, "tasks", "verifier_agent_id", "TEXT").await?;
    ensure_column(pool, "tasks", "queue_order", "INTEGER NOT NULL DEFAULT 0").await?;
    ensure_column(pool, "tasks", "last_failure_kind", "TEXT").await?;
    ensure_column(pool, "tasks", "waiting_reason", "TEXT").await?;
    ensure_column(pool, "agents", "failure_kind", "TEXT").await?;
    ensure_column(
        pool,
        "agents",
        "transient_failures",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    ensure_column(pool, "jobs", "failure_kind", "TEXT").await?;
    ensure_column(pool, "jobs", "input_tokens", "INTEGER").await?;
    ensure_column(pool, "jobs", "output_tokens", "INTEGER").await?;
    ensure_column(pool, "jobs", "cost", "REAL").await?;
    ensure_column(pool, "jobs", "duration_ms", "INTEGER").await?;
    ensure_column(pool, "jobs", "usage_known", "INTEGER NOT NULL DEFAULT 0").await?;
    ensure_column(pool, "agents", "availability_json", "TEXT").await?;
    ensure_column(pool, "agents", "usage_limit_json", "TEXT").await?;
    ensure_column(pool, "agents", "usage_reset_at", "TEXT").await?;
    sqlx::raw_sql("CREATE INDEX IF NOT EXISTS idx_jobs_agent ON jobs(agent_id, created_at)")
        .execute(pool)
        .await?;
    sqlx::raw_sql("UPDATE tasks SET queue_order = seq WHERE queue_order = 0 AND seq > 0")
        .execute(pool)
        .await?;
    sqlx::raw_sql(
        "CREATE INDEX IF NOT EXISTS idx_tasks_queue ON tasks(project_id, queue_order, seq)",
    )
    .execute(pool)
    .await?;
    ensure_column(
        pool,
        "project_agents",
        "agent_name",
        "TEXT NOT NULL DEFAULT ''",
    )
    .await?;
    sqlx::raw_sql("CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_id)")
        .execute(pool)
        .await?;

    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(pool)
        .await?;
    if version >= 1 {
        return Ok(());
    }

    let mut conn = pool.acquire().await?;
    sqlx::raw_sql("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await?;
    let migrated = sqlx::raw_sql(
        "BEGIN IMMEDIATE;
         CREATE TABLE tasks_migrated (
             id TEXT PRIMARY KEY,
             project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
             seq INTEGER NOT NULL,
             queue_order INTEGER NOT NULL DEFAULT 0,
             key TEXT NOT NULL,
             title TEXT NOT NULL,
             description TEXT NOT NULL,
             acceptance TEXT NOT NULL,
             deps TEXT NOT NULL,
             parent_id TEXT,
             milestone INTEGER NOT NULL DEFAULT 0,
             human_gate TEXT,
             forced_agent_id TEXT,
             forced_agent_role TEXT,
             executor_agent_id TEXT,
             verifier_agent_id TEXT,
             stage TEXT NOT NULL,
             status TEXT,
             agent_id TEXT,
             last_agent_id TEXT,
             worktree TEXT,
             branch TEXT,
             attempts INTEGER NOT NULL DEFAULT 0,
             rejections INTEGER NOT NULL DEFAULT 0,
             last_error TEXT,
             last_failure_kind TEXT,
             waiting_reason TEXT,
             result_summary TEXT,
             verifier_issues TEXT NOT NULL DEFAULT '[]',
             guidance TEXT NOT NULL DEFAULT '',
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL
         );
         INSERT INTO tasks_migrated (
             id, project_id, seq, queue_order, key, title, description, acceptance, deps, parent_id,
             milestone, human_gate, forced_agent_id, forced_agent_role, executor_agent_id,
             verifier_agent_id, stage, status,
             agent_id, last_agent_id, worktree, branch, attempts, rejections, last_error,
             last_failure_kind, waiting_reason,
             result_summary, verifier_issues, guidance, created_at, updated_at
         )
         SELECT
             id, project_id, seq, queue_order, key, title, description, acceptance, deps, parent_id,
             milestone, human_gate, forced_agent_id, forced_agent_role, executor_agent_id,
             verifier_agent_id, stage, status,
             agent_id, last_agent_id, worktree, branch, attempts, rejections, last_error,
             last_failure_kind, waiting_reason,
             result_summary, verifier_issues, guidance, created_at, updated_at
         FROM tasks;
         DROP TABLE tasks;
         ALTER TABLE tasks_migrated RENAME TO tasks;
         UPDATE tasks SET queue_order = seq WHERE queue_order = 0 AND seq > 0;
         CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project_id, seq);
         CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_id);
         CREATE INDEX IF NOT EXISTS idx_tasks_queue ON tasks(project_id, queue_order, seq);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_tasks_active_key ON tasks(project_id, key)
             WHERE COALESCE(status, '') != 'canceled';
         PRAGMA user_version = 1;
         COMMIT;",
    )
    .execute(&mut *conn)
    .await;
    let enabled = sqlx::raw_sql("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await;
    migrated?;
    enabled?;
    Ok(())
}

async fn ensure_column(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    definition: &str,
) -> anyhow::Result<()> {
    let exists: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?"
    ))
    .bind(column)
    .fetch_one(pool)
    .await?;
    if exists == 0 {
        sqlx::raw_sql(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))
        .execute(pool)
        .await?;
    }
    Ok(())
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

/// A project inherits a snapshot of the global agent configuration when it is
/// first touched. Priorities are normalized per role so project changes only
/// reorder the project's own pool.
pub async fn ensure_project_agents(pool: &SqlitePool, project_id: &str) -> anyhow::Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_agents WHERE project_id = ?")
        .bind(project_id)
        .fetch_one(pool)
        .await?;
    if count > 0 {
        return Ok(());
    }
    let agents = sqlx::query_as::<_, AgentRow>(
        "SELECT * FROM agents ORDER BY priority ASC, created_at ASC, id ASC",
    )
    .fetch_all(pool)
    .await?;
    let timestamp = crate::domain::now();
    let mut tx = pool.begin().await?;
    let recheck: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM project_agents WHERE project_id = ?")
            .bind(project_id)
            .fetch_one(&mut *tx)
            .await?;
    if recheck > 0 {
        tx.commit().await?;
        return Ok(());
    }
    for role in [ROLE_PLANNER, ROLE_EXECUTOR, ROLE_VERIFIER] {
        let mut priority = 0;
        for agent in &agents {
            if !parse_string_list(&agent.roles)
                .iter()
                .any(|item| item == role)
            {
                continue;
            }
            sqlx::query(
                "INSERT OR IGNORE INTO project_agents \
                 (project_id, role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(project_id)
            .bind(role)
            .bind(&agent.id)
            .bind(&agent.name)
            .bind(priority)
            .bind(agent.enabled)
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(&mut *tx)
            .await?;
            priority += 1;
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn load_project_agent_pool(
    pool: &SqlitePool,
    project_id: &str,
) -> anyhow::Result<ProjectAgentPool> {
    ensure_project_agents(pool, project_id).await?;
    let rows = sqlx::query_as::<_, ProjectAgentRow>(
        "SELECT * FROM project_agents WHERE project_id = ? ORDER BY role ASC, priority ASC, agent_id ASC",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    let agents = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents")
        .fetch_all(pool)
        .await?;
    let mut roles = Vec::new();
    for role in [ROLE_PLANNER, ROLE_EXECUTOR, ROLE_VERIFIER] {
        let mut entries = Vec::new();
        for row in rows.iter().filter(|row| row.role == role) {
            let agent = agents.iter().find(|agent| agent.id == row.agent_id);
            let unavailable_reason = match agent {
                None => Some("全局 Agent 已被删除".to_string()),
                Some(agent)
                    if !parse_string_list(&agent.roles)
                        .iter()
                        .any(|item| item == role) =>
                {
                    Some("全局 Agent 已不再具备该角色".to_string())
                }
                Some(_) => None,
            };
            let availability = agent.and_then(|agent| {
                crate::usage::parse_availability(agent.availability_json.as_deref())
            });
            let availability_reason =
                crate::usage::availability_status(availability.as_ref(), chrono::Local::now())
                    .reason;
            let usage_limit = agent.and_then(|agent| {
                crate::usage::parse_usage_limit(agent.usage_limit_json.as_deref())
            });
            let usage_window = match agent {
                Some(agent) => {
                    crate::usage::agent_usage(
                        pool,
                        &agent.id,
                        agent.usage_limit_json.as_deref(),
                        agent.usage_reset_at.as_deref(),
                        chrono::Utc::now(),
                    )
                    .await?
                }
                None => None,
            };
            entries.push(ProjectAgentEntry {
                agent_id: row.agent_id.clone(),
                name: agent
                    .map(|agent| agent.name.clone())
                    .unwrap_or_else(|| row.agent_name.clone()),
                kind: agent
                    .map(|agent| agent.kind.clone())
                    .unwrap_or_else(|| "missing".into()),
                model: agent.map(|agent| agent.model.clone()).unwrap_or_default(),
                priority: row.priority,
                enabled: row.enabled != 0,
                agent_enabled: agent.map(|agent| agent.enabled != 0).unwrap_or(false),
                available: unavailable_reason.is_none(),
                unavailable_reason,
                max_concurrency: agent.map(|agent| agent.max_concurrency.max(1)).unwrap_or(1),
                cooldown_until: agent.and_then(|agent| agent.cooldown_until.clone()),
                last_error: agent.and_then(|agent| agent.last_error.clone()),
                failure_kind: agent.and_then(|agent| agent.failure_kind.clone()),
                transient_failures: agent.map(|agent| agent.transient_failures).unwrap_or(0),
                availability,
                availability_reason,
                usage_limit,
                usage_window,
            });
        }
        roles.push(ProjectRolePool {
            role: role.to_string(),
            entries,
        });
    }
    Ok(ProjectAgentPool {
        project_id: project_id.to_string(),
        roles,
    })
}
