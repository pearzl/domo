//! 持久化与迁移：**唯一写 SQL 的地方**。
//!
//! - 全进程只有一个 SQLite 连接，由 `state::Db = Mutex<Connection>` 保护。
//!   **绝不在持锁期间做阻塞操作**（git、网络、子进程、大文件读写）：先取快照 → 释放锁 →
//!   做耗时工作 → 重新加锁写回。持锁跑 git 会造成服务级冻结（HTTP 全部无响应、调度器停摆）。
//! - 迁移必须幂等：`CREATE TABLE IF NOT EXISTS` + `ensure_column`，对既有库安全，
//!   不得要求用户删库；测试用 `migrate_for_tests`。
//! - 时间统一为毫秒时间戳（`now()`）；路径入库前统一转绝对路径。
//! - 事件 append-only、归档只增不改（见 `events`）。

use crate::models::*;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub fn new_id(prefix: &str) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}{}", &u[..12])
}

pub fn open(path: &str) -> Connection {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let conn = Connection::open(path).expect("open sqlite db");
    let _ = conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;",
    );
    migrate(&conn);
    // M4：启动期三档恢复标记——没有 worker 能跨重启存活，把「中断的执行中任务」
    // 与「第三档人工重建挂起的任务」置为待恢复（实际派发仍受项目 paused 冻结约束）。
    crate::recovery::boot_mark(&conn);
    conn
}

/// 测试用：在给定连接上执行与 `db::open` 相同的迁移（不含启动期恢复标记）。
#[cfg(test)]
pub fn migrate_for_tests(c: &Connection) {
    migrate(c);
}

fn migrate(c: &Connection) {
    c.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS projects (
            id TEXT PRIMARY KEY, name TEXT NOT NULL, brief TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL, plan_content TEXT, confirmed_at INTEGER, created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS engineerings (
            id TEXT PRIMARY KEY, name TEXT NOT NULL, brief TEXT NOT NULL DEFAULT '',
            workspace_kind TEXT NOT NULL DEFAULT 'single', workspace_path TEXT,
            default_agent_pool TEXT, constraints TEXT, created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS milestones (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT NOT NULL,
            goal TEXT, effect TEXT, task_ids TEXT NOT NULL DEFAULT '[]', status TEXT NOT NULL DEFAULT 'pending'
        );
        CREATE TABLE IF NOT EXISTS tasks (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL, milestone_id TEXT,
            title TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
            depends_on TEXT NOT NULL DEFAULT '[]',
            assignee_role TEXT NOT NULL DEFAULT 'executor',
            executor_agent_id TEXT, acceptor_agent_id TEXT,
            acceptance_required INTEGER NOT NULL DEFAULT 1,
            acceptance_criteria TEXT, negotiation_status TEXT NOT NULL DEFAULT 'agreed',
            negotiation_round INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'planned',
            attempt INTEGER NOT NULL DEFAULT 0,
            rejection_count INTEGER NOT NULL DEFAULT 0,
            failure_count INTEGER NOT NULL DEFAULT 0,
            artifacts TEXT NOT NULL DEFAULT '[]',
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS agents (
            id TEXT PRIMARY KEY, name TEXT NOT NULL, adapter TEXT NOT NULL,
            roles TEXT NOT NULL, config TEXT NOT NULL DEFAULT '{}',
            capabilities TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'idle',
            current_task_ids TEXT NOT NULL DEFAULT '[]'
        );
        CREATE TABLE IF NOT EXISTS inbox (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
            "type" TEXT NOT NULL, payload TEXT NOT NULL DEFAULT '{}',
            status TEXT NOT NULL DEFAULT 'open', resolution TEXT, created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL,
            project_id TEXT NOT NULL, task_id TEXT, "type" TEXT NOT NULL, payload TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS guidance (
            id TEXT PRIMARY KEY, task_id TEXT NOT NULL, text TEXT NOT NULL, created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL,
            ts INTEGER NOT NULL, line TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS decisions (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL, task_id TEXT,
            kind TEXT NOT NULL, request TEXT NOT NULL DEFAULT '{}',
            scope TEXT NOT NULL DEFAULT 'tactical',
            status TEXT NOT NULL DEFAULT 'pending',
            ruling TEXT, attempt INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL, resolved_at INTEGER
        );
        CREATE TABLE IF NOT EXISTS reports (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL UNIQUE,
            status TEXT NOT NULL DEFAULT 'pending',
            content TEXT, narrative TEXT, error TEXT, path TEXT,
            facts TEXT NOT NULL DEFAULT '{}',
            attempts INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            generated_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project_id);
        CREATE INDEX IF NOT EXISTS idx_events_project ON events(project_id);
        CREATE INDEX IF NOT EXISTS idx_events_task ON events(task_id);
        CREATE INDEX IF NOT EXISTS idx_logs_task ON logs(task_id);
        CREATE INDEX IF NOT EXISTS idx_decisions_project ON decisions(project_id);
        CREATE INDEX IF NOT EXISTS idx_decisions_task ON decisions(task_id);
        CREATE INDEX IF NOT EXISTS idx_reports_project ON reports(project_id);
        "#,
    )
    .expect("migrate");

    // M2: projects.workspace (TEXT JSON, nullable) — idempotent column add
    add_column_if_missing(c, "projects", "workspace", "TEXT");
    // M6 工程层：项目归属工程（旧数据为 NULL，后续由人迁移）
    add_column_if_missing(c, "projects", "engineering_id", "TEXT");
    c.execute_batch("CREATE INDEX IF NOT EXISTS idx_projects_engineering ON projects(engineering_id);")
        .ok();
    // M7 验收机械检查（只支持 file_exists / file_not_exists / command 三类结构）
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS task_checks (
            task_id TEXT NOT NULL, idx INTEGER NOT NULL,
            kind TEXT NOT NULL, spec TEXT NOT NULL,
            PRIMARY KEY(task_id, idx)
        );
        CREATE TABLE IF NOT EXISTS acceptance_evidence (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id TEXT NOT NULL, attempt INTEGER NOT NULL,
            ts INTEGER NOT NULL, results TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_acceptance_evidence_task ON acceptance_evidence(task_id, id);",
    )
    .ok();
    // M2.5: 项目 agent 池（JSON 数组；NULL=全部 agent）
    add_column_if_missing(c, "projects", "agent_pool", "TEXT");
    // M4: 项目工作区内登记的 git 仓库（JSON [{rel,main}]）
    add_column_if_missing(c, "projects", "repos", "TEXT");
    // M8/ARCH §15：项目基线（单仓 {branch|commit}；多仓 {repos:{rel:branch}}）
    add_column_if_missing(c, "projects", "baseline", "TEXT");
    // M4: 任务涉及的仓库 scope（JSON rel 数组）
    add_column_if_missing(c, "tasks", "scope_repos", "TEXT");
    // M5：agent 运行原始输出归档（模型输出已由我们获得 → 自行存档，供审计回放与结论复核）
    let _ = c.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_outputs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id TEXT,
            project_id TEXT NOT NULL,
            phase TEXT NOT NULL DEFAULT '',
            attempt INTEGER NOT NULL DEFAULT 0,
            session TEXT NOT NULL DEFAULT '',
            real_session_id TEXT,
            ts INTEGER NOT NULL,
            outcome TEXT NOT NULL DEFAULT '',
            text TEXT NOT NULL DEFAULT '',
            raw TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_agent_outputs_task ON agent_outputs(task_id, id);",
    );
    // M4.5：人类指定 agent（固定）与已尝试 agent 记录（失败换人排除）
    add_column_if_missing(c, "tasks", "agent_pinned", "INTEGER NOT NULL DEFAULT 0");
    add_column_if_missing(c, "tasks", "tried_agents", "TEXT NOT NULL DEFAULT '{}'");

    // M3: tasks.milestone_id (TEXT, nullable) — idempotent column add. The base
    // CREATE TABLE above already includes it for fresh DBs; older DBs get it here.
    add_column_if_missing(c, "tasks", "milestone_id", "TEXT");

    // M3 协商：tasks.negotiation_round（已完成轮次）— 幂等列添加。
    add_column_if_missing(c, "tasks", "negotiation_round", "INTEGER NOT NULL DEFAULT 0");

    // ARCH A2：任务状态条件更新版本号（每次状态变更 +1，派发前 CAS 校验）。
    add_column_if_missing(c, "tasks", "version", "INTEGER NOT NULL DEFAULT 0");

    // M4 三档恢复（DESIGN §10）：中断任务的恢复状态列（幂等列添加）。
    //   recovery_pending: 1=待走三档恢复流程（启动标记 / 档位切换时保持）
    //   recovery_tier:    下一个要尝试的档位（native_resume | handoff）
    //   recovery_manual:  1=第三档人工重建挂起（suspended），下一次重启重新进一轮三档
    // M4：任务计划（执行者产出）+ 计划审查打回计数
    add_column_if_missing(c, "tasks", "execution_plan", "TEXT");
    add_column_if_missing(c, "tasks", "plan_rejection_count", "INTEGER NOT NULL DEFAULT 0");
    add_column_if_missing(c, "tasks", "recovery_pending", "INTEGER NOT NULL DEFAULT 0");
    add_column_if_missing(c, "tasks", "recovery_tier", "TEXT NOT NULL DEFAULT ''");
    add_column_if_missing(c, "tasks", "recovery_manual", "INTEGER NOT NULL DEFAULT 0");

    // M4：逻辑会话 -> 真实 opencode 会话 id 的持久化映射（原仅存内存，kill -9 即丢）。
    // 执行派发（phase='execute'）在 CLI 输出里解析到真实 id 就落库，重启后可据此原生 resume。
    c.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS session_map (
            logical TEXT PRIMARY KEY,
            real TEXT NOT NULL,
            task_id TEXT, phase TEXT, attempt INTEGER,
            updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_session_map_task ON session_map(task_id, phase, attempt);
        "#,
    )
    .expect("session_map");

    // M3: indexes for milestone lookups (idempotent).
    c.execute_batch(
        r#"
        CREATE INDEX IF NOT EXISTS idx_milestones_project ON milestones(project_id);
        CREATE INDEX IF NOT EXISTS idx_tasks_milestone ON tasks(milestone_id);
        "#,
    )
    .expect("milestone indexes");

    // M5 项目知识库：人工指导沉淀的项目规范条目（随 SQLite 持久化）。
    //   scope:      'project'=仅来源项目可见 | 'global'=全部项目可见（跨项目复用）
    //   applies_to: 适用范围说明（可空）
    //   source_event_id: 来源人工指导事件（events.id，回链审计）
    c.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS knowledge (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
            scope TEXT NOT NULL DEFAULT 'project',
            body TEXT NOT NULL, applies_to TEXT,
            source_event_id INTEGER NOT NULL,
            created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_knowledge_project ON knowledge(project_id);
        "#,
    )
    .expect("knowledge");

    // boot recovery: no in-memory worker survives a restart; agent slots must be freed
    c.execute_batch("UPDATE agents SET current_task_ids='[]', status='idle';")
        .expect("boot recovery");

    // M3 报告：无内存 worker 能跨重启存活，中断在 pending/generating 的报告统一置为
    // failed（保留已有内容），由前端/API 手工重试——与「报告生成失败可重试」语义一致。
    c.execute_batch(
        "UPDATE reports SET status='failed', \
         error='报告生成在进程重启时中断，可在前端或 API 手工重试', \
         updated_at=(strftime('%s','now')*1000) WHERE status IN ('pending','generating');",
    )
    .expect("report boot recovery");
}

/// 数据目录：与 SQLite 同级的目录（默认 `data/`），用于落盘报告等产出。
/// 由 `DOMO_DB` 的父目录推导，未设置时回退到 `data`。
pub fn data_dir() -> std::path::PathBuf {
    std::env::var("DOMO_DB")
        .ok()
        .map(std::path::PathBuf::from)
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::PathBuf::from("data"))
}

/// Idempotent column add: only ALTER when PRAGMA reports it missing. Safe to
/// run on every boot (including against an already-migrated DB).
fn add_column_if_missing(c: &Connection, table: &str, column: &str, decl: &str) {
    let sql = format!("PRAGMA table_info({table})");
    let mut st = match c.prepare(&sql) {
        Ok(st) => st,
        Err(_) => return,
    };
    let exists = st
        .query_map([], |r| r.get::<_, String>(1))
        .map(|rows| rows.filter_map(|x| x.ok()).any(|n| n == column))
        .unwrap_or(false);
    drop(st);
    if !exists {
        let ddl = format!("ALTER TABLE {table} ADD COLUMN {column} {decl};");
        c.execute_batch(&ddl)
            .unwrap_or_else(|e| panic!("add {table}.{column} column: {e}"));
    }
}

// ---------- row mapping ----------

fn project_row(r: &Row) -> rusqlite::Result<Project> {
    let confirmed_at: Option<i64> = r.get("confirmed_at")?;
    let plan = confirmed_at.map(|t| Plan {
        confirmed_at: t,
        content: r.get("plan_content").unwrap_or(None),
    });
    let workspace: Option<String> = r.get("workspace")?;
    let agent_pool: Option<String> = r.get("agent_pool").ok();
    let agent_pool = agent_pool.and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok());
    let repos: Vec<crate::models::RepoRef> = r
        .get::<_, Option<String>>("repos")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let baseline = r
        .get::<_, Option<String>>("baseline")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok());
    Ok(Project {
        id: r.get("id")?,
        engineering_id: r.get("engineering_id").ok().flatten(),
        name: r.get("name")?,
        brief: r.get("brief")?,
        status: r.get("status")?,
        plan,
        workspace: workspace.and_then(|s| serde_json::from_str(&s).ok()),
        agent_pool,
        repos,
        baseline,
        created_at: r.get("created_at")?,
    })
}

