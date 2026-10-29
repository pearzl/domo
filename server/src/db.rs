//! SQLite connection setup and embedded schema.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::models::{
    default_team_row_to_pool, parse_string_list, string_list_json, AgentRow, DefaultTeamRow,
    ProjectAgentEntry, ProjectAgentPool, ProjectAgentRow, ProjectRolePool, TaskAgentPool,
    TaskAgentRow, TaskRow, AGENT_OPENCODE, ROLE_EXECUTOR, ROLE_PLANNER, ROLE_VERIFIER,
};

/// 项目池覆盖全部三种角色；任务池只派生执行者与验收者，规划者始终用项目池。
pub const PROJECT_POOL_ROLES: &[&str] = &[ROLE_PLANNER, ROLE_EXECUTOR, ROLE_VERIFIER];
pub const TASK_POOL_ROLES: &[&str] = &[ROLE_EXECUTOR, ROLE_VERIFIER];

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
    confirm_mode TEXT NOT NULL DEFAULT 'manual',
    proposal TEXT,
    confirmed_at TEXT,
    completion_mode TEXT NOT NULL DEFAULT 'tasks',
    merged_at TEXT,
    merged_commit TEXT,
    avoid_agent_id TEXT,
    avoid_agent_role TEXT,
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

CREATE TABLE IF NOT EXISTS task_agents (
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    agent_name TEXT NOT NULL DEFAULT '',
    priority INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (task_id, role, agent_id)
);
CREATE INDEX IF NOT EXISTS idx_task_agents_role
    ON task_agents(task_id, role, priority);

CREATE TABLE IF NOT EXISTS task_agent_roles (
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    initialized_at TEXT NOT NULL,
    PRIMARY KEY (task_id, role)
);

CREATE TABLE IF NOT EXISTS default_team (
    role TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    agent_name TEXT NOT NULL DEFAULT '',
    priority INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (role, agent_id)
);

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
    avoid_agent_id TEXT,
    avoid_agent_role TEXT,
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
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    reasoning_tokens INTEGER,
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
    job_id TEXT,
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
    seed_defaults(&pool).await?;
    Ok(pool)
}

