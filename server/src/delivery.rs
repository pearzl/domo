//! Versioned delivery inputs, project consumption gates and transactional audit.
//! Contract snapshots are immutable; task/job references never follow later plans.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::SqlitePool;

pub async fn initialize(pool: &SqlitePool) -> crate::db::DbResult<()> {
    sqlx::raw_sql(SCHEMA).execute(pool).await?;
    Ok(())
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS contracts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL REFERENCES projects(id),
    generation INTEGER NOT NULL,
    goal TEXT NOT NULL,
    proposal TEXT,
    source TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS replan_history (
    project_id TEXT NOT NULL REFERENCES projects(id),
    generation INTEGER NOT NULL, goal TEXT NOT NULL, previous_goal TEXT,
    previous_stage TEXT, previous_report TEXT, previous_proposal TEXT,
    draft TEXT, state TEXT NOT NULL, reason TEXT, created_at TEXT NOT NULL,
    PRIMARY KEY(project_id, generation)
);
CREATE TRIGGER IF NOT EXISTS replan_history_request AFTER UPDATE OF replan_generation ON projects
WHEN NEW.replan_generation > OLD.replan_generation BEGIN
    INSERT INTO replan_history(project_id, generation, goal, previous_goal, previous_stage,
        previous_report, previous_proposal, state, reason, created_at)
    VALUES(NEW.id, NEW.replan_generation, NEW.goal, OLD.goal, OLD.stage,
        OLD.report, OLD.proposal, 'analyzing', NEW.replan_reason, NEW.updated_at);
END;
CREATE TRIGGER IF NOT EXISTS replan_history_draft AFTER UPDATE OF replan_proposal ON projects
WHEN NEW.replan_active = 1 AND NEW.replan_proposal IS NOT NULL BEGIN
    UPDATE replan_history SET draft = NEW.replan_proposal, state = 'draft'
        WHERE project_id = NEW.id AND generation = NEW.replan_generation;
END;
CREATE TRIGGER IF NOT EXISTS replan_history_end AFTER UPDATE OF replan_active ON projects
WHEN NEW.replan_active = 0 AND OLD.replan_active = 1 BEGIN
    UPDATE replan_history SET state = CASE WHEN NEW.proposal IS NOT OLD.proposal
        OR NEW.replan_cleanup_needed = 1 THEN 'applied' ELSE 'abandoned' END
        WHERE project_id = NEW.id AND generation = OLD.replan_generation;
END;
CREATE INDEX IF NOT EXISTS idx_contracts_project ON contracts(project_id, id);
CREATE TRIGGER IF NOT EXISTS audit_contract_insert AFTER INSERT ON contracts BEGIN
    INSERT INTO events(project_id, kind, message, payload, created_at)
        VALUES(NEW.project_id, 'contract_recorded', '交付契约版本已记录',
            json_object('version', 1, 'contract_id', NEW.id, 'generation', NEW.generation,
                'source', NEW.source, 'goal', NEW.goal, 'proposal', json(NEW.proposal)), NEW.created_at);
END;
CREATE TABLE IF NOT EXISTS merge_operations (
    project_id TEXT PRIMARY KEY REFERENCES projects(id),
    base_before TEXT NOT NULL,
    project_before TEXT NOT NULL,
    prepared_commit TEXT,
    phase TEXT NOT NULL,
    error TEXT,
    started_at TEXT NOT NULL,
    finished_at TEXT
);
CREATE TABLE IF NOT EXISTS project_limits (
    project_id TEXT PRIMARY KEY REFERENCES projects(id),
    cost_budget REAL,
    task_attempt_limit INTEGER,
    replan_limit INTEGER,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS dispatches (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL REFERENCES jobs(id),
    project_id TEXT NOT NULL,
    task_id TEXT,
    role TEXT NOT NULL,
    agent_id TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_dispatches_task ON dispatches(task_id, role);
CREATE TABLE IF NOT EXISTS job_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL, project_id TEXT NOT NULL, agent_id TEXT, role TEXT NOT NULL,
    input_tokens INTEGER, output_tokens INTEGER, cache_read_tokens INTEGER,
    cache_write_tokens INTEGER, reasoning_tokens INTEGER, cost REAL, duration_ms INTEGER,
    usage_known INTEGER NOT NULL, started_at TEXT, finished_at TEXT, created_at TEXT NOT NULL,
    snapshot TEXT NOT NULL
);
CREATE VIEW IF NOT EXISTS accounting_jobs AS
    SELECT project_id, agent_id, role, input_tokens, output_tokens, cache_read_tokens,
        cache_write_tokens, reasoning_tokens, cost, duration_ms, usage_known,
        started_at, finished_at, created_at FROM jobs
    UNION ALL
    SELECT project_id, agent_id, role, input_tokens, output_tokens, cache_read_tokens,
        cache_write_tokens, reasoning_tokens, cost, duration_ms, usage_known,
        started_at, finished_at, created_at FROM job_runs;
CREATE TRIGGER IF NOT EXISTS preserve_job_retry BEFORE UPDATE OF status ON jobs
WHEN NEW.status = 'running' AND OLD.status = 'failed' BEGIN
    INSERT INTO job_runs(job_id, project_id, agent_id, role, input_tokens, output_tokens,
        cache_read_tokens, cache_write_tokens, reasoning_tokens, cost, duration_ms,
        usage_known, started_at, finished_at, created_at, snapshot)
    VALUES(OLD.id, OLD.project_id, OLD.agent_id, OLD.role, OLD.input_tokens, OLD.output_tokens,
        OLD.cache_read_tokens, OLD.cache_write_tokens, OLD.reasoning_tokens, OLD.cost,
        OLD.duration_ms, OLD.usage_known, OLD.started_at, OLD.finished_at, OLD.created_at,
        json_object('prompt', OLD.prompt, 'result', OLD.result, 'stdout', OLD.stdout,
            'stderr', OLD.stderr, 'error', OLD.error, 'contract_id', OLD.contract_id));
END;
CREATE TRIGGER IF NOT EXISTS contract_project_insert AFTER INSERT ON projects BEGIN
    INSERT INTO contracts(project_id, generation, goal, proposal, source, created_at)
        VALUES(NEW.id, NEW.replan_generation, NEW.goal, NEW.proposal, 'goal', NEW.created_at);
    UPDATE projects SET contract_id = last_insert_rowid() WHERE id = NEW.id;
END;
CREATE TRIGGER IF NOT EXISTS contract_plan_update AFTER UPDATE OF proposal ON projects
WHEN NEW.proposal IS NOT NULL AND NEW.confirmed_at IS NOT NULL
    AND NEW.stage IN ('executing', 'summarizing')
    AND (NEW.proposal IS NOT OLD.proposal OR NEW.goal IS NOT OLD.goal
        OR NEW.replan_active IS NOT OLD.replan_active OR OLD.confirmed_at IS NULL)
BEGIN
    INSERT INTO contracts(project_id, generation, goal, proposal, source, created_at)
        VALUES(NEW.id, NEW.replan_generation, NEW.goal, NEW.proposal, 'plan', NEW.updated_at);
    UPDATE projects SET contract_id = last_insert_rowid() WHERE id = NEW.id;
    UPDATE tasks SET contract_id = (SELECT contract_id FROM projects WHERE id = NEW.id)
        WHERE project_id = NEW.id AND created_at = NEW.updated_at;
END;
CREATE TRIGGER IF NOT EXISTS contract_goal_update AFTER UPDATE OF goal ON projects
WHEN NEW.goal IS NOT OLD.goal AND NEW.confirmed_at IS NULL AND NEW.replan_active = 0 BEGIN
    INSERT INTO contracts(project_id, generation, goal, proposal, source, created_at)
        VALUES(NEW.id, NEW.replan_generation, NEW.goal, NULL, 'goal', NEW.updated_at);
    UPDATE projects SET contract_id = last_insert_rowid() WHERE id = NEW.id;
END;
CREATE TRIGGER IF NOT EXISTS contract_task_insert AFTER INSERT ON tasks BEGIN
    UPDATE tasks SET contract_id = (SELECT contract_id FROM projects WHERE id = NEW.project_id)
        WHERE id = NEW.id;
END;
CREATE TRIGGER IF NOT EXISTS dispatch_job_insert AFTER INSERT ON jobs BEGIN
    UPDATE jobs SET contract_id = COALESCE((SELECT contract_id FROM tasks WHERE id = NEW.task_id),
        (SELECT contract_id FROM projects WHERE id = NEW.project_id))
        WHERE id = NEW.id;
    INSERT INTO dispatches(job_id, project_id, task_id, role, agent_id, created_at)
        VALUES(NEW.id, NEW.project_id, NEW.task_id, NEW.role, NEW.agent_id, NEW.created_at);
END;
CREATE TRIGGER IF NOT EXISTS dispatch_job_resume AFTER UPDATE OF status ON jobs
WHEN NEW.status = 'running' AND OLD.status != 'running' BEGIN
    INSERT INTO dispatches(job_id, project_id, task_id, role, agent_id, created_at)
        VALUES(NEW.id, NEW.project_id, NEW.task_id, NEW.role, NEW.agent_id,
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
END;
CREATE TRIGGER IF NOT EXISTS audit_project_state AFTER UPDATE OF stage, status ON projects
WHEN NEW.stage IS NOT OLD.stage OR NEW.status IS NOT OLD.status BEGIN
    INSERT INTO events(project_id, kind, message, payload, created_at)
        VALUES(NEW.id, 'project_state_changed', '项目阶段或状态变化',
        json_object('version', 1, 'old_stage', OLD.stage, 'old_status', OLD.status,
            'stage', NEW.stage, 'status', NEW.status, 'reason', NEW.blocked_reason,
            'generation', NEW.replan_generation, 'contract_id', NEW.contract_id), NEW.updated_at);
END;
CREATE TRIGGER IF NOT EXISTS audit_task_state AFTER UPDATE OF stage, status ON tasks
WHEN NEW.stage IS NOT OLD.stage OR NEW.status IS NOT OLD.status BEGIN
    INSERT INTO events(project_id, task_id, kind, message, payload, created_at)
        VALUES(NEW.project_id, NEW.id, 'task_state_changed', '任务阶段或状态变化',
        json_object('version', 1, 'old_stage', OLD.stage, 'old_status', OLD.status,
            'stage', NEW.stage, 'status', NEW.status, 'reason', NEW.last_error,
            'contract_id', NEW.contract_id), NEW.updated_at);
END;
CREATE TRIGGER IF NOT EXISTS audit_job_state AFTER UPDATE OF status ON jobs
WHEN NEW.status IS NOT OLD.status BEGIN
    INSERT INTO events(project_id, task_id, job_id, kind, message, payload, created_at)
        VALUES(NEW.project_id, NEW.task_id, NEW.id, 'job_state_changed', '作业状态变化',
        json_object('version', 1, 'old_status', OLD.status, 'status', NEW.status,
            'agent_id', NEW.agent_id, 'role', NEW.role, 'error', NEW.error),
            COALESCE(NEW.finished_at, NEW.started_at, NEW.created_at));
END;
"#;

#[derive(Debug, Default, Serialize, Deserialize, sqlx::FromRow)]
pub struct Limits {
    pub cost_budget: Option<f64>,
    pub task_attempt_limit: Option<i64>,
    pub replan_limit: Option<i64>,
}

impl Limits {
    pub fn validate(&self) -> Result<(), String> {
        if self.cost_budget.is_some_and(|n| !n.is_finite() || n < 0.0)
            || self.task_attempt_limit.is_some_and(|n| n < 1)
            || self.replan_limit.is_some_and(|n| n < 0)
        {
            return Err("费用预算必须非负，任务尝试上限至少为 1，重规划上限必须非负".into());
        }
        Ok(())
    }
}

pub async fn limits(pool: &SqlitePool, project_id: &str) -> Result<Limits> {
    Ok(sqlx::query_as::<_, Limits>(
        "SELECT cost_budget, task_attempt_limit, replan_limit FROM project_limits WHERE project_id = ?",
    ).bind(project_id).fetch_optional(pool).await?.unwrap_or_default())
}

/// In-flight work may settle after this check. Unknown prices stay explicit;
/// the budget limits known consumption, not the provider's eventual bill.
pub async fn wake_configuration(pool: &SqlitePool, project_id: Option<&str>) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query(
        "UPDATE projects SET status = 'running', blocked_reason = NULL, blocked_kind = NULL \
        WHERE status = 'blocked' AND (? IS NULL OR id = ?) AND (blocked_kind = 'configuration' \
        OR (stage = 'executing' AND EXISTS(SELECT 1 FROM tasks WHERE project_id = projects.id \
            AND status = 'blocked' AND last_failure_kind = 'configuration')))",
    )
    .bind(project_id)
    .bind(project_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE tasks SET status = 'queued', last_error = NULL, last_failure_kind = NULL \
        WHERE status = 'blocked' AND last_failure_kind = 'configuration' AND (? IS NULL OR project_id = ?)")
        .bind(project_id).bind(project_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn gate(
    pool: &SqlitePool,
    project_id: &str,
    task_id: Option<&str>,
) -> Result<Option<String>> {
    let config = limits(pool, project_id).await?;
    let (cost, unknown): (f64, i64) = sqlx::query_as(
        "SELECT CAST(COALESCE(SUM(cost), 0) AS REAL), COALESCE(SUM(cost IS NULL), 0) FROM accounting_jobs WHERE project_id = ?",
    ).bind(project_id).fetch_one(pool).await?;
    if config.cost_budget.is_some_and(|budget| cost >= budget) {
        return Ok(Some(format!(
            "项目费用预算已达到：已知费用 {cost}，未知费用作业 {unknown}；请提高预算后继续"
        )));
    }
    if let (Some(task_id), Some(limit)) = (task_id, config.task_attempt_limit) {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM dispatches WHERE task_id = ? AND role = 'executor'",
        )
        .bind(task_id)
        .fetch_one(pool)
        .await?;
        if count >= limit {
            return Ok(Some(format!(
                "任务自动执行尝试已达到上限 {limit}，累计 {count} 次；请提高限制后继续"
            )));
        }
    }
    Ok(None)
}

pub async fn snapshot(pool: &SqlitePool, project_id: &str) -> Result<Value> {
    type DraftRow = (i64, String, Option<String>, String, Option<String>);
    let rows: Vec<(i64, i64, String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT id, generation, goal, proposal, source, created_at FROM contracts WHERE project_id = ? ORDER BY id",
    ).bind(project_id).fetch_all(pool).await?;
    let contracts: Vec<Value> = rows
        .into_iter()
        .map(|(id, generation, goal, proposal, source, at)| {
            json!({"id": id, "generation": generation, "goal": goal,
            "proposal": proposal.and_then(|p| serde_json::from_str::<Value>(&p).ok()),
            "source": source, "created_at": at})
        })
        .collect();
    let drafts: Vec<DraftRow> = sqlx::query_as(
        "SELECT generation, goal, draft, state, reason FROM replan_history WHERE project_id = ? ORDER BY generation",
    ).bind(project_id).fetch_all(pool).await?;
    let current: Option<i64> = sqlx::query_scalar("SELECT contract_id FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(pool)
        .await?;
    let acceptance: Option<String> = sqlx::query_scalar(
        "SELECT result FROM jobs WHERE project_id = ? AND role = 'project_acceptance' AND status = 'succeeded' \
         AND contract_id = (SELECT contract_id FROM projects WHERE id = jobs.project_id) ORDER BY created_at DESC LIMIT 1",
    ).bind(project_id).fetch_optional(pool).await?.flatten();
    let merge: Option<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT phase, base_before, project_before, error FROM merge_operations WHERE project_id = ?",
    ).bind(project_id).fetch_optional(pool).await?;
    let (cost, unknown): (f64, i64) = sqlx::query_as(
        "SELECT CAST(COALESCE(SUM(cost), 0) AS REAL), COALESCE(SUM(cost IS NULL), 0) FROM accounting_jobs WHERE project_id = ?",
    ).bind(project_id).fetch_one(pool).await?;
    Ok(
        json!({"contracts": contracts, "current_contract_id": current,
        "replans": drafts.into_iter().map(|(generation, goal, draft, state, reason)| json!({"generation": generation,
            "goal": goal, "draft": draft.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()), "state": state, "reason": reason})).collect::<Vec<_>>(),
        "acceptance": acceptance.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()),
        "merge_operation": merge.map(|(phase, base, project, error)| json!({"phase": phase, "base_before": base, "project_before": project, "error": error})),
        "limits": limits(pool, project_id).await?, "known_cost": cost, "unknown_cost_jobs": unknown}),
    )
}

pub async fn job_contract(pool: &SqlitePool, job_id: &str) -> Result<Value> {
    let (id, goal, proposal): (i64, String, Option<String>) = sqlx::query_as(
        "SELECT c.id, c.goal, c.proposal FROM contracts c JOIN jobs j ON j.contract_id = c.id WHERE j.id = ?",
    ).bind(job_id).fetch_one(pool).await?;
    Ok(
        json!({"id": id, "goal": goal, "proposal": proposal.and_then(|p| serde_json::from_str::<Value>(&p).ok())}),
    )
}

/// Verdict parsing extracts the role result; delivery authorization separately
/// checks its evidence against the job's contract and artifact version.
pub fn validate_evidence(
    raw: &str,
    contract: &Value,
    criteria: &[String],
    artifact_version: &Value,
) -> Result<(), String> {
    let result = crate::domain::extract_json(raw).ok_or("验收结果不是 JSON")?;
    let evidence = result.get("evidence").ok_or("验收缺少 evidence 证据对象")?;
    if evidence.get("contract_id") != contract.get("id") {
        return Err("验收证据引用了错误的交付契约版本".into());
    }
    if artifact_version.is_string() && evidence.get("artifact_version") != Some(artifact_version) {
        return Err("验收证据没有引用本轮所检查的 Git 提交版本".into());
    }
    for field in ["artifacts", "risks", "human_checks"] {
        if !evidence.get(field).is_some_and(Value::is_array) {
            return Err(format!("验收证据缺少 {field} 数组；无对应项时填写空数组"));
        }
    }
    let checked = evidence
        .get("criteria")
        .and_then(Value::as_array)
        .ok_or("验收证据缺少逐项 criteria")?;
    if criteria.is_empty() || checked.len() != criteria.len() {
        return Err("验收证据条目数必须与当前验收标准一致".into());
    }
    for (criterion, item) in criteria.iter().zip(checked) {
        if item.get("criterion").and_then(Value::as_str) != Some(criterion.as_str())
            || !item.get("passed").is_some_and(Value::is_boolean)
        {
            return Err(format!("验收证据未逐项对照标准：{criterion}"));
        }
        let checks = item
            .get("checks")
            .and_then(Value::as_array)
            .ok_or("验收条目缺少实际检查")?;
        if checks.is_empty()
            || checks.iter().any(|check| {
                ["method", "result"].iter().any(|field| {
                    check
                        .get(field)
                        .and_then(Value::as_str)
                        .is_none_or(|value| value.trim().is_empty())
                })
            })
        {
            return Err(format!("验收条目必须说明实际检查方法及结果：{criterion}"));
        }
        if result.get("pass").and_then(Value::as_bool) == Some(true) && item["passed"] != true {
            return Err("验收结论通过，但存在未通过的验收条目".into());
        }
    }
    Ok(())
}

pub fn validate_project_evidence(
    raw: &str,
    contract: &Value,
    criteria: &[String],
    artifact_version: &Value,
) -> Result<(), String> {
    let result = crate::domain::extract_json(raw).ok_or("项目验收结果不是 JSON")?;
    if result.get("contract_id") != contract.get("id")
        || (artifact_version.is_string()
            && result.get("artifact_version") != Some(artifact_version))
    {
        return Err("项目验收没有引用本轮契约及产物版本".into());
    }
    let checked = result
        .get("criteria")
        .and_then(Value::as_array)
        .ok_or("缺少项目验收条目")?;
    if !criteria.is_empty()
        && (checked.len() != criteria.len()
            || checked
                .iter()
                .zip(criteria)
                .any(|(item, criterion)| item["criterion"].as_str() != Some(criterion)))
    {
        return Err("项目验收必须逐项覆盖当前契约验收标准".into());
    }
    for item in checked {
        if item["verdict"] == "met"
            && !item
                .get("evidence")
                .and_then(Value::as_array)
                .is_some_and(|evidence| {
                    evidence
                        .iter()
                        .any(|item| item.as_str().is_some_and(|s| !s.trim().is_empty()))
                })
        {
            return Err("项目验收通过条目必须附实际检查证据".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_verdicts_require_versioned_evidence_and_actual_checks() {
        let contract = json!({"id": 7});
        let criteria = vec!["报告可读".to_string()];
        assert!(validate_evidence(r#"{"pass":true}"#, &contract, &criteria, &Value::Null).is_err());
        let mut result = json!({"pass": true, "evidence": {"contract_id": 7,
            "artifact_version": "abc", "artifacts": ["report.md"], "risks": [], "human_checks": [],
            "criteria": [{"criterion": "报告可读", "passed": true,
                "checks": [{"method": "读取 report.md", "result": "包含结论与引用"}]}]}});
        assert!(
            validate_evidence(&result.to_string(), &contract, &criteria, &json!("abc")).is_ok()
        );
        result["evidence"]["criteria"][0]["checks"] = json!([]);
        assert!(
            validate_evidence(&result.to_string(), &contract, &criteria, &json!("abc")).is_err()
        );
        result["evidence"]["contract_id"] = json!(8);
        assert!(
            validate_evidence(&result.to_string(), &contract, &criteria, &Value::Null).is_err()
        );
    }

    #[tokio::test]
    async fn contracts_audit_and_consumption_survive_retries_and_rollback() {
        let root = std::env::temp_dir().join(format!("domo-delivery-{}", uuid::Uuid::new_v4()));
        let pool = crate::db::connect(&root.join("domo.db")).await.unwrap();
        sqlx::query("INSERT INTO projects(id, name, goal, repo_path, base_branch, branch, workspace, stage, status, created_at, updated_at) \
            VALUES('p', 'report', 'goal', '', '', '', '', 'analysis', 'running', 't0', 't0')")
            .execute(&pool).await.unwrap();
        sqlx::query("UPDATE projects SET proposal = ?, confirmed_at = 't1', stage = 'executing', updated_at = 't1' WHERE id = 'p'")
            .bind(r#"{"kind":"plan","acceptance":["criterion"]}"#).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO tasks(id, project_id, seq, key, title, description, acceptance, deps, stage, status, created_at, updated_at) \
            VALUES('t', 'p', 1, 'T1', 'report', 'write', '[]', '[]', 'executing', 'queued', 't1', 't1')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO jobs(id, project_id, task_id, role, status, prompt, cost, usage_known, result, created_at) \
            VALUES('j', 'p', 't', 'executor', 'failed', 'original input', 2.5, 1, 'original result', 't2')")
            .execute(&pool).await.unwrap();
        let contract = job_contract(&pool, "j").await.unwrap();
        sqlx::query("UPDATE jobs SET status = 'running', result = NULL, cost = NULL, usage_known = 0 WHERE id = 'j'")
            .execute(&pool).await.unwrap();
        let saved: String = sqlx::query_scalar("SELECT snapshot FROM job_runs WHERE job_id = 'j'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(saved.contains("original result"));
        let stats = snapshot(&pool, "p").await.unwrap();
        assert_eq!(stats["known_cost"], 2.5);
        assert_eq!(stats["unknown_cost_jobs"], 1);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dispatches WHERE task_id = 't'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2);
        sqlx::query("UPDATE projects SET proposal = '{\"kind\":\"plan\",\"summary\":\"next\"}', updated_at = 't3' WHERE id = 'p'")
            .execute(&pool).await.unwrap();
        assert_eq!(job_contract(&pool, "j").await.unwrap(), contract);
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
            .fetch_one(&pool)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("UPDATE projects SET status = 'paused' WHERE id = 'p'")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(before, after);
        sqlx::query("INSERT INTO project_limits(project_id, cost_budget, task_attempt_limit, updated_at) VALUES('p', 2.0, 2, 't4')")
            .execute(&pool).await.unwrap();
        assert!(gate(&pool, "p", Some("t"))
            .await
            .unwrap()
            .unwrap()
            .contains("费用预算"));
        sqlx::query("UPDATE project_limits SET cost_budget = NULL WHERE project_id = 'p'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(gate(&pool, "p", Some("t"))
            .await
            .unwrap()
            .unwrap()
            .contains("尝试"));
        pool.close().await;
        for attempt in 0..20 {
            match tokio::fs::remove_dir_all(&root).await {
                Ok(()) => break,
                Err(error)
                    if cfg!(windows)
                        && matches!(error.raw_os_error(), Some(32 | 33))
                        && attempt < 19 =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                Err(error) => panic!("temporary database cleanup failed: {error}"),
            }
        }
    }
}