// ---------- 工程（engineering） ----------

fn engineering_row(r: &Row) -> rusqlite::Result<Engineering> {
    let pool: Option<String> = r.get("default_agent_pool").ok().flatten();
    Ok(Engineering {
        id: r.get("id")?,
        name: r.get("name")?,
        brief: r.get("brief")?,
        workspace_kind: r.get("workspace_kind")?,
        workspace_path: r.get("workspace_path")?,
        default_agent_pool: pool.and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok()),
        constraints: r.get("constraints")?,
        created_at: r.get("created_at")?,
    })
}

pub fn list_engineerings(c: &Connection) -> Vec<Engineering> {
    let mut st = c
        .prepare("SELECT * FROM engineerings ORDER BY created_at, id")
        .unwrap();
    st.query_map([], engineering_row)
        .unwrap()
        .filter_map(|x| x.ok())
        .collect()
}

pub fn get_engineering(c: &Connection, id: &str) -> Option<Engineering> {
    c.query_row("SELECT * FROM engineerings WHERE id=?1", params![id], engineering_row)
        .optional()
        .ok()
        .flatten()
}

pub fn insert_engineering(c: &Connection, e: &Engineering) {
    let pool = e
        .default_agent_pool
        .as_ref()
        .map(|p| serde_json::to_string(p).unwrap_or_else(|_| "[]".into()));
    c.execute(
        "INSERT INTO engineerings (id,name,brief,workspace_kind,workspace_path,default_agent_pool,constraints,created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![e.id, e.name, e.brief, e.workspace_kind, e.workspace_path, pool, e.constraints, e.created_at],
    )
    .expect("insert engineering");
}

pub fn update_engineering(c: &Connection, e: &Engineering) {
    let pool = e
        .default_agent_pool
        .as_ref()
        .map(|p| serde_json::to_string(p).unwrap_or_else(|_| "[]".into()));
    c.execute(
        "UPDATE engineerings SET name=?2, brief=?3, workspace_kind=?4, workspace_path=?5,
                                 default_agent_pool=?6, constraints=?7 WHERE id=?1",
        params![e.id, e.name, e.brief, e.workspace_kind, e.workspace_path, pool, e.constraints],
    )
    .expect("update engineering");
}

/// 某工程下的项目（按创建时间）。
pub fn list_projects_of(c: &Connection, engineering_id: &str) -> Vec<Project> {
    let mut st = c
        .prepare("SELECT * FROM projects WHERE engineering_id=?1 ORDER BY created_at, id")
        .unwrap();
    st.query_map(params![engineering_id], project_row)
        .unwrap()
        .filter_map(|x| x.ok())
        .collect()
}

pub fn set_project_engineering(c: &Connection, project_id: &str, engineering_id: Option<&str>) {
    let _ = c.execute(
        "UPDATE projects SET engineering_id=?2 WHERE id=?1",
        params![project_id, engineering_id],
    );
}

fn task_row(r: &Row) -> rusqlite::Result<Task> {
    let dep_s: String = r.get("depends_on")?;
    let art_s: String = r.get("artifacts")?;
    Ok(Task {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        milestone_id: r.get("milestone_id")?,
        title: r.get("title")?,
        description: r.get("description")?,
        depends_on: serde_json::from_str(&dep_s).unwrap_or_default(),
        assignee_role: r.get("assignee_role")?,
        executor_agent_id: r.get("executor_agent_id")?,
        acceptor_agent_id: r.get("acceptor_agent_id")?,
        acceptance_required: r.get::<_, i64>("acceptance_required")? != 0,
        acceptance_criteria: r.get("acceptance_criteria")?,
        negotiation_status: r.get("negotiation_status")?,
        execution_plan: r.get("execution_plan").ok().flatten(),
        plan_rejection_count: r.get("plan_rejection_count").ok().unwrap_or(0),
        scope_repos: r
            .get::<_, Option<String>>("scope_repos")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default(),
        agent_pinned: r.get::<_, i64>("agent_pinned").unwrap_or(0) != 0,
        tried_agents: r
            .get::<_, Option<String>>("tried_agents")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or_else(|| serde_json::json!({})),
        negotiation_round: r.get("negotiation_round").unwrap_or(0),
        status: r.get("status")?,
        version: r.get("version").unwrap_or(0),
        attempt: r.get("attempt")?,
        rejection_count: r.get("rejection_count")?,
        failure_count: r.get("failure_count")?,
        artifacts: serde_json::from_str(&art_s).unwrap_or_default(),
        created_at: r.get("created_at")?,
    })
}

fn milestone_row(r: &Row) -> rusqlite::Result<Milestone> {
    let task_ids_s: String = r.get("task_ids")?;
    Ok(Milestone {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        name: r.get("name")?,
        goal: r.get("goal")?,
        effect: r.get("effect")?,
        task_ids: serde_json::from_str(&task_ids_s).unwrap_or_default(),
        status: r.get("status")?,
    })
}

pub fn agent_row(r: &Row) -> rusqlite::Result<Agent> {
    let roles_s: String = r.get("roles")?;
    let cfg_s: String = r.get("config")?;
    let cap_s: String = r.get("capabilities")?;
    let occ_s: String = r.get("current_task_ids")?;
    let mut occ: Vec<String> = serde_json::from_str(&occ_s).unwrap_or_default();
    occ.retain(|t| !t.is_empty());
    let roles: Vec<String> = serde_json::from_str(&roles_s).unwrap_or_default();
    let status = if occ.is_empty() { "idle".to_string() } else { "busy".to_string() };
    Ok(Agent {
        id: r.get("id")?,
        name: r.get("name")?,
        adapter: r.get("adapter")?,
        roles,
        config: serde_json::from_str(&cfg_s).unwrap_or_else(|_| serde_json::json!({})),
        capabilities: serde_json::from_str(&cap_s).unwrap_or_default(),
        status,
        current_task_ids: occ,
    })
}

fn inbox_row(r: &Row) -> rusqlite::Result<InboxItem> {
    let res_s: Option<String> = r.get("resolution")?;
    let payload_s: String = r.get("payload")?;
    Ok(InboxItem {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        kind: r.get("type")?,
        payload: serde_json::from_str(&payload_s).unwrap_or_else(|_| serde_json::json!({})),
        status: r.get("status")?,
        resolution: res_s.and_then(|s| serde_json::from_str(&s).ok()),
        created_at: r.get("created_at")?,
    })
}

