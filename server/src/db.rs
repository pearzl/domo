//! SQLite connection setup and embedded schema.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::models::{
    default_team_row_to_pool, parse_string_list, string_list_json, AgentRow, DefaultTeamRow,
    ProjectAgentEntry, ProjectAgentPool, ProjectAgentRow, ProjectRolePool, TaskAgentPool,
    TaskAgentRow, AGENT_OPENCODE, ROLE_EXECUTOR, ROLE_PLANNER, ROLE_VERIFIER, STATUS_ARCHIVED,
    STATUS_CANCELED, TASK_STAGE_COMPLETED,
};
use crate::usage::UsageError;

/// Failures raised while opening the database and seeding its tables.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Usage(#[from] UsageError),
    #[error("数据库结构不受支持；请使用空数据目录，不支持历史数据库迁移")]
    UnsupportedSchema,
}

pub type DbResult<T> = std::result::Result<T, DbError>;

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
    allow_priority_fallback INTEGER NOT NULL DEFAULT 0,
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
    contract_id INTEGER,
    blocked_kind TEXT,
    name TEXT NOT NULL,
    goal TEXT NOT NULL,
    repo_path TEXT NOT NULL,
    base_branch TEXT NOT NULL,
    branch TEXT NOT NULL,
    workspace TEXT NOT NULL,
    workspace_type TEXT NOT NULL DEFAULT 'git',
    stage TEXT NOT NULL,
    status TEXT,
    blocked_reason TEXT,
    report TEXT,
    report_feedback TEXT,
    replan_reason TEXT,
    replan_source TEXT,
    replan_requested_at TEXT,
    replan_active INTEGER NOT NULL DEFAULT 0,
    replan_proposal TEXT,
    replan_generation INTEGER NOT NULL DEFAULT 0,
    replan_previous_stage TEXT,
    replan_previous_goal TEXT,
    replan_cleanup_needed INTEGER NOT NULL DEFAULT 0,
    acceptance TEXT,
    acceptance_passed INTEGER NOT NULL DEFAULT 0,
    acceptance_round INTEGER NOT NULL DEFAULT 0,
    acceptance_at TEXT,
    confirm_mode TEXT NOT NULL DEFAULT 'manual',
    proposal TEXT,
    confirmed_at TEXT,
    goal_locked_at TEXT,
    accepted_commit TEXT,
    accept_reason TEXT,
    merged_at TEXT,
    merged_commit TEXT,
    merge_rebased INTEGER NOT NULL DEFAULT 0,
    merge_agent_resolved INTEGER NOT NULL DEFAULT 0,
    merge_conflicts TEXT,
    avoid_agent_id TEXT,
    avoid_agent_role TEXT,
    retry_agent_id TEXT,
    retry_role TEXT,
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
    contract_id INTEGER,
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
    stage TEXT NOT NULL,
    status TEXT,
    agent_id TEXT,
    last_agent_id TEXT,
    avoid_agent_id TEXT,
    avoid_agent_role TEXT,
    retry_agent_id TEXT,
    retry_role TEXT,
    worktree TEXT,
    branch TEXT,
    artifacts_released_at TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    rejections INTEGER NOT NULL DEFAULT 0,
    merge_conflicts INTEGER NOT NULL DEFAULT 0,
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
CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_id);
CREATE INDEX IF NOT EXISTS idx_tasks_queue ON tasks(project_id, queue_order, seq);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tasks_key ON tasks(project_id, key);

CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY,
    contract_id INTEGER,
    execution_job_id TEXT REFERENCES jobs(id),
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
    resume INTEGER NOT NULL DEFAULT 0,
    replan_generation INTEGER NOT NULL DEFAULT 0,
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
    payload TEXT NOT NULL DEFAULT '{}',
    message TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_project ON events(project_id, id);

CREATE TABLE IF NOT EXISTS replan_cleanup (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    requested_at TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL,
    completed_at TEXT,
    PRIMARY KEY (project_id, generation)
);
CREATE INDEX IF NOT EXISTS idx_replan_cleanup_status ON replan_cleanup(status, created_at);