async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    ensure_column(pool, "projects", "replan_reason", "TEXT").await?;
    ensure_column(pool, "projects", "replan_source", "TEXT").await?;
    ensure_column(pool, "projects", "replan_requested_at", "TEXT").await?;
    rename_column(pool, "projects", "goal_confirm_mode", "confirm_mode").await?;
    rename_column(pool, "projects", "goal_proposal", "proposal").await?;
    rename_column(pool, "projects", "goal_confirmed_at", "confirmed_at").await?;
    ensure_column(
        pool,
        "projects",
        "confirm_mode",
        "TEXT NOT NULL DEFAULT 'manual'",
    )
    .await?;
    ensure_column(pool, "projects", "proposal", "TEXT").await?;
    sqlx::raw_sql("UPDATE projects SET proposal = NULL WHERE proposal LIKE '%\"kind\":\"ready\"%'")
        .execute(pool)
        .await?;
    ensure_column(pool, "projects", "confirmed_at", "TEXT").await?;
    ensure_column(
        pool,
        "projects",
        "completion_mode",
        "TEXT NOT NULL DEFAULT 'tasks'",
    )
    .await?;
    ensure_column(pool, "projects", "merged_at", "TEXT").await?;
    ensure_column(pool, "projects", "merged_commit", "TEXT").await?;
    ensure_column(pool, "projects", "avoid_agent_id", "TEXT").await?;
    ensure_column(pool, "projects", "avoid_agent_role", "TEXT").await?;
    ensure_column(pool, "events", "job_id", "TEXT").await?;
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
    ensure_column(pool, "tasks", "avoid_agent_id", "TEXT").await?;
    ensure_column(pool, "tasks", "avoid_agent_role", "TEXT").await?;
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
    ensure_column(pool, "jobs", "cache_read_tokens", "INTEGER").await?;
    ensure_column(pool, "jobs", "cache_write_tokens", "INTEGER").await?;
    ensure_column(pool, "jobs", "reasoning_tokens", "INTEGER").await?;
    ensure_column(pool, "jobs", "cost", "REAL").await?;
    ensure_column(pool, "jobs", "duration_ms", "INTEGER").await?;
    ensure_column(pool, "jobs", "usage_known", "INTEGER NOT NULL DEFAULT 0").await?;
    ensure_column(pool, "agents", "availability_json", "TEXT").await?;
    ensure_column(pool, "agents", "usage_limit_json", "TEXT").await?;
    ensure_column(pool, "agents", "usage_reset_at", "TEXT").await?;
    sqlx::raw_sql("CREATE INDEX IF NOT EXISTS idx_jobs_agent ON jobs(agent_id, created_at)")
        .execute(pool)
        .await?;
    sqlx::raw_sql(
        "UPDATE projects SET stage = 'analysis' WHERE stage IN ('goal_confirm', 'planning');
         UPDATE jobs SET role = 'analysis' WHERE role IN ('confirm', 'goal');
         UPDATE tasks SET queue_order = seq WHERE queue_order = 0 AND seq > 0;",
    )
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
             avoid_agent_id TEXT,
             avoid_agent_role TEXT,
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
             agent_id, last_agent_id, avoid_agent_id, avoid_agent_role, worktree, branch,
             attempts, rejections, last_error,
             last_failure_kind, waiting_reason,
             result_summary, verifier_issues, guidance, created_at, updated_at
         )
         SELECT
             id, project_id, seq, queue_order, key, title, description, acceptance, deps, parent_id,
             milestone, human_gate, forced_agent_id, forced_agent_role, executor_agent_id,
             verifier_agent_id, stage, status,
             agent_id, last_agent_id, avoid_agent_id, avoid_agent_role, worktree, branch,
             attempts, rejections, last_error,
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

async fn rename_column(pool: &SqlitePool, table: &str, old: &str, new: &str) -> anyhow::Result<()> {
    let old_exists: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?"
    ))
    .bind(old)
    .fetch_one(pool)
    .await?;
    let new_exists: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?"
    ))
    .bind(new)
    .fetch_one(pool)
    .await?;
    if old_exists > 0 && new_exists == 0 {
        sqlx::raw_sql(&format!("ALTER TABLE {table} RENAME COLUMN {old} TO {new}"))
            .execute(pool)
            .await?;
    }
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

async fn seed_defaults(pool: &SqlitePool) -> anyhow::Result<()> {
    let agent_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agents")
        .fetch_one(pool)
        .await?;
    let mut fresh_agent_id: Option<String> = None;
    if agent_count == 0 {
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
        fresh_agent_id = Some(id);
    }
    seed_default_team(pool, fresh_agent_id).await
}