fn event_row(r: &Row) -> rusqlite::Result<Event> {
    let p: String = r.get("payload")?;
    Ok(Event {
        id: r.get("id")?,
        ts: r.get("ts")?,
        project_id: r.get("project_id")?,
        task_id: r.get("task_id")?,
        kind: r.get("type")?,
        payload: serde_json::from_str(&p).unwrap_or_else(|_| serde_json::json!({})),
    })
}

fn decision_row(r: &Row) -> rusqlite::Result<Decision> {
    let req: String = r.get("request")?;
    let ruling: Option<String> = r.get("ruling")?;
    Ok(Decision {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        task_id: r.get("task_id")?,
        kind: r.get("kind")?,
        request: serde_json::from_str(&req).unwrap_or_else(|_| serde_json::json!({})),
        scope: r.get("scope")?,
        status: r.get("status")?,
        ruling: ruling.and_then(|s| serde_json::from_str(&s).ok()),
        attempt: r.get("attempt")?,
        created_at: r.get("created_at")?,
        resolved_at: r.get("resolved_at")?,
    })
}

fn report_row(r: &Row) -> rusqlite::Result<Report> {
    let facts_s: String = r.get("facts").unwrap_or_else(|_| "{}".into());
    Ok(Report {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        status: r.get("status")?,
        content: r.get("content")?,
        narrative: r.get("narrative")?,
        error: r.get("error")?,
        path: r.get("path")?,
        facts: serde_json::from_str(&facts_s).unwrap_or_else(|_| serde_json::json!({})),
        attempts: r.get("attempts")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        generated_at: r.get("generated_at")?,
    })
}

// ---------- read ----------

pub fn get_project(c: &Connection, id: &str) -> Option<Project> {
    c.query_row("SELECT * FROM projects WHERE id=?1", params![id], project_row).optional().ok().flatten()
}
pub fn list_projects(c: &Connection) -> Vec<Project> {
    let mut st = c.prepare("SELECT * FROM projects ORDER BY created_at, id").unwrap();
    let v = st.query_map([], project_row).unwrap().filter_map(|x| x.ok()).collect();
    v
}
// ---------- 验收机械检查（M7） ----------

/// 覆盖式写入任务的机械检查项（每项是 `{"type": ...}` 的 JSON）。
pub fn set_task_checks(c: &Connection, task_id: &str, checks: &[Value]) {
    let _ = c.execute("DELETE FROM task_checks WHERE task_id=?1", params![task_id]);
    for (i, chk) in checks.iter().enumerate() {
        let kind = chk.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
        let _ = c.execute(
            "INSERT INTO task_checks (task_id, idx, kind, spec) VALUES (?1,?2,?3,?4)",
            params![task_id, i as i64, kind, chk.to_string()],
        );
    }
}

pub fn get_task_checks(c: &Connection, task_id: &str) -> Vec<Value> {
    let mut st = match c.prepare("SELECT spec FROM task_checks WHERE task_id=?1 ORDER BY idx") {
        Ok(st) => st,
        Err(_) => return vec![],
    };
    st.query_map(params![task_id], |r| r.get::<_, String>(0))
        .map(|rows| {
            rows.filter_map(|x| x.ok())
                .filter_map(|s| serde_json::from_str::<Value>(&s).ok())
                .collect()
        })
        .unwrap_or_default()
}

pub fn insert_acceptance_evidence(c: &Connection, task_id: &str, attempt: i64, results: &Value) {
    let _ = c.execute(
        "INSERT INTO acceptance_evidence (task_id, attempt, ts, results) VALUES (?1,?2,?3,?4)",
        params![task_id, attempt, now(), results.to_string()],
    );
}