CREATE TRIGGER IF NOT EXISTS project_terminal_state BEFORE UPDATE OF stage, status ON projects
WHEN OLD.status = 'archived' AND (NEW.stage IS NOT OLD.stage OR NEW.status IS NOT OLD.status)
BEGIN SELECT RAISE(ABORT, 'archived project cannot transition'); END;
CREATE TRIGGER IF NOT EXISTS task_terminal_state BEFORE UPDATE OF stage, status ON tasks
WHEN (OLD.status = 'canceled' AND (NEW.stage IS NOT OLD.stage OR NEW.status IS NOT OLD.status))
    OR (OLD.stage = 'completed' AND NEW.stage IS NOT OLD.stage)
BEGIN SELECT RAISE(ABORT, 'terminal task cannot transition'); END;
"#;

pub async fn connect(path: &Path) -> DbResult<SqlitePool> {
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
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await?;
    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_one(&pool)
    .await?;
    if tables > 0 && version != 4 {
        return Err(DbError::UnsupportedSchema);
    }
    sqlx::raw_sql(SCHEMA).execute(&pool).await?;
    crate::delivery::initialize(&pool).await?;
    crate::resources::initialize(&pool).await?;
    crate::queue::initialize(&pool).await?;
    crate::replan_review::initialize(&pool).await?;
    seed_defaults(&pool).await?;
    sqlx::raw_sql("PRAGMA user_version = 4")
        .execute(&pool)
        .await?;
    Ok(pool)
}

async fn seed_defaults(pool: &SqlitePool) -> DbResult<()> {
    let agent_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agents")
        .fetch_one(pool)
        .await?;
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
    }
    ensure_default_team(pool).await
}