/// 默认团队为空时初始化：空库沿用 OpenCode 默认团队 Agent；存量库把全局
/// Agent 配置按既有优先级顺序快照进来，保持继承顺序不变。
async fn seed_default_team(
    pool: &SqlitePool,
    fresh_agent_id: Option<String>,
) -> anyhow::Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM default_team")
        .fetch_one(pool)
        .await?;
    if count > 0 {
        return Ok(());
    }
    let timestamp = crate::domain::now();
    if let Some(agent_id) = fresh_agent_id {
        let mut tx = pool.begin().await?;
        for role in [ROLE_PLANNER, ROLE_EXECUTOR, ROLE_VERIFIER] {
            sqlx::query(
                "INSERT OR IGNORE INTO default_team \
                 (role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
                 VALUES (?, ?, ?, 0, 1, ?, ?)",
            )
            .bind(role)
            .bind(&agent_id)
            .bind("OpenCode 默认团队")
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        return Ok(());
    }
    let agents = sqlx::query_as::<_, AgentRow>(
        "SELECT * FROM agents ORDER BY priority ASC, created_at ASC, id ASC",
    )
    .fetch_all(pool)
    .await?;
    if agents.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
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
                "INSERT OR IGNORE INTO default_team \
                 (role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
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

pub async fn load_agent(pool: &SqlitePool, id: &str) -> anyhow::Result<Option<AgentRow>> {
    Ok(
        sqlx::query_as::<_, AgentRow>("SELECT * FROM agents WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

/// A project inherits a snapshot of the default team configuration when it is
/// first touched. Priorities are normalized per role so project changes only
/// reorder the project's own pool. Entries whose global agent has been deleted
/// are skipped, and a disabled global agent starts disabled in the pool.
pub async fn ensure_project_agents(pool: &SqlitePool, project_id: &str) -> anyhow::Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_agents WHERE project_id = ?")
        .bind(project_id)
        .fetch_one(pool)
        .await?;
    if count > 0 {
        return Ok(());
    }
    let rows = default_team_pool_rows(pool).await?;
    let agents = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents")
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
        for row in rows.iter().filter(|row| row.role == role) {
            let Some(agent) = agents.iter().find(|agent| agent.id == row.agent_id) else {
                continue;
            };
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
            .bind(i64::from(row.enabled != 0 && agent.enabled != 0))
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
    Ok(ProjectAgentPool {
        project_id: project_id.to_string(),
        roles: build_role_pools(pool, &rows, PROJECT_POOL_ROLES).await?,
    })
}

/// 默认团队视图：返回 ProjectAgentPool 形态（project_id 固定为 "default"），
/// 条目带全局 Agent 的可用性与启停信息，供前端面板直接复用。
pub async fn load_default_team_pool(pool: &SqlitePool) -> anyhow::Result<ProjectAgentPool> {
    let rows = default_team_pool_rows(pool).await?;
    Ok(ProjectAgentPool {
        project_id: "default".to_string(),
        roles: build_role_pools(pool, &rows, PROJECT_POOL_ROLES).await?,
    })
}

/// 默认团队按角色展开为项目池形态的行，角色顺序固定，条目按 priority 与
/// agent_id 排序保证稳定。
/// 默认团队按角色展开为项目池形态的行，角色顺序固定，条目按 priority 与
/// agent_id 排序保证稳定。启停与全局 Agent 状态合并，和继承语义保持一致。
async fn default_team_pool_rows(pool: &SqlitePool) -> anyhow::Result<Vec<ProjectAgentRow>> {
    let members = sqlx::query_as::<_, DefaultTeamRow>("SELECT * FROM default_team")
        .fetch_all(pool)
        .await?;
    let agents = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents")
        .fetch_all(pool)
        .await?;
    let mut rows = Vec::new();
    for role in [ROLE_PLANNER, ROLE_EXECUTOR, ROLE_VERIFIER] {
        let mut for_role: Vec<&DefaultTeamRow> = members
            .iter()
            .filter(|member| member.role == role)
            .collect();
        for_role.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then(a.agent_id.cmp(&b.agent_id))
        });
        for member in for_role {
            let global_enabled = agents
                .iter()
                .find(|agent| agent.id == member.agent_id)
                .map(|agent| agent.enabled != 0)
                .unwrap_or(true);
            let mut row = default_team_row_to_pool(member.clone());
            row.role = role.to_string();
            row.enabled = i64::from(member.enabled != 0 && global_enabled);
            rows.push(row);
        }
    }
    Ok(rows)
}

/// 把池行与全局 Agent 合并成带可用性信息的角色分组视图。`roles` 决定返回哪些
/// 角色分组，项目池三种角色，任务池只有执行者与验收者。
async fn build_role_pools(
    pool: &SqlitePool,
    rows: &[ProjectAgentRow],
    roles: &[&str],
) -> anyhow::Result<Vec<ProjectRolePool>> {
    let agents = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents")
        .fetch_all(pool)
        .await?;
    let mut role_pools = Vec::new();
    for role in roles {
        let mut entries = Vec::new();
        for row in rows.iter().filter(|row| row.role == *role) {
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
        role_pools.push(ProjectRolePool {
            role: role.to_string(),
            entries,
        });
    }
    Ok(role_pools)
}

/// 任务 Agent 池：首次访问时从项目池对应角色快照初始化；任务上遗留的
/// executor_agent_id/verifier_agent_id 会迁移为只含该 Agent 的单 Agent 池。
/// 项目池后来的变化不会改写已初始化的任务池。每个角色的初始化状态单独记录，
/// 因此某角色被清空后不会因为其他角色的操作重新从项目池填充。
pub async fn ensure_task_agents(
    pool: &SqlitePool,
    project_id: &str,
    task_id: &str,
) -> anyhow::Result<()> {
    let initialized: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM task_agent_roles WHERE task_id = ?")
            .bind(task_id)
            .fetch_one(pool)
            .await?;
    if initialized as usize >= TASK_POOL_ROLES.len() {
        return Ok(());
    }
    ensure_project_agents(pool, project_id).await?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    ensure_task_agents_tx(&mut tx, task_id).await?;
    tx.commit().await?;
    Ok(())
}

/// 在已有事务内初始化任务池：调用方创建任务时在同一事务内完成快照。
/// `task_agent_roles` 记录每个角色是否已经快照，空角色同样算已初始化。
/// 调用方应保证项目 Agent 池已存在。
pub async fn ensure_task_agents_tx(
    executor: &mut sqlx::SqliteConnection,
    task_id: &str,
) -> anyhow::Result<()> {
    let task = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
        .bind(task_id)
        .fetch_optional(&mut *executor)
        .await?;
    let Some(task) = task else {
        return Ok(());
    };
    let initialized: Vec<(String,)> =
        sqlx::query_as("SELECT role FROM task_agent_roles WHERE task_id = ?")
            .bind(task_id)
            .fetch_all(&mut *executor)
            .await?;
    // 兼容已有任务池数据：角色下已有条目但缺少初始化记录时视为已初始化。
    let populated: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT role FROM task_agents WHERE task_id = ?")
            .bind(task_id)
            .fetch_all(&mut *executor)
            .await?;
    let timestamp = crate::domain::now();
    let agents = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents")
        .fetch_all(&mut *executor)
        .await?;
    let project_rows = sqlx::query_as::<_, ProjectAgentRow>(
        "SELECT * FROM project_agents WHERE project_id = ? AND role IN (?, ?) \
         ORDER BY role ASC, priority ASC, agent_id ASC",
    )
    .bind(&task.project_id)
    .bind(ROLE_EXECUTOR)
    .bind(ROLE_VERIFIER)
    .fetch_all(&mut *executor)
    .await?;
    for role in TASK_POOL_ROLES {
        let already_initialized = initialized.iter().any(|(value,)| value == role);
        let has_entries = populated.iter().any(|(value,)| value == role);
        if already_initialized || has_entries {
            sqlx::query(
                "INSERT OR IGNORE INTO task_agent_roles (task_id, role, initialized_at) \
                 VALUES (?, ?, ?)",
            )
            .bind(task_id)
            .bind(role)
            .bind(&timestamp)
            .execute(&mut *executor)
            .await?;
            continue;
        }
        // 旧覆盖字段迁移：Agent 存在且具备该角色时迁移为单 Agent 池；
        // 否则按项目池对应角色快照初始化。
        let legacy = match *role {
            ROLE_EXECUTOR => task.executor_agent_id.as_deref(),
            ROLE_VERIFIER => task.verifier_agent_id.as_deref(),
            _ => None,
        };
        let migrated = legacy.and_then(|agent_id| {
            agents.iter().find(|agent| {
                agent.id == agent_id
                    && parse_string_list(&agent.roles)
                        .iter()
                        .any(|item| item == role)
            })
        });
        if let Some(agent) = migrated {
            sqlx::query(
                "INSERT OR IGNORE INTO task_agents \
                 (task_id, role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, 0, ?, ?, ?)",
            )
            .bind(task_id)
            .bind(role)
            .bind(&agent.id)
            .bind(&agent.name)
            .bind(agent.enabled)
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(&mut *executor)
            .await?;
        } else {
            for row in project_rows.iter().filter(|row| row.role == *role) {
                sqlx::query(
                    "INSERT OR IGNORE INTO task_agents \
                     (task_id, role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(task_id)
                .bind(role)
                .bind(&row.agent_id)
                .bind(&row.agent_name)
                .bind(row.priority)
                .bind(row.enabled)
                .bind(&timestamp)
                .bind(&timestamp)
                .execute(&mut *executor)
                .await?;
            }
        }
        sqlx::query(
            "INSERT OR IGNORE INTO task_agent_roles (task_id, role, initialized_at) \
             VALUES (?, ?, ?)",
        )
        .bind(task_id)
        .bind(role)
        .bind(&timestamp)
        .execute(&mut *executor)
        .await?;
    }
    Ok(())
}

pub async fn load_task_agent_pool(
    pool: &SqlitePool,
    project_id: &str,
    task_id: &str,
) -> anyhow::Result<TaskAgentPool> {
    ensure_task_agents(pool, project_id, task_id).await?;
    let rows = sqlx::query_as::<_, TaskAgentRow>(
        "SELECT * FROM task_agents WHERE task_id = ? ORDER BY role ASC, priority ASC, agent_id ASC",
    )
    .bind(task_id)
    .fetch_all(pool)
    .await?;
    let rows = rows
        .into_iter()
        .map(|row| row.into_project_row())
        .collect::<Vec<_>>();
    Ok(TaskAgentPool {
        project_id: project_id.to_string(),
        task_id: task_id.to_string(),
        roles: build_role_pools(pool, &rows, TASK_POOL_ROLES).await?,
    })
}

/// 把任务池某角色覆盖为只含该 Agent 的单 Agent 池：旧覆盖接口映射到这里，
/// 对应“池中只有一个 Agent 时等价于指定该 Agent”。
pub async fn set_single_task_agent(
    pool: &SqlitePool,
    task_id: &str,
    role: &str,
    agent_id: &str,
) -> anyhow::Result<()> {
    let agent = load_agent(pool, agent_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Agent {agent_id} 不存在"))?;
    let timestamp = crate::domain::now();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    ensure_task_agents_tx(&mut tx, task_id).await?;
    sqlx::query("DELETE FROM task_agents WHERE task_id = ? AND role = ?")
        .bind(task_id)
        .bind(role)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO task_agents \
         (task_id, role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 0, 1, ?, ?)",
    )
    .bind(task_id)
    .bind(role)
    .bind(&agent.id)
    .bind(&agent.name)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT OR IGNORE INTO task_agent_roles (task_id, role, initialized_at) \
         VALUES (?, ?, ?)",
    )
    .bind(task_id)
    .bind(role)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// 把任务池某角色重置为项目池对应角色的当前快照（视角同初始化：整段覆盖）。
/// 兼容入口把“清除覆盖”映射到这里；优先级与启停按项目池当时状态复制。
pub async fn reset_task_agent_role(
    pool: &SqlitePool,
    project_id: &str,
    task_id: &str,
    role: &str,
) -> anyhow::Result<()> {
    ensure_project_agents(pool, project_id).await?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    ensure_task_agents_tx(&mut tx, task_id).await?;
    let project_rows = sqlx::query_as::<_, ProjectAgentRow>(
        "SELECT * FROM project_agents WHERE project_id = ? AND role = ? \
         ORDER BY priority ASC, agent_id ASC",
    )
    .bind(project_id)
    .bind(role)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM task_agents WHERE task_id = ? AND role = ?")
        .bind(task_id)
        .bind(role)
        .execute(&mut *tx)
        .await?;
    let timestamp = crate::domain::now();
    for row in &project_rows {
        sqlx::query(
            "INSERT OR IGNORE INTO task_agents \
             (task_id, role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(task_id)
        .bind(role)
        .bind(&row.agent_id)
        .bind(&row.agent_name)
        .bind(row.priority)
        .bind(row.enabled)
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