pub fn latest_acceptance_evidence(c: &Connection, task_id: &str) -> Option<Value> {
    c.query_row(
        "SELECT results FROM acceptance_evidence WHERE task_id=?1 ORDER BY id DESC LIMIT 1",
        params![task_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .and_then(|s| serde_json::from_str(&s).ok())
}

pub fn list_tasks(c: &Connection, project_id: &str) -> Vec<Task> {    let mut st = c.prepare("SELECT * FROM tasks WHERE project_id=?1 ORDER BY created_at, id").unwrap();
    let v = st.query_map(params![project_id], task_row).unwrap().filter_map(|x| x.ok()).collect();
    v
}
pub fn list_milestones(c: &Connection, project_id: &str) -> Vec<Milestone> {
    let mut st = c.prepare("SELECT * FROM milestones WHERE project_id=?1 ORDER BY rowid").unwrap();
    let v = st.query_map(params![project_id], milestone_row).unwrap().filter_map(|x| x.ok()).collect();
    v
}
pub fn get_milestone(c: &Connection, id: &str) -> Option<Milestone> {
    c.query_row("SELECT * FROM milestones WHERE id=?1", params![id], milestone_row).optional().ok().flatten()
}
/// Tasks belonging to a milestone (authoritative membership: tasks.milestone_id).
pub fn tasks_of_milestone(c: &Connection, project_id: &str, milestone_id: &str) -> Vec<Task> {
    list_tasks(c, project_id)
        .into_iter()
        .filter(|t| t.milestone_id.as_deref() == Some(milestone_id))
        .collect()
}
/// An open milestone_gate inbox item for a milestone, if any (recovery/idempotency).
pub fn open_gate_item(c: &Connection, milestone_id: &str) -> Option<InboxItem> {
    list_inbox(c)
        .into_iter()
        .find(|i| i.kind == "milestone_gate" && i.status == "open"
            && i.payload.get("milestone_id").and_then(|v| v.as_str()) == Some(milestone_id))
}

/// M3: an open milestone-window (`replan` kind with payload.kind="milestone_window")
/// inbox item for a milestone, if any. Used to keep the replanning window item
/// idempotent across reloads / repeated gate rejections.
pub fn open_window_item(c: &Connection, milestone_id: &str) -> Option<InboxItem> {
    list_inbox(c)
        .into_iter()
        .find(|i| i.kind == "replan" && i.status == "open"
            && i.payload.get("kind").and_then(|v| v.as_str()) == Some("milestone_window")
            && i.payload.get("milestone_id").and_then(|v| v.as_str()) == Some(milestone_id))
}

/// All open milestone-window inbox items of a project (used when re-arming).
#[allow(dead_code)]
pub fn open_window_items(c: &Connection, project_id: &str) -> Vec<InboxItem> {
    list_inbox(c)
        .into_iter()
        .filter(|i| i.kind == "replan" && i.status == "open"
            && i.project_id == project_id
            && i.payload.get("kind").and_then(|v| v.as_str()) == Some("milestone_window"))
        .collect()
}

/// Whether a milestone ever had a replanning window opened (open *or* resolved),
/// regardless of inbox status. Used to gate the `failed → awaiting_confirm`
/// re-entry so a stale `failed` milestone without any window is left frozen.
pub fn has_window_item(c: &Connection, milestone_id: &str) -> bool {
    list_inbox(c).into_iter().any(|i| {
        i.kind == "replan"
            && i.payload.get("kind").and_then(|v| v.as_str()) == Some("milestone_window")
            && i.payload.get("milestone_id").and_then(|v| v.as_str()) == Some(milestone_id)
    })
}
/// ARCH §5.2：派发条件更新（compare-and-set）。
///
/// 仅当 `id`、`version`、当前状态都在预期内时更新为 `to`、写入 attempt 并把 version+1。
/// 返回 false = 已被其他唤醒/操作抢先，调用方必须放弃本次派发。
pub fn cas_task_dispatch(
    c: &Connection,
    id: &str,
    expected_version: i64,
    from: &[&str],
    to: &str,
    attempt: i64,
) -> bool {
    if from.is_empty() {
        return false;
    }
    let placeholders = vec!["?"; from.len()].join(",");
    let sql = format!(
        "UPDATE tasks SET status=?, attempt=?, version=version+1 \
         WHERE id=? AND version=? AND status IN ({placeholders})"
    );
    let mut vals: Vec<rusqlite::types::Value> = vec![
        rusqlite::types::Value::Text(to.to_string()),
        rusqlite::types::Value::Integer(attempt),
        rusqlite::types::Value::Text(id.to_string()),
        rusqlite::types::Value::Integer(expected_version),
    ];
    vals.extend(from.iter().map(|s| rusqlite::types::Value::Text((*s).to_string())));
    c.execute(&sql, rusqlite::params_from_iter(vals))
        .map(|n| n == 1)
        .unwrap_or(false)
}

/// 仅“占用”一个阶段（状态不变，例如 plan_review / awaiting_acceptance），
/// 用 version+1 保证同一版本只能被一个派发者抢到。
pub fn cas_task_claim(c: &Connection, id: &str, expected_version: i64, status: &str) -> bool {
    c.execute(
        "UPDATE tasks SET version=version+1 WHERE id=?1 AND version=?2 AND status=?3",
        params![id, expected_version, status],
    )
    .map(|n| n == 1)
    .unwrap_or(false)
}

/// 停机时把 WAL 落盘（尽力而为；失败不阻塞退出）。
pub fn checkpoint(c: &Connection) {
    let _ = c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
}

pub fn get_task(c: &Connection, id: &str) -> Option<Task> {
    c.query_row("SELECT * FROM tasks WHERE id=?1", params![id], task_row).optional().ok().flatten()
}
pub fn list_agents(c: &Connection) -> Vec<Agent> {
    // 软删除：status='deleted' 的实例仅在审计快照中可见，不进运行池
    let mut st = c.prepare("SELECT * FROM agents WHERE status!='deleted' ORDER BY rowid").unwrap();
    let v = st.query_map([], agent_row).unwrap().filter_map(|x| x.ok()).collect();
    v
}
pub fn get_agent(c: &Connection, id: &str) -> Option<Agent> {
    c.query_row("SELECT * FROM agents WHERE id=?1", params![id], agent_row).optional().ok().flatten()
}
/// ARCH §8.3 启动对账：合并重复的 open 收件箱条目。
///
/// 同一项目、同一类型、同一目标（task / milestone / decision）且同一 reason 的 open
/// 条目只保留最早一条；其余标记为 resolved（resolution 记录去重原因）。幂等。
pub fn dedupe_open_inbox(c: &Connection) -> usize {
    let mut seen = std::collections::HashSet::new();
    let mut resolved = 0usize;
    for i in list_inbox(c).into_iter().filter(|i| i.status == "open") {
        let field = |k: &str| i.payload.get(k).and_then(|v| v.as_str()).unwrap_or("");
        let key = format!(
            "{}|{}|{}|{}|{}|{}",
            i.project_id,
            i.kind,
            field("task_id"),
            field("milestone_id"),
            field("decision_id"),
            field("reason"),
        );
        // 没有目标信息的条目无法可靠判定重复 → 一律保留。
        if key.ends_with("|||||") || seen.insert(key) {
            continue;
        }
        let resolution = serde_json::json!({"resolution": "deduped", "note": "startup reconciliation"});
        if c.execute(
            "UPDATE inbox SET status='resolved', resolution=?2 WHERE id=?1 AND status='open'",
            params![i.id, resolution.to_string()],
        )
        .map(|n| n == 1)
        .unwrap_or(false)
        {
            resolved += 1;
        }
    }
    resolved
}

pub fn list_inbox(c: &Connection) -> Vec<InboxItem> {
    let mut st = c.prepare("SELECT * FROM inbox ORDER BY (status='open') DESC, created_at, id").unwrap();
    let v = st.query_map([], inbox_row).unwrap().filter_map(|x| x.ok()).collect();
    v
}
pub fn get_inbox(c: &Connection, id: &str) -> Option<InboxItem> {
    c.query_row("SELECT * FROM inbox WHERE id=?1", params![id], inbox_row).optional().ok().flatten()
}
pub fn list_events(c: &Connection, project_id: Option<&str>, task_id: Option<&str>) -> Vec<Event> {
    let (sql, params_): (String, Vec<String>) = match (project_id, task_id) {
        (Some(p), Some(t)) => ("SELECT * FROM events WHERE project_id=?1 AND task_id=?2 ORDER BY id".to_string(), vec![p.into(), t.into()]),
        (Some(p), None) => ("SELECT * FROM events WHERE project_id=?1 ORDER BY id".to_string(), vec![p.into()]),
        (None, Some(t)) => ("SELECT * FROM events WHERE task_id=?1 ORDER BY id".to_string(), vec![t.into()]),
        (None, None) => ("SELECT * FROM events ORDER BY id".to_string(), vec![]),
    };
    let mut st = c.prepare(&sql).unwrap();
    let iter = st.query_map(rusqlite::params_from_iter(params_), event_row).unwrap();
    iter.filter_map(|x| x.ok()).collect()
}

/// 审计视图专用：排序方向 + 分页 + 时间范围 + 类型过滤，返回 (本页事件, 符合条件的总数)。
pub fn list_events_paged(
    c: &Connection,
    project_id: Option<&str>,
    type_filter: &[String],
    type_prefixes: &[String],
    from: Option<i64>,
    to: Option<i64>,
    desc: bool,
    limit: i64,
    offset: i64,
) -> (Vec<Event>, i64) {
    let mut where_parts: Vec<String> = Vec::new();
    let mut params_: Vec<String> = Vec::new();
    if let Some(p) = project_id {
        params_.push(p.into());
        where_parts.push(format!("project_id=?{}", params_.len()));
    }
    if !type_filter.is_empty() {
        for t in type_filter {
            params_.push(t.clone());
        }
        let idx: Vec<String> = (params_.len() - type_filter.len() + 1..=params_.len())
            .map(|i| format!("?{i}"))
            .collect();
        where_parts.push(format!("type IN ({})", idx.join(",")));
    }
    // 类型前缀组内部 OR、与其它条件 AND（如 project. OR agent.）
    let prefix_sql = {
        let mut parts: Vec<String> = Vec::new();
        for pre in type_prefixes {
            if pre.is_empty() { continue; }
            params_.push(format!("{pre}%"));
            parts.push(format!("type LIKE ?{}", params_.len()));
        }
        if parts.is_empty() { String::new() } else { format!("({})", parts.join(" OR ")) }
    };
    if !prefix_sql.is_empty() {
        where_parts.push(prefix_sql);
    }
    if let Some(f) = from {
        params_.push(f.to_string());
        where_parts.push(format!("ts>=?{}", params_.len()));
    }
    if let Some(t) = to {
        params_.push(t.to_string());
        where_parts.push(format!("ts<=?{}", params_.len()));
    }
    let where_sql = if where_parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_parts.join(" AND "))
    };
    let order_sql = if desc { "DESC" } else { "ASC" };
    let total: i64 = {
        let sql = format!("SELECT count(*) FROM events{where_sql}");
        c.query_row(&sql, rusqlite::params_from_iter(&params_), |r| r.get(0)).unwrap_or(0)
    };
    let sql = format!(
        "SELECT * FROM events{where_sql} ORDER BY id {order_sql} LIMIT {limit} OFFSET {offset}"
    );
    let mut st = c.prepare(&sql).unwrap();
    let iter = st.query_map(rusqlite::params_from_iter(&params_), event_row).unwrap();
    (iter.filter_map(|x| x.ok()).collect(), total)
}
pub fn task_logs(c: &Connection, task_id: &str) -> Vec<LogLine> {
    let mut st = c
        .prepare("SELECT id, task_id, ts, line FROM logs WHERE task_id=?1 ORDER BY id")
        .unwrap();
    st.query_map(params![task_id], |r| {
        Ok(LogLine { id: r.get(0)?, task_id: r.get(1)?, ts: r.get(2)?, line: r.get(3)? })
    })
    .unwrap()
    .flatten()
    .collect()
}
pub fn list_guidance(c: &Connection, task_id: &str) -> Vec<Guidance> {
    let mut st = c.prepare("SELECT id, task_id, text, created_at FROM guidance WHERE task_id=?1 ORDER BY id").unwrap();
    st.query_map(params![task_id], |r| {
        Ok(Guidance { id: r.get(0)?, task_id: r.get(1)?, text: r.get(2)?, created_at: r.get(3)? })
    })
    .unwrap()
    .flatten()
    .collect()
}
pub fn latest_guidance(c: &Connection, task_id: &str) -> Option<String> {
    c.query_row(
        "SELECT text FROM guidance WHERE task_id=?1 ORDER BY id DESC LIMIT 1",
        params![task_id],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}
pub fn inbox_open_count(c: &Connection, project_id: &str) -> i64 {
    c.query_row(
        "SELECT COUNT(*) FROM inbox WHERE project_id=?1 AND status='open'",
        params![project_id],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// 单条事件（知识条目来源回链 / 校验用）。
pub fn get_event(c: &Connection, id: i64) -> Option<Event> {
    c.query_row("SELECT * FROM events WHERE id=?1", params![id], event_row)
        .optional()
        .ok()
        .flatten()
}

// ---------- knowledge (M5 项目知识库) ----------

fn knowledge_row(r: &Row) -> rusqlite::Result<KnowledgeEntry> {
    Ok(KnowledgeEntry {
        id: r.get(0)?,
        project_id: r.get(1)?,
        scope: r.get(2)?,
        body: r.get(3)?,
        applies_to: r.get(4)?,
        source_event_id: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

pub fn insert_knowledge(c: &Connection, k: &KnowledgeEntry) {
    c.execute(
        "INSERT INTO knowledge (id,project_id,scope,body,applies_to,source_event_id,created_at,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            k.id, k.project_id, k.scope, k.body, k.applies_to,
            k.source_event_id, k.created_at, k.updated_at
        ],
    )
    .expect("insert knowledge");
}

pub fn get_knowledge(c: &Connection, id: &str) -> Option<KnowledgeEntry> {
    c.query_row(
        "SELECT * FROM knowledge WHERE id=?1",
        params![id],
        knowledge_row,
    )
    .optional()
    .ok()
    .flatten()
}

/// 某项目可见的知识条目 = 本项目条目 + 全局条目（跨项目复用）。
pub fn list_knowledge(c: &Connection, project_id: &str) -> Vec<KnowledgeEntry> {
    let mut st = c
        .prepare(
            "SELECT * FROM knowledge WHERE project_id=?1 OR scope='global' ORDER BY created_at, id",
        )
        .unwrap();
    st.query_map(params![project_id], knowledge_row)
        .unwrap()
        .filter_map(|x| x.ok())
        .collect()
}

pub fn update_knowledge(
    c: &Connection,
    id: &str,
    body: &str,
    applies_to: Option<&str>,
    updated_at: i64,
) {
    let _ = c.execute(
        "UPDATE knowledge SET body=?2, applies_to=?3, updated_at=?4 WHERE id=?1",
        params![id, body, applies_to, updated_at],
    );
}

pub fn delete_knowledge(c: &Connection, id: &str) -> bool {
    c.execute("DELETE FROM knowledge WHERE id=?1", params![id]).unwrap_or(0) > 0
}

/// 提升为全局条目（跨项目复用）。
pub fn set_knowledge_scope(c: &Connection, id: &str, scope: &str, updated_at: i64) {
    let _ = c.execute(
        "UPDATE knowledge SET scope=?2, updated_at=?3 WHERE id=?1",
        params![id, scope, updated_at],
    );
}

// ---------- reports (M3 项目总结报告) ----------

/// 某项目的总结报告（每项目至多一份）。
pub fn get_report(c: &Connection, project_id: &str) -> Option<Report> {
    c.query_row("SELECT * FROM reports WHERE project_id=?1", params![project_id], report_row)
        .optional()
        .ok()
        .flatten()
}

/// Upsert by `project_id`（报告 id 每项目稳定）。报告内容/事实整行覆盖写。
pub fn save_report(c: &Connection, rep: &Report) {
    c.execute(
        "INSERT INTO reports (id,project_id,status,content,narrative,error,path,facts,attempts,created_at,updated_at,generated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(project_id) DO UPDATE SET
            status=excluded.status, content=excluded.content, narrative=excluded.narrative,
            error=excluded.error, path=excluded.path, facts=excluded.facts,
            attempts=excluded.attempts, updated_at=excluded.updated_at,
            generated_at=excluded.generated_at",
        params![
            rep.id, rep.project_id, rep.status, rep.content, rep.narrative, rep.error,
            rep.path, jstring(&rep.facts), rep.attempts, rep.created_at, rep.updated_at,
            rep.generated_at
        ],
    )
    .expect("save report");
}

pub fn get_decision(c: &Connection, id: &str) -> Option<Decision> {
    c.query_row("SELECT * FROM decisions WHERE id=?1", params![id], decision_row)
        .optional()
        .ok()
        .flatten()
}
pub fn list_decisions(c: &Connection, project_id: Option<&str>) -> Vec<Decision> {
    match project_id {
        Some(p) => {
            let mut st = c
                .prepare("SELECT * FROM decisions WHERE project_id=?1 ORDER BY created_at, id")
                .unwrap();
            st.query_map(params![p], decision_row)
                .unwrap()
                .filter_map(|x| x.ok())
                .collect()
        }
        None => {
            let mut st = c.prepare("SELECT * FROM decisions ORDER BY created_at, id").unwrap();
            st.query_map([], decision_row).unwrap().filter_map(|x| x.ok()).collect()
        }
    }
}
/// 某任务某类裁定请求（取最近一条，用于闸门判断与幂等保护）
pub fn task_decision(c: &Connection, task_id: &str, kind: &str) -> Option<Decision> {
    c.query_row(
        "SELECT * FROM decisions WHERE task_id=?1 AND kind=?2 ORDER BY created_at DESC, id DESC LIMIT 1",
        params![task_id, kind],
        decision_row,
    )
    .optional()
    .ok()
    .flatten()
}
/// 决策者实例列表：全局 agent 池中持有 decision_maker 角色的实例
pub fn list_decision_makers(c: &Connection) -> Vec<Agent> {
    list_agents(c)
        .into_iter()
        .filter(|a| a.roles.iter().any(|r| r == "decision_maker"))
        .collect()
}

/// 决策者实例解析：全局 agent 池中持有 decision_maker 角色的第一个实例
#[allow(dead_code)]
pub fn find_decision_maker(c: &Connection) -> Option<Agent> {
    list_decision_makers(c).into_iter().next()
}

// ---------- write ----------

pub fn jstring(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

// ---------- milestones (M3) ----------

pub fn insert_milestone(c: &Connection, m: &Milestone) {
    c.execute(
        "INSERT INTO milestones (id,project_id,name,goal,effect,task_ids,status) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            m.id, m.project_id, m.name, m.goal, m.effect,
            jstring(&Value::Array(m.task_ids.iter().map(|t| Value::String(t.clone())).collect())),
            m.status
        ],
    )
    .expect("insert milestone");
}

pub fn update_milestone(c: &Connection, m: &Milestone) {
    let _ = c.execute(
        "UPDATE milestones SET name=?2, goal=?3, effect=?4, task_ids=?5 WHERE id=?1",
        params![
            m.id, m.name, m.goal, m.effect,
            jstring(&Value::Array(m.task_ids.iter().map(|t| Value::String(t.clone())).collect()))
        ],
    );
}

pub fn set_milestone_status(c: &Connection, id: &str, status: &str) {
    let _ = c.execute("UPDATE milestones SET status=?2 WHERE id=?1", params![id, status]);
}

/// Point a task at a milestone (or clear it) — the association source of truth.
pub fn set_task_milestone(c: &Connection, task_id: &str, milestone_id: Option<&str>) {
    let _ = c.execute("UPDATE tasks SET milestone_id=?2 WHERE id=?1", params![task_id, milestone_id]);
}

/// M4：写入任务计划（执行者产出）。
pub fn set_task_plan(c: &Connection, task_id: &str, plan: Option<&str>) {
    let _ = c.execute("UPDATE tasks SET execution_plan=?2 WHERE id=?1", params![task_id, plan]);
}

/// M4：计划审查打回计数。
pub fn set_plan_rejection_count(c: &Connection, task_id: &str, n: i64) {
    let _ = c.execute("UPDATE tasks SET plan_rejection_count=?2 WHERE id=?1", params![task_id, n]);
}

/// M3 协商：更新协商状态 / 已完成轮次（可选写入验收标准）。
#[allow(dead_code)] // M4 协商退场
pub fn set_task_negotiation(
    c: &Connection,
    task_id: &str,
    status: &str,
    round: i64,
    criteria: Option<&str>,
) {
    match criteria {
        Some(v) => {
            let _ = c.execute(
                "UPDATE tasks SET negotiation_status=?2, negotiation_round=?3, acceptance_criteria=?4 WHERE id=?1",
                params![task_id, status, round, v],
            );
        }
        None => {
            let _ = c.execute(
                "UPDATE tasks SET negotiation_status=?2, negotiation_round=?3 WHERE id=?1",
                params![task_id, status, round],
            );
        }
    }
}

/// Recompute each milestone's stored task_ids from tasks.milestone_id. Keeping
/// the derived list persisted makes the model self-describing and the migration
/// idempotent (calling it twice is a no-op).
pub fn sync_milestone_task_ids(c: &Connection, project_id: &str) {
    let tasks = list_tasks(c, project_id);
    for m in list_milestones(c, project_id) {
        let ids: Vec<String> = tasks
            .iter()
            .filter(|t| t.milestone_id.as_deref() == Some(m.id.as_str()))
            .map(|t| t.id.clone())
            .collect();
        if ids != m.task_ids {
            let _ = c.execute(
                "UPDATE milestones SET task_ids=?2 WHERE id=?1",
                params![m.id, jstring(&Value::Array(ids.into_iter().map(Value::String).collect()))],
            );
        }
    }
}

/// M2: persist the project workspace spec (None clears it).
#[allow(dead_code)]
pub fn set_project_workspace(c: &Connection, id: &str, ws: Option<&Value>) {
    let s = ws.map(|v| v.to_string());
    let _ = c.execute("UPDATE projects SET workspace=?2 WHERE id=?1", params![id, s]);
}

/// Effective agent working directory for a project (DESIGN §13):
/// local → the configured absolute path; git → data/workspaces/<project_id>.
pub fn project_workdir(c: &Connection, p: &Project) -> Option<std::path::PathBuf> {
    // 工作区属于工程：项目只声明基线。项目不再持有自己的工作区。
    if let Some(eid) = p.engineering_id.as_deref() {
        if let Some(e) = get_engineering(c, eid) {
            if let Some(path) = e.workspace_path {
                return Some(std::path::PathBuf::from(path));
            }
            let root = std::env::var("DOMO_WORKSPACE_ROOT").unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                format!("{home}/.domo/workspace")
            });
            return Some(std::path::PathBuf::from(root).join(&e.id));
        }
    }
    // 兼容：未归属工程的项目仍可用自带工作区（新库不会有此类数据）
    let w = p.workspace.as_ref()?;
    match w.get("kind").and_then(|k| k.as_str())? {
        "local" => Some(std::path::PathBuf::from(w.get("path")?.as_str()?)),
        "git" => Some(std::path::PathBuf::from(format!("data/workspaces/{}", p.id))),
        _ => None,
    }
}

#[allow(dead_code)]
pub fn insert_agent(c: &Connection, a: &Agent) {
    c.execute(
        "INSERT INTO agents (id,name,adapter,roles,config,capabilities,status,current_task_ids) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            a.id, a.name, a.adapter,
            serde_json::to_string(&a.roles).unwrap(),
            serde_json::to_string(&a.config).unwrap(),
            serde_json::to_string(&a.capabilities).unwrap(),
            a.status,
            serde_json::to_string(&a.current_task_ids).unwrap()
        ],
    ).unwrap();
}

pub fn insert_decision(c: &Connection, d: &Decision) {
    c.execute(
        "INSERT INTO decisions (id, project_id, task_id, kind, request, scope, status, ruling, attempt, created_at, resolved_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            d.id, d.project_id, d.task_id, d.kind,
            d.request.to_string(), d.scope, d.status,
            d.ruling.as_ref().map(|v| v.to_string()),
            d.attempt, d.created_at, d.resolved_at
        ],
    )
    .expect("insert decision");
}

pub fn occupancy_add(c: &Connection, agent_id: &str, task_id: &str) {
    if let Some(mut a) = get_agent(c, agent_id) {
        if !a.current_task_ids.iter().any(|t| t == task_id) {
            a.current_task_ids.push(task_id.to_string());
            a.status = "busy".into();
            let _ = c.execute(
                "UPDATE agents SET current_task_ids=?2, status=?3 WHERE id=?1",
                params![agent_id, serde_json::to_string(&a.current_task_ids).unwrap(), "busy"],
            );
        }
    }
}

pub fn occupancy_remove(c: &Connection, agent_id: &str, task_id: &str) {
    if let Some(mut a) = get_agent(c, agent_id) {
        a.current_task_ids.retain(|t| t != task_id);
        let status = if a.current_task_ids.is_empty() { "idle" } else { "busy" };
        let _ = c.execute(
            "UPDATE agents SET current_task_ids=?2, status=?3 WHERE id=?1",
            params![agent_id, serde_json::to_string(&a.current_task_ids).unwrap(), status],
        );
    }
}

#[allow(dead_code)]
pub fn occupancy_clear_task(c: &Connection, agent_id: &str, task_id: &str) {
    occupancy_remove(c, agent_id, task_id)
}

pub fn agent_free(c: &Connection, agent_id: &str) -> bool {
    match get_agent(c, agent_id) {
        Some(a) => {
            let cap = a.capabilities.max_parallel.max(1);
            (a.current_task_ids.len() as i64) < cap
        }
        None => false,
    }
}

// ---------- M4 三档恢复（DESIGN §10）：恢复状态 + 真实会话 id 持久化 ----------

/// 任务的三档恢复状态（tasks.recovery_* 列）。
#[derive(Debug, Clone, Default)]
pub struct RecoveryState {
    /// 1=待走三档恢复（启动标记 / 档位切换期间保持）
    pub pending: bool,
    /// 下一个要尝试的档位：native_resume（默认）| handoff
    pub tier: String,
    /// 1=第三档人工重建挂起（suspended），下次重启重新进一轮三档
    pub manual: bool,
}

pub fn recovery_state(c: &Connection, task_id: &str) -> RecoveryState {
    c.query_row(
        "SELECT recovery_pending, recovery_tier, recovery_manual FROM tasks WHERE id=?1",
        params![task_id],
        |r| {
            Ok(RecoveryState {
                pending: r.get::<_, i64>(0)? != 0,
                tier: r.get::<_, String>(1).unwrap_or_default(),
                manual: r.get::<_, i64>(2)? != 0,
            })
        },
    )
    .unwrap_or_default()
}

/// 更新恢复状态（任一字段为 None 表示不改）。
pub fn recovery_set(
    c: &Connection,
    task_id: &str,
    pending: Option<bool>,
    tier: Option<&str>,
    manual: Option<bool>,
) {
    let mut sets: Vec<String> = Vec::new();
    if let Some(p) = pending {
        sets.push(format!("recovery_pending={}", p as i64));
    }
    if let Some(t) = tier {
        sets.push(format!("recovery_tier='{}'", t.replace('\'', "\'\'")));
    }
    if let Some(m) = manual {
        sets.push(format!("recovery_manual={}", m as i64));
    }
    if sets.is_empty() {
        return;
    }
    let sql = format!("UPDATE tasks SET {} WHERE id=?1", sets.join(", "));
    let _ = c.execute(&sql, params![task_id]);
}

/// 逻辑会话 → 真实 opencode 会话 id 持久化（同一 logical 后写覆盖，保留最新一次）。
pub fn session_put(c: &Connection, logical: &str, real: &str, task_id: Option<&str>, phase: Option<&str>, attempt: Option<i64>) {
    let _ = c.execute(
        "INSERT INTO session_map (logical, real, task_id, phase, attempt, updated_at) VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(logical) DO UPDATE SET real=excluded.real, task_id=excluded.task_id,
            phase=excluded.phase, attempt=excluded.attempt, updated_at=excluded.updated_at",
        params![logical, real, task_id, phase, attempt, now()],
    );
}

/// 逻辑会话对应的真实 opencode 会话 id（内存优先由调用方处理，这里是跨重启的事实源）。
#[allow(dead_code)]
pub fn session_get(c: &Connection, logical: &str) -> Option<String> {
    c.query_row(
        "SELECT real FROM session_map WHERE logical=?1",
        params![logical],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// 某任务某次执行尝试最近一次使用的真实会话 id（三档恢复·第一档的 resume 候选）。
/// handoff 轮次也会写入同一 (task, attempt)，因此取 updated_at 最新的一行。
pub fn exec_resume_session(c: &Connection, task_id: &str, attempt: i64) -> Option<String> {
    c.query_row(
        "SELECT real FROM session_map WHERE task_id=?1 AND phase='execute' AND attempt=?2
         ORDER BY updated_at DESC, rowid DESC LIMIT 1",
        params![task_id, attempt],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Cycle check: would `task_id` (existing task being edited, or a brand-new task
/// id) depending on `new_deps` introduce a cycle?
///
/// The edge direction is "dependency must complete before dependent":
/// `x -> y` means `y.depends_on` contains `x`. Adding `task_id` edges
/// `new_deps -> task_id` creates a cycle iff `task_id` is reachable from one of
/// `new_deps` by following each node's own `depends_on` list (i.e. `task_id` is
/// already a (transitive) dependency of a new dependency). `task_id`'s own
/// outgoing edges are replaced by `new_deps` for the check.
pub fn deps_introduce_cycle(c: &Connection, project_id: &str, task_id: Option<&str>, new_deps: &[String]) -> bool {
    let tasks = list_tasks(c, project_id);
    let mut deps_map: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for t in &tasks {
        if task_id == Some(t.id.as_str()) {
            continue; // superseded by new_deps below
        }
        deps_map.insert(t.id.clone(), t.depends_on.clone());
    }
    if let Some(id) = task_id {
        deps_map.insert(id.to_string(), new_deps.to_vec());
    }
    let mut seen: std::collections::HashSet<String> = Default::default();
    let mut stack: Vec<String> = new_deps.to_vec();
    while let Some(n) = stack.pop() {
        if task_id == Some(n.as_str()) {
            return true;
        }
        if !seen.insert(n.clone()) {
            continue;
        }
        if let Some(next) = deps_map.get(&n) {
            stack.extend(next.clone());
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_has_column(c: &Connection, table: &str, column: &str) -> bool {
        let sql = format!("PRAGMA table_info({table})");
        let mut st = c.prepare(&sql).unwrap();
        let rows: Vec<String> = st
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|x| x.ok())
            .collect();
        rows.iter().any(|n| n == column)
    }

    fn seed_project(c: &Connection, id: &str) {
        c.execute(
            "INSERT INTO projects (id,name,brief,status,plan_content,confirmed_at,created_at) VALUES (?1,'p','',?2,NULL,NULL,1)",
            params![id, "running"],
        ).unwrap();
    }

    #[test]
    fn engineering_crud_and_project_link() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        let e = Engineering {
            id: "eng_1".into(),
            name: "演示工程".into(),
            brief: "b".into(),
            workspace_kind: "multi".into(),
            workspace_path: Some("/tmp".into()),
            default_agent_pool: Some(vec!["a1".into()]),
            constraints: Some("禁改 vendor".into()),
            created_at: 1,
        };
        insert_engineering(&c, &e);
        assert_eq!(list_engineerings(&c).len(), 1);
        let mut got = get_engineering(&c, "eng_1").unwrap();
        assert_eq!(got.name, "演示工程");
        assert_eq!(got.default_agent_pool.as_deref(), Some(&["a1".to_string()][..]));

        seed_project(&c, "prj_link");
        set_project_engineering(&c, "prj_link", Some("eng_1"));
        assert_eq!(get_project(&c, "prj_link").unwrap().engineering_id.as_deref(), Some("eng_1"));
        assert_eq!(list_projects_of(&c, "eng_1").len(), 1);

        got.brief = "b2".into();
        update_engineering(&c, &got);
        assert_eq!(get_engineering(&c, "eng_1").unwrap().brief, "b2");

        set_project_engineering(&c, "prj_link", None);
        assert!(get_project(&c, "prj_link").unwrap().engineering_id.is_none());
        assert!(list_projects_of(&c, "eng_1").is_empty());
    }

    #[test]
    fn migrate_is_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        migrate(&c); // a second boot must not error
        migrate(&c);
        assert!(table_has_column(&c, "tasks", "milestone_id"));
        assert!(table_has_column(&c, "tasks", "version"));
        assert!(table_has_column(&c, "projects", "workspace"));
        assert!(table_has_column(&c, "projects", "baseline"));
    }

    #[test]
    fn cas_dispatch_allows_only_one_claimer() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        seed_project(&c, "prj_cas");
        c.execute(
            "INSERT INTO tasks (id,project_id,title,status,attempt,version,created_at)
             VALUES ('t1','prj_cas','x','ready',0,0,1)",
            [],
        ).unwrap();

        assert!(cas_task_dispatch(&c, "t1", 0, &["ready"], "executing", 1));
        let t = get_task(&c, "t1").unwrap();
        assert_eq!(t.status, "executing");
        assert_eq!(t.attempt, 1);
        assert_eq!(t.version, 1);

        // 旧 version / 旧状态 → 0 行，第二次派发必须放弃。
        assert!(!cas_task_dispatch(&c, "t1", 0, &["ready"], "executing", 2));
        assert!(!cas_task_dispatch(&c, "t1", 1, &["ready"], "executing", 2));
        assert_eq!(get_task(&c, "t1").unwrap().attempt, 1);
    }

    #[test]
    fn startup_reconciliation_dedupes_open_inbox() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        for (id, reason) in [
            ("inb_1", "failure_limit"),
            ("inb_2", "failure_limit"),
            ("inb_3", "rejection_limit"),
        ] {
            c.execute(
                "INSERT INTO inbox (id,project_id,\"type\",payload,status,created_at)
                 VALUES (?1,'prj_x','escalation',?2,'open',1)",
                params![id, serde_json::json!({"task_id": "t1", "reason": reason}).to_string()],
            ).unwrap();
        }

        assert_eq!(dedupe_open_inbox(&c), 1);
        assert_eq!(dedupe_open_inbox(&c), 0); // 幂等
        let open: Vec<_> = list_inbox(&c).into_iter().filter(|i| i.status == "open").collect();
        assert_eq!(open.len(), 2);
        assert!(open.iter().any(|i| i.payload["reason"] == "failure_limit"));
        assert!(open.iter().any(|i| i.payload["reason"] == "rejection_limit"));
    }

    #[test]
    fn project_baseline_roundtrip() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        seed_project(&c, "prj_base");
        assert!(get_project(&c, "prj_base").unwrap().baseline.is_none());

        let b = serde_json::json!({"repos": {"api": "domo/iter_base", "web": "domo/iter_base"}});
        set_project_baseline(&c, "prj_base", Some(&b));
        assert_eq!(get_project(&c, "prj_base").unwrap().baseline, Some(b));

        set_project_baseline(&c, "prj_base", None);
        assert!(get_project(&c, "prj_base").unwrap().baseline.is_none());
    }

    #[test]
    fn open_sets_concurrency_pragmas() {
        let path = std::env::temp_dir().join(format!("domo-pragma-{}-{}.db", std::process::id(), now()));
        let p = path.to_string_lossy().to_string();
        let c = open(&p);
        let mode: String = c.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        let busy: i64 = c.query_row("PRAGMA busy_timeout", [], |r| r.get(0)).unwrap();
        let fk: i64 = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        drop(c);
        assert_eq!(mode.to_lowercase(), "wal");
        assert_eq!(busy, 5000);
        assert_eq!(fk, 1);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{p}{suffix}"));
        }
    }

    #[test]
    fn legacy_schema_gains_milestone_column() {
        let c = Connection::open_in_memory().unwrap();
        // pre-M3 tasks table without milestone_id
        c.execute_batch(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, title TEXT NOT NULL, created_at INTEGER NOT NULL);",
        ).unwrap();
        migrate(&c);
        assert!(table_has_column(&c, "tasks", "milestone_id"));
        migrate(&c); // idempotent repeat
        // milestones table exists and is queryable
        assert!(list_milestones(&c, "prj_x").is_empty());
    }

    #[test]
    fn negotiation_round_column_and_persistence() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        migrate(&c); // idempotent: negotiation_round column add must not error twice
        assert!(table_has_column(&c, "tasks", "negotiation_round"));
        seed_project(&c, "prj1");
        c.execute(
            "INSERT INTO tasks (id,project_id,title,acceptance_required,acceptance_criteria,negotiation_status,negotiation_round,status,created_at)
             VALUES ('t1','prj1','a',1,'draft criteria','pending',0,'planned',1)",
            [],
        ).unwrap();
        let t = get_task(&c, "t1").unwrap();
        assert_eq!(t.negotiation_status, "pending");
        assert_eq!(t.negotiation_round, 0);

        // rounds/status survive a "restart" (state lives in the DB, not memory)
        set_task_negotiation(&c, "t1", "pending", 2, None);
        set_task_negotiation(&c, "t1", "agreed", 2, Some("agreed criteria"));
        let t = get_task(&c, "t1").unwrap();
        assert_eq!(t.negotiation_status, "agreed");
        assert_eq!(t.negotiation_round, 2);
        assert_eq!(t.acceptance_criteria.as_deref(), Some("agreed criteria"));
    }

    #[test]
    fn milestone_roundtrip_and_task_sync() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        seed_project(&c, "prj1");
        c.execute_batch(
            "INSERT INTO tasks (id,project_id,milestone_id,title,created_at) VALUES ('t1','prj1','mst1','a',1);
             INSERT INTO tasks (id,project_id,milestone_id,title,created_at) VALUES ('t2','prj1',NULL,'b',2);
             INSERT INTO tasks (id,project_id,milestone_id,title,created_at) VALUES ('t3','prj1','mst1','c',3);",
        ).unwrap();
        let m = Milestone {
            id: "mst1".into(),
            project_id: "prj1".into(),
            name: "M1".into(),
            goal: Some("g".into()),
            effect: Some("e".into()),
            task_ids: vec![],
            status: "pending".into(),
        };
        insert_milestone(&c, &m);

        sync_milestone_task_ids(&c, "prj1");
        let got = get_milestone(&c, "mst1").unwrap();
        assert_eq!(got.task_ids, vec!["t1".to_string(), "t3".to_string()]);
        assert_eq!(got.goal.as_deref(), Some("g"));

        // sync is idempotent
        sync_milestone_task_ids(&c, "prj1");
        assert_eq!(get_milestone(&c, "mst1").unwrap().task_ids.len(), 2);

        // membership is derived from tasks.milestone_id
        let members = tasks_of_milestone(&c, "prj1", "mst1");
        assert_eq!(members.len(), 2);

        set_task_milestone(&c, "t2", Some("mst1"));
        sync_milestone_task_ids(&c, "prj1");
        assert_eq!(get_milestone(&c, "mst1").unwrap().task_ids.len(), 3);
    }

    #[test]
    fn open_gate_item_finds_only_open_milestone_gate() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        seed_project(&c, "prj1");
        c.execute(
            "INSERT INTO inbox (id,project_id,\"type\",payload,status,resolution,created_at) VALUES ('inb1','prj1','milestone_gate','{\"milestone_id\":\"mst1\"}','open',NULL,1)",
            [],
        ).unwrap();
        assert!(open_gate_item(&c, "mst1").is_some());
        c.execute("UPDATE inbox SET status='resolved' WHERE id='inb1'", []).unwrap();
        assert!(open_gate_item(&c, "mst1").is_none());
    }

    fn seed_tasks(c: &Connection, rows: &[(&str, &[&str])]) {
        for (id, deps) in rows {
            let deps_json = serde_json::to_string(&deps.to_vec()).unwrap();
            c.execute(
                "INSERT INTO tasks (id,project_id,title,depends_on,created_at) VALUES (?1,'prj1',?1,?2,1)",
                params![id, deps_json],
            ).unwrap();
        }
    }

    #[test]
    fn cycle_check_detects_direct_two_cycle() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        seed_project(&c, "prj1");
        seed_tasks(&c, &[("a", &[]), ("b", &["a"])]);
        // b depends on a already; making a depend on b closes a 2-cycle
        assert!(deps_introduce_cycle(&c, "prj1", Some("a"), &["b".into()]));
        // the same-batch variant (b created in this batch, a edited to depend on b)
        assert!(deps_introduce_cycle(&c, "prj1", Some("a"), &["c".into(), "b".into()]));
        // a sensible edge (a depends on nothing new) is not a cycle
        assert!(!deps_introduce_cycle(&c, "prj1", Some("b"), &[]));
    }

    #[test]
    fn cycle_check_detects_transitive_cycle() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        seed_project(&c, "prj1");
        seed_tasks(&c, &[("a", &[]), ("b", &["a"]), ("c", &["b"])]);
        // c -> b -> a; making a depend on c closes the loop
        assert!(deps_introduce_cycle(&c, "prj1", Some("a"), &["c".into()]));
        // a brand-new task id can never be depended on yet -> no cycle
        assert!(!deps_introduce_cycle(&c, "prj1", Some("newid"), &["a".into(), "c".into()]));
        assert!(!deps_introduce_cycle(&c, "prj1", None, &["a".into()]));
    }

    #[test]
    fn open_window_item_matches_only_milestone_window_replan() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        seed_project(&c, "prj1");
        c.execute(
            "INSERT INTO inbox (id,project_id,\"type\",payload,status,resolution,created_at) VALUES
             ('inb_gate','prj1','milestone_gate','{\"milestone_id\":\"mst1\"}','open',NULL,1),
             ('inb_plain','prj1','replan','{\"detail\":\"x\"}','open',NULL,2),
             ('inb_win','prj1','replan','{\"kind\":\"milestone_window\",\"milestone_id\":\"mst1\"}','open',NULL,3)",
            [],
        ).unwrap();
        assert_eq!(open_window_item(&c, "mst1").map(|i| i.id), Some("inb_win".into()));
        assert_eq!(open_window_items(&c, "prj1").len(), 1);
        assert!(open_window_item(&c, "mst2").is_none());

        // has_window_item: true for open *and* resolved windows, false otherwise.
        assert!(has_window_item(&c, "mst1"));
        assert!(!has_window_item(&c, "mst2"));
        c.execute("UPDATE inbox SET status='resolved' WHERE id='inb_win'", []).unwrap();
        assert!(open_window_item(&c, "mst1").is_none(), "resolved window is no longer open");
        assert!(has_window_item(&c, "mst1"), "resolved window still counts as worked");
    }

    fn kn(id: &str, pid: &str, scope: &str, body: &str, applies: Option<&str>, ev: i64) -> KnowledgeEntry {
        KnowledgeEntry {
            id: id.into(), project_id: pid.into(), scope: scope.into(),
            body: body.into(), applies_to: applies.map(String::from),
            source_event_id: ev, created_at: 100, updated_at: 100,
        }
    }

    #[test]
    fn knowledge_crud_roundtrip_fields() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        migrate(&c); // idempotent: knowledge table create must not error twice
        seed_project(&c, "prj1");
        c.execute(
            "INSERT INTO events (id,ts,project_id,task_id,\"type\",payload) VALUES (7,1,'prj1',NULL,'task.guidance_added','{}')",
            [],
        ).unwrap();

        let k = kn("kn1", "prj1", "project", "所有 PR 必须带测试", Some("全部任务"), 7);
        insert_knowledge(&c, &k);

        // 字段逐项一致
        let got = get_knowledge(&c, "kn1").unwrap();
        assert_eq!(got, k);

        // update：正文/适用范围/updated_at 变化，其余不变
        update_knowledge(&c, "kn1", "所有 PR 必须带测试与截图", None, 200);
        let got = get_knowledge(&c, "kn1").unwrap();
        assert_eq!(got.body, "所有 PR 必须带测试与截图");
        assert_eq!(got.applies_to, None);
        assert_eq!(got.updated_at, 200);
        assert_eq!(got.source_event_id, 7);
        assert_eq!(got.created_at, 100);

        // delete 后消失，其它条目无损
        insert_knowledge(&c, &kn("kn2", "prj1", "project", "第二条", None, 7));
        assert!(delete_knowledge(&c, "kn1"));
        assert!(get_knowledge(&c, "kn1").is_none());
        assert!(get_knowledge(&c, "kn2").is_some());
        assert!(!delete_knowledge(&c, "kn1"), "second delete is a no-op");
    }

    #[test]
    fn knowledge_visibility_project_vs_global() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        seed_project(&c, "prjA");
        seed_project(&c, "prjB");
        insert_knowledge(&c, &kn("ka", "prjA", "project", "A 的项目规范", None, 1));
        insert_knowledge(&c, &kn("kg", "prjA", "global", "全局规范", None, 1));

        // A 可见：自己的项目条目 + 全局条目
        let a: Vec<String> = list_knowledge(&c, "prjA").into_iter().map(|e| e.id).collect();
        assert_eq!(a, vec!["ka".to_string(), "kg".to_string()]);
        // B 只可见全局条目，不可见 A 的项目条目
        let b: Vec<String> = list_knowledge(&c, "prjB").into_iter().map(|e| e.id).collect();
        assert_eq!(b, vec!["kg".to_string()]);
    }

    #[test]
    fn knowledge_promote_scope_makes_it_global() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c);
        seed_project(&c, "prjA");
        seed_project(&c, "prjB");
        insert_knowledge(&c, &kn("ka", "prjA", "project", "规范", None, 1));
        assert!(list_knowledge(&c, "prjB").is_empty());

        set_knowledge_scope(&c, "ka", "global", 300);
        let got = get_knowledge(&c, "ka").unwrap();
        assert_eq!(got.scope, "global");
        assert_eq!(got.updated_at, 300);
        // 提升后 B 可见并可引用
        assert_eq!(list_knowledge(&c, "prjB").len(), 1);
    }
}