/// 默认团队始终覆盖全部已注册 Agent：按 Agent 的 roles 展开到各角色，
/// 已存在的条目保留优先级与启停，缺失的条目追加到角色末尾。全局 Agent
/// 被删除或角色集合变化时，对应条目随之清理并重排优先级。
pub async fn ensure_default_team(pool: &SqlitePool) -> DbResult<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let agents = sqlx::query_as::<_, AgentRow>(
        "SELECT * FROM agents ORDER BY priority ASC, created_at ASC, id ASC",
    )
    .fetch_all(&mut *tx)
    .await?;
    let existing = sqlx::query_as::<_, DefaultTeamRow>("SELECT * FROM default_team")
        .fetch_all(&mut *tx)
        .await?;
    let timestamp = crate::domain::now();
    for role in [ROLE_PLANNER, ROLE_EXECUTOR, ROLE_VERIFIER] {
        let mut ordered: Vec<&DefaultTeamRow> =
            existing.iter().filter(|row| row.role == role).collect();
        ordered.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then(a.agent_id.cmp(&b.agent_id))
        });
        let mut desired: Vec<&AgentRow> = Vec::new();
        for row in ordered {
            if let Some(agent) = agents.iter().find(|agent| agent.id == row.agent_id) {
                if parse_string_list(&agent.roles)
                    .iter()
                    .any(|item| item == role)
                {
                    desired.push(agent);
                }
            }
        }
        for agent in &agents {
            if parse_string_list(&agent.roles)
                .iter()
                .any(|item| item == role)
                && !desired.iter().any(|existing| existing.id == agent.id)
            {
                desired.push(agent);
            }
        }
        let desired_ids: Vec<&str> = desired.iter().map(|agent| agent.id.as_str()).collect();
        for row in existing.iter().filter(|row| row.role == role) {
            if !desired_ids.contains(&row.agent_id.as_str()) {
                sqlx::query("DELETE FROM default_team WHERE role = ? AND agent_id = ?")
                    .bind(role)
                    .bind(&row.agent_id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        for (priority, agent) in desired.iter().enumerate() {
            let exists = existing
                .iter()
                .any(|row| row.role == role && row.agent_id == agent.id);
            if exists {
                sqlx::query(
                    "UPDATE default_team SET priority = ?, agent_name = ?, updated_at = ? \
                     WHERE role = ? AND agent_id = ?",
                )
                .bind(priority as i64)
                .bind(&agent.name)
                .bind(&timestamp)
                .bind(role)
                .bind(&agent.id)
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query(
                    "INSERT INTO default_team \
                     (role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(role)
                .bind(&agent.id)
                .bind(&agent.name)
                .bind(priority as i64)
                .bind(agent.enabled)
                .bind(&timestamp)
                .bind(&timestamp)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn load_agent(pool: &SqlitePool, id: &str) -> DbResult<Option<AgentRow>> {
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
pub async fn ensure_project_agents(pool: &SqlitePool, project_id: &str) -> DbResult<()> {
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
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
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
) -> DbResult<ProjectAgentPool> {
    ensure_project_agents(pool, project_id).await?;
    let rows = sqlx::query_as::<_, ProjectAgentRow>(
        "SELECT * FROM project_agents WHERE project_id = ? ORDER BY role ASC, priority ASC, agent_id ASC",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(ProjectAgentPool {
        project_id: project_id.to_string(),
        roles: build_role_pools(pool, Some(project_id), &rows, PROJECT_POOL_ROLES).await?,
    })
}

/// 默认团队视图：返回 ProjectAgentPool 形态（project_id 固定为 "default"），
/// 条目带全局 Agent 的可用性与启停信息，供前端面板直接复用。
pub async fn load_default_team_pool(pool: &SqlitePool) -> DbResult<ProjectAgentPool> {
    let rows = default_team_pool_rows(pool).await?;
    Ok(ProjectAgentPool {
        project_id: "default".to_string(),
        roles: build_role_pools(pool, None, &rows, PROJECT_POOL_ROLES).await?,
    })
}

/// 默认团队按角色展开为项目池形态的行，角色顺序固定，条目按 priority 与
/// agent_id 排序保证稳定。
/// 默认团队按角色展开为项目池形态的行，角色顺序固定，条目按 priority 与
/// agent_id 排序保证稳定。启停与全局 Agent 状态合并，和继承语义保持一致。
async fn default_team_pool_rows(pool: &SqlitePool) -> DbResult<Vec<ProjectAgentRow>> {
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
/// 角色分组，项目池三种角色，任务池只有执行者与验收者。`project_id` 存在时，
/// 同一角色下项目池已停用的 Agent 会标记为项目层不可用；任务池据此展示项目
/// 池对已有任务的屏蔽状态。
async fn build_role_pools(
    pool: &SqlitePool,
    project_id: Option<&str>,
    rows: &[ProjectAgentRow],
    roles: &[&str],
) -> DbResult<Vec<ProjectRolePool>> {
    let agents = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents")
        .fetch_all(pool)
        .await?;
    let project_enabled = match project_id {
        Some(project_id) => {
            let project_rows = sqlx::query_as::<_, ProjectAgentRow>(
                "SELECT * FROM project_agents WHERE project_id = ?",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await?;
            project_rows
                .into_iter()
                .map(|row| ((row.role, row.agent_id), row.enabled != 0))
                .collect::<HashMap<_, _>>()
        }
        None => HashMap::new(),
    };
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
            let last_error = agent.and_then(|agent| agent.last_error.clone());
            let failure_summary = crate::failure::summarize(last_error.as_deref());
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
                project_enabled: project_enabled
                    .get(&(row.role.clone(), row.agent_id.clone()))
                    .copied()
                    .unwrap_or(true),
                available: unavailable_reason.is_none(),
                unavailable_reason,
                max_concurrency: agent.map(|agent| agent.max_concurrency.max(1)).unwrap_or(1),
                allow_priority_fallback: agent
                    .map(|agent| agent.allow_priority_fallback != 0)
                    .unwrap_or(false),
                cooldown_until: agent.and_then(|agent| agent.cooldown_until.clone()),
                last_error,
                failure_kind: agent.and_then(|agent| agent.failure_kind.clone()),
                failure_summary,
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

/// 在已有事务内初始化任务池：调用方创建任务时在同一事务内完成快照。
/// 空池也是有效快照；读取与调度均不重新填充。调用方应保证项目池已存在。
pub async fn initialize_task_agents_tx(
    executor: &mut sqlx::SqliteConnection,
    task_id: &str,
) -> DbResult<()> {
    let timestamp = crate::domain::now();
    sqlx::query(
        "INSERT INTO task_agents \
         (task_id, role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
         SELECT ?, role, agent_id, agent_name, priority, enabled, ?, ? FROM project_agents \
         WHERE project_id = (SELECT project_id FROM tasks WHERE id = ?) AND role IN (?, ?)",
    )
    .bind(task_id)
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(task_id)
    .bind(ROLE_EXECUTOR)
    .bind(ROLE_VERIFIER)
    .execute(executor)
    .await?;
    Ok(())
}

/// 新建 Agent 时同步到存量 Agent 池的写入统计。
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentPoolSyncSummary {
    pub project_entries: i64,
    pub task_entries: i64,
}

/// 把已注册的 Agent 追加到全部未归档项目和未结束任务的 Agent 池末尾。
///
/// 项目池按 Agent 的规划/执行/验收角色追加，任务池只追加执行者与验收者。
/// 写入使用单个 `BEGIN IMMEDIATE` 事务，优先级取角色内 `MAX(priority)+1`，
/// 并通过 `INSERT OR IGNORE` 保持幂等；已归档项目、已完成或已取消任务不参与。
pub async fn add_agent_to_existing_pools(
    pool: &SqlitePool,
    agent_id: &str,
) -> DbResult<AgentPoolSyncSummary> {
    let Some(agent) = load_agent(pool, agent_id).await? else {
        return Ok(AgentPoolSyncSummary::default());
    };
    let roles = parse_string_list(&agent.roles);
    let project_roles: Vec<&str> = PROJECT_POOL_ROLES
        .iter()
        .copied()
        .filter(|role| roles.iter().any(|item| item.as_str() == *role))
        .collect();
    let task_roles: Vec<&str> = TASK_POOL_ROLES
        .iter()
        .copied()
        .filter(|role| roles.iter().any(|item| item.as_str() == *role))
        .collect();
    if project_roles.is_empty() && task_roles.is_empty() {
        return Ok(AgentPoolSyncSummary::default());
    }

    let timestamp = crate::domain::now();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let mut summary = AgentPoolSyncSummary::default();

    for role in &project_roles {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO project_agents \
             (project_id, role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
             SELECT p.id, ?, ?, ?, \
                COALESCE((SELECT MAX(pa.priority) + 1 FROM project_agents pa \
                          WHERE pa.project_id = p.id AND pa.role = ?), 0), \
                ?, ?, ? \
             FROM projects p \
             WHERE COALESCE(p.status, '') != ?",
        )
        .bind(role)
        .bind(&agent.id)
        .bind(&agent.name)
        .bind(role)
        .bind(agent.enabled)
        .bind(&timestamp)
        .bind(&timestamp)
        .bind(STATUS_ARCHIVED)
        .execute(&mut *tx)
        .await?;
        summary.project_entries += result.rows_affected() as i64;
    }

    if !task_roles.is_empty() {
        for role in &task_roles {
            let result = sqlx::query(
                "INSERT OR IGNORE INTO task_agents \
                 (task_id, role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
                 SELECT t.id, ?, ?, ?, \
                    COALESCE((SELECT MAX(ta.priority) + 1 FROM task_agents ta \
                              WHERE ta.task_id = t.id AND ta.role = ?), 0), \
                    ?, ?, ? \
                 FROM tasks t JOIN projects p ON p.id = t.project_id \
                 WHERE COALESCE(p.status, '') != ? \
                   AND t.stage != ? \
                   AND COALESCE(t.status, '') != ?",
            )
            .bind(role)
            .bind(&agent.id)
            .bind(&agent.name)
            .bind(role)
            .bind(agent.enabled)
            .bind(&timestamp)
            .bind(&timestamp)
            .bind(STATUS_ARCHIVED)
            .bind(TASK_STAGE_COMPLETED)
            .bind(STATUS_CANCELED)
            .execute(&mut *tx)
            .await?;
            summary.task_entries += result.rows_affected() as i64;
        }
    }

    tx.commit().await?;
    Ok(summary)
}

pub async fn load_task_agent_pool(
    pool: &SqlitePool,
    project_id: &str,
    task_id: &str,
) -> DbResult<TaskAgentPool> {
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
        roles: build_role_pools(pool, Some(project_id), &rows, TASK_POOL_ROLES).await?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_other_schema_versions_without_rewriting_data() {
        let root = std::env::temp_dir().join(format!("domo-schema-{}", Uuid::new_v4()));
        let path = root.join("domo.db");
        let pool = connect(&path).await.unwrap();
        sqlx::raw_sql("PRAGMA user_version = 99")
            .execute(&pool)
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agents")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(matches!(
            connect(&path).await,
            Err(DbError::UnsupportedSchema)
        ));
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(version, 99);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agents")
                .fetch_one(&pool)
                .await
                .unwrap(),
            count
        );
        pool.close().await;
        let _ = std::fs::remove_dir_all(root);
    }
}