/// M2.5: 设置项目 agent 池（None=全部 agent）。
pub fn set_project_agent_pool(c: &Connection, id: &str, pool: Option<&[String]>) {
    let s = pool.map(|p| serde_json::to_string(p).unwrap_or_default());
    let _ = c.execute("UPDATE projects SET agent_pool=?2 WHERE id=?1", params![id, s]);
}

/// M4：写入项目工作区登记的仓库列表。
pub fn set_project_repos(c: &Connection, id: &str, repos: &[crate::models::RepoRef]) {
    let _ = c.execute(
        "UPDATE projects SET repos=?2 WHERE id=?1",
        params![id, serde_json::to_string(repos).unwrap_or_else(|_| "[]".into())],
    );
}

/// 工程是否为多仓目录（不依赖项目仓库数量，单仓工程不受影响）。
pub fn project_is_multi(c: &Connection, p: &Project) -> bool {
    p.engineering_id
        .as_deref()
        .and_then(|eid| get_engineering(c, eid))
        .map(|e| e.workspace_kind == "multi")
        .unwrap_or(false)
}

/// ARCH §15：写入项目基线（None 清除）。
pub fn set_project_baseline(c: &Connection, id: &str, baseline: Option<&Value>) {
    let s = baseline.map(|v| v.to_string());
    let _ = c.execute("UPDATE projects SET baseline=?2 WHERE id=?1", params![id, s]);
}

/// M4：读取项目登记的仓库列表。
pub fn project_repos(c: &Connection, id: &str) -> Vec<crate::models::RepoRef> {
    c.query_row("SELECT repos FROM projects WHERE id=?1", params![id], |r| r.get::<_, Option<String>>(0))
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// M4：任务仓库 scope。
pub fn set_task_scope_repos(c: &Connection, task_id: &str, scope: &[String]) {
    let _ = c.execute(
        "UPDATE tasks SET scope_repos=?2 WHERE id=?1",
        params![task_id, serde_json::to_string(scope).unwrap_or_else(|_| "[]".into())],
    );
}

/// M4.5：项目 agent 池（角色 → 按优先级升序的 agent id 列表）。
/// 兼容旧形态（扁平 id 数组）：展开为「这些 agent 各自具备的角色，按登记顺序编号」。
pub fn project_pool(c: &Connection, project_id: &str) -> std::collections::HashMap<String, Vec<String>> {
    let raw: Option<String> = c
        .query_row("SELECT agent_pool FROM projects WHERE id=?1", params![project_id], |r| r.get(0))
        .ok()
        .flatten();
    let mut out: std::collections::HashMap<String, Vec<String>> = Default::default();
    let Some(raw) = raw.filter(|s| !s.trim().is_empty()) else { return out };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else { return out };
    match v {
        // 旧形态：[agent_id...] → 按各 agent 的角色展开
        Value::Array(arr) => {
            for id in arr.iter().filter_map(|x| x.as_str()) {
                if let Some(a) = get_agent(c, id) {
                    for role in &a.roles {
                        out.entry(role.clone()).or_default().push(id.to_string());
                    }
                }
            }
        }
        // 新形态：{"executor":[{agent_id,priority}...]}
        Value::Object(map) => {
            for (role, list) in map {
                let mut items: Vec<(i64, String)> = list
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|it| {
                                let id = it.get("agent_id")?.as_str()?.to_string();
                                let pri = it.get("priority").and_then(|p| p.as_i64()).unwrap_or(0);
                                Some((pri, id))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                items.sort();
                out.insert(role.clone(), items.into_iter().map(|(_, id)| id).collect());
            }
        }
        _ => {}
    }
    out
}

/// M4.5：写入项目池（角色 → 有序 agent id）。
pub fn set_project_pool(c: &Connection, project_id: &str, pool: &std::collections::HashMap<String, Vec<String>>) {
    let mut map = serde_json::Map::new();
    for (role, ids) in pool {
        let arr: Vec<Value> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| serde_json::json!({"agent_id": id, "priority": i as i64}))
            .collect();
        map.insert(role.clone(), Value::Array(arr));
    }
    let _ = c.execute(
        "UPDATE projects SET agent_pool=?2 WHERE id=?1",
        params![project_id, Value::Object(map).to_string()],
    );
}

/// M4.5：任务已尝试过的 agent 记录（{role: [ids]}）。
pub fn task_tried(c: &Connection, task_id: &str) -> Value {
    c.query_row("SELECT tried_agents FROM tasks WHERE id=?1", params![task_id], |r| {
        r.get::<_, Option<String>>(0)
    })
    .ok()
    .flatten()
    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    .unwrap_or_else(|| serde_json::json!({}))
}

pub fn task_tried_add(c: &Connection, task_id: &str, role: &str, agent_id: &str) {
    let mut v = task_tried(c, task_id);
    let arr = v
        .as_object_mut()
        .map(|m| m.entry(role.to_string()).or_insert_with(|| serde_json::json!([])))
        .and_then(|a| a.as_array_mut());
    if let Some(arr) = arr {
        if !arr.iter().any(|x| x.as_str() == Some(agent_id)) {
            arr.push(Value::String(agent_id.to_string()));
        }
    }
    let _ = c.execute("UPDATE tasks SET tried_agents=?2 WHERE id=?1", params![task_id, v.to_string()]);
}

/// M4.5：清空已尝试记录（人类指定/恢复重派时重置）。
pub fn task_tried_clear(c: &Connection, task_id: &str) {
    let _ = c.execute("UPDATE tasks SET tried_agents='{}' WHERE id=?1", params![task_id]);
}

/// M4.5：人类固定/解绑任务的执行者或验收者。
pub fn set_task_agent(c: &Connection, task_id: &str, role: &str, agent_id: Option<&str>, pinned: bool) {
    let col = if role == "acceptor" { "acceptor_agent_id" } else { "executor_agent_id" };
    let _ = c.execute(
        &format!("UPDATE tasks SET {col}=?2, agent_pinned=?3, version=version+1 WHERE id=?1"),
        params![task_id, agent_id, pinned as i64],
    );
}

/// M5：归档一次 agent 运行的完整输出（模型输出已由 DOMO 获得 → 自行存档）。
pub struct AgentOutputRecord<'a> {
    pub task_id: Option<&'a str>,
    pub project_id: &'a str,
    pub phase: &'a str,
    pub attempt: i64,
    pub session: &'a str,
    pub real_session_id: Option<&'a str>,
    pub outcome: &'a str,
    pub text: &'a str,
    pub raw: &'a str,
}

pub fn insert_agent_output(c: &Connection, r: &AgentOutputRecord) {
    let _ = c.execute(
        "INSERT INTO agent_outputs (task_id,project_id,phase,attempt,session,real_session_id,ts,outcome,text,raw)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![r.task_id, r.project_id, r.phase, r.attempt, r.session, r.real_session_id,
                now(), r.outcome, r.text, r.raw],
    );
}

/// 某任务的归档输出（按时间正序；`limit` 为最近 N 条，0=全部）。
pub fn list_agent_outputs(c: &Connection, task_id: &str, limit: i64) -> Vec<Value> {
    let sql = if limit > 0 {
        format!("SELECT id,phase,attempt,session,real_session_id,ts,outcome,text,length(raw) raw_len FROM agent_outputs WHERE task_id=?1 ORDER BY id DESC LIMIT {limit}")
    } else {
        "SELECT id,phase,attempt,session,real_session_id,ts,outcome,text,length(raw) raw_len FROM agent_outputs WHERE task_id=?1 ORDER BY id".to_string()
    };
    let mut st = c.prepare(&sql).unwrap();
    let rows: Vec<Value> = st
        .query_map(params![task_id], |r| {
            Ok(serde_json::json!({
                "id": r.get::<_, i64>(0)?,
                "phase": r.get::<_, String>(1)?,
                "attempt": r.get::<_, i64>(2)?,
                "session": r.get::<_, String>(3)?,
                "real_session_id": r.get::<_, Option<String>>(4)?,
                "ts": r.get::<_, i64>(5)?,
                "outcome": r.get::<_, String>(6)?,
                "text": r.get::<_, String>(7)?,
                "raw_len": r.get::<_, i64>(8)?,
            }))
        })
        .unwrap()
        .filter_map(|x| x.ok())
        .collect();
    rows
}

/// 单条归档输出全文（含原始 stdout 流）。
pub fn get_agent_output(c: &Connection, id: i64) -> Option<Value> {
    c.query_row(
        "SELECT id,task_id,project_id,phase,attempt,session,real_session_id,ts,outcome,text,raw FROM agent_outputs WHERE id=?1",
        params![id],
        |r| Ok(serde_json::json!({
            "id": r.get::<_, i64>(0)?,
            "task_id": r.get::<_, Option<String>>(1)?,
            "project_id": r.get::<_, String>(2)?,
            "phase": r.get::<_, String>(3)?,
            "attempt": r.get::<_, i64>(4)?,
            "session": r.get::<_, String>(5)?,
            "real_session_id": r.get::<_, Option<String>>(6)?,
            "ts": r.get::<_, i64>(7)?,
            "outcome": r.get::<_, String>(8)?,
            "text": r.get::<_, String>(9)?,
            "raw": r.get::<_, String>(10)?,
        })),
    ).ok()
}

#[cfg(test)]
mod agent_output_tests {
    use super::*;

    #[test]
    fn agent_output_roundtrip() {
        let c = open(":memory:");
        insert_agent_output(&c, &AgentOutputRecord {
            task_id: Some("tsk_x"), project_id: "prj_x", phase: "accept", attempt: 2,
            session: "tsk_x_a2_accept", real_session_id: Some("ses_real"), outcome: "success",
            text: "核对完成\nVERDICT: pass", raw: "{\"type\":\"text\"}",
        });
        let list = list_agent_outputs(&c, "tsk_x", 10);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["phase"], "accept");
        assert_eq!(list[0]["attempt"], 2);
        assert!(list[0]["text"].as_str().unwrap().contains("VERDICT: pass"));
        let id = list[0]["id"].as_i64().unwrap();
        let full = get_agent_output(&c, id).unwrap();
        assert_eq!(full["real_session_id"], "ses_real");
        assert_eq!(full["raw"], "{\"type\":\"text\"}");
    }
}
