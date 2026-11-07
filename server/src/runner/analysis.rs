use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::domain::{self, now};
use crate::events::emit;
use crate::models::*;
use crate::state::AppState;

use super::common::{apply_stale_result, has_active_tasks, insert_message, models_list_json};
use super::failure::plan_project_failure;

pub(super) async fn handle_analysis_questions(
    state: &AppState,
    project: &ProjectRow,
    questions: &[String],
    summary: &str,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    if !crate::replan::analysis_is_current(state, project).await? {
        return Ok(());
    }
    if project.replan_active == 0 && has_active_tasks(&state.db, &project.id).await? {
        return apply_stale_result(
            state,
            project,
            "plan_ignored",
            "项目已有活动任务，忽略规划者提问，回到执行阶段".into(),
        )
        .await;
    }
    let content = if summary.trim().is_empty() {
        questions
            .iter()
            .map(|q| format!("- {q}"))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        format!(
            "{}\n\n{}",
            summary,
            questions
                .iter()
                .map(|q| format!("- {q}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    insert_message(&state.db, &project.id, "planner", &content).await?;
    sqlx::query(
        "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = NULL, \
         proposal = CASE WHEN replan_active = 0 THEN NULL ELSE proposal END, \
         replan_proposal = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_ANALYSIS)
    .bind(STATUS_BLOCKED)
    .bind(now())
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "analysis_questions",
        "规划者提出了需要确认的问题",
    )
    .await;
    Ok(())
}

/// Loads the keys that still occupy the project's unique-key namespace.
/// Canceled tasks are excluded because their keys may be reused.
pub(super) async fn load_task_keys<'e, E>(executor: E, _project_id: &str) -> Result<HashSet<String>>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let keys: Vec<String> = sqlx::query_scalar("SELECT key FROM tasks WHERE project_id = ?")
        .fetch_all(executor)
        .await?;
    Ok(keys.into_iter().collect())
}

/// Rewrites the task keys in a stored proposal after collisions were resolved.
pub(super) fn remap_proposal_keys(raw: &str, mapping: &HashMap<String, String>) -> Result<String> {
    if mapping.is_empty() {
        return Ok(raw.to_string());
    }
    let mut proposal: AnalysisProposal = serde_json::from_str(raw)?;
    for task in &mut proposal.tasks {
        if let Some(key) = mapping.get(&task.key) {
            task.key = key.clone();
        }
        task.deps = task
            .deps
            .iter()
            .map(|dep| mapping.get(dep).cloned().unwrap_or_else(|| dep.clone()))
            .collect();
    }
    Ok(serde_json::to_string(&proposal)?)
}

pub(crate) async fn apply_analysis_plan(
    state: &AppState,
    project: &ProjectRow,
    goal: &str,
    tasks: &[domain::NormalizedTask],
    summary: &str,
    proposal_json: &str,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    apply_analysis_plan_locked(state, project, goal, tasks, summary, proposal_json).await
}

/// Applies an already generated plan without another planner invocation.
/// Caller holds the scheduling lock through validation and application.
pub(crate) async fn apply_analysis_plan_locked(
    state: &AppState,
    project: &ProjectRow,
    goal: &str,
    tasks: &[domain::NormalizedTask],
    summary: &str,
    proposal_json: &str,
) -> Result<()> {
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let current = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
        .bind(&project.id)
        .fetch_one(&mut *tx)
        .await?;
    if current.status.as_deref() == Some(STATUS_ARCHIVED)
        || current.replan_generation != project.replan_generation
        || current.replan_active != project.replan_active
    {
        return Err(crate::replan::StaleReplan.into());
    }
    if current.goal_locked_at.is_some()
        && (current.replan_generation != project.replan_generation
            || current.goal != project.goal
            || current.goal.trim() != goal.trim())
    {
        return Err(crate::replan::StaleReplan.into());
    }
    let timestamp = now();
    let applied = if project.replan_active != 0 {
        crate::replan::validate_apply_tx(&mut tx, project, proposal_json).await?;
        Some(crate::replan::supersede_tx(&mut tx, &project.id, &timestamp).await?)
    } else {
        if current.stage != STAGE_ANALYSIS
            || (current.proposal.as_deref() != Some(proposal_json)
                && current.confirm_mode != CONFIRM_AUTO)
        {
            return Err(crate::replan::StaleReplan.into());
        }
        let existing: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND stage != ? \
             AND COALESCE(status, '') != ?",
        )
        .bind(&project.id)
        .bind(TASK_STAGE_COMPLETED)
        .bind(STATUS_CANCELED)
        .fetch_one(&mut *tx)
        .await?;
        if existing > 0 {
            tx.rollback().await?;
            apply_stale_result(
                state,
                project,
                "plan_ignored",
                "项目已有活动任务，忽略规划结果，回到执行阶段".into(),
            )
            .await?;
            return Ok(());
        }
        None
    };
    let taken = load_task_keys(&mut *tx, &project.id).await?;
    let (tasks, mapping) = domain::reserve_plan_keys(tasks.to_vec(), &taken);
    let proposal_json = remap_proposal_keys(proposal_json, &mapping)?;
    let max_seq: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM tasks WHERE project_id = ?")
            .bind(&project.id)
            .fetch_one(&mut *tx)
            .await?;
    let max_queue: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(queue_order), 0) FROM tasks WHERE project_id = ?")
            .bind(&project.id)
            .fetch_one(&mut *tx)
            .await?;
    for (index, task) in tasks.iter().enumerate() {
        let task_id = domain::new_id("tsk");
        sqlx::query(
            "INSERT INTO tasks (id, project_id, seq, queue_order, key, title, description, \
             acceptance, deps, stage, status, attempts, rejections, verifier_issues, created_at, \
             updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, '[]', ?, ?)",
        )
        .bind(&task_id)
        .bind(&project.id)
        .bind(max_seq + (index + 1) as i64)
        .bind(max_queue + (index + 1) as i64)
        .bind(&task.key)
        .bind(&task.title)
        .bind(&task.description)
        .bind(models_list_json(&task.acceptance))
        .bind(models_list_json(&task.deps))
        .bind(TASK_STAGE_PENDING)
        .bind(STATUS_QUEUED)
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(&mut *tx)
        .await?;
        crate::db::initialize_task_agents_tx(&mut tx, &task_id).await?;
    }
    if project.replan_active != 0 {
        crate::replan::enqueue_cleanup_tx(
            &mut tx,
            &project.id,
            project.replan_generation,
            &timestamp,
        )
        .await?;
    }
    // The acceptance round is cumulative across plans so automatic replans cannot bypass its cap.
    sqlx::query(
        "UPDATE projects SET goal = CASE WHEN replan_active != 0 THEN goal ELSE ? END, \
         proposal = ?, stage = ?, \
         status = CASE WHEN status IN ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
         blocked_reason = NULL, report = NULL, report_feedback = NULL, \
         acceptance = NULL, acceptance_passed = 0, acceptance_at = NULL, \
         replan_active = 0, replan_proposal = NULL, replan_previous_stage = NULL, \
         replan_previous_goal = NULL, goal_locked_at = COALESCE(goal_locked_at, ?), \
         confirmed_at = COALESCE(confirmed_at, ?), updated_at = ? WHERE id = ?",
    )
    .bind(goal)
    .bind(proposal_json)
    .bind(STAGE_EXECUTING)
    .bind(STATUS_RUNNING)
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(&project.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if let Some(applied) = applied {
        crate::replan::finish_apply(state, project, applied).await;
    }
    let note = if summary.trim().is_empty() {
        format!("项目分析完成，共 {} 个任务", tasks.len())
    } else {
        format!("项目分析完成，共 {} 个任务：{}", tasks.len(), summary)
    };
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "plan_created",
        note,
    )
    .await;
    Ok(())
}

pub(super) fn analysis_proposal_content(
    summary: &str,
    goal: Option<&str>,
    assumptions: &[String],
    acceptance: &[String],
    tasks: &[domain::NormalizedTask],
    plan: &[String],
    verification: &[String],
) -> String {
    let mut content = summary.trim().to_string();
    if let Some(goal) = goal.map(str::trim).filter(|value| !value.is_empty()) {
        content.push_str(&format!("\n\n目标：{goal}"));
    }
    let push_list = |content: &mut String, title: &str, items: &[String]| {
        if items.is_empty() {
            return;
        }
        content.push_str(&format!("\n\n{title}："));
        for item in items {
            content.push_str(&format!("\n- {item}"));
        }
    };
    push_list(&mut content, "关键假设", assumptions);
    push_list(&mut content, "验收口径", acceptance);
    if !tasks.is_empty() {
        content.push_str("\n\n任务：");
        for task in tasks {
            content.push_str(&format!(
                "\n- {} {}（依赖：{}）",
                task.key,
                task.title,
                if task.deps.is_empty() {
                    "无".to_string()
                } else {
                    task.deps.join("、")
                }
            ));
            for item in &task.acceptance {
                content.push_str(&format!("\n  - 验收：{item}"));
            }
        }
    }
    push_list(&mut content, "执行步骤", plan);
    push_list(&mut content, "自检方式", verification);
    content
}

/// 自动确认取作业结束时刻的确认方式，运行中切换对当前作业立即生效。
async fn latest_confirm_mode(state: &AppState, project_id: &str) -> Result<String> {
    let mode = sqlx::query_scalar::<_, String>("SELECT confirm_mode FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(&state.db)
        .await?;
    Ok(mode)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_analysis_plan(
    state: &AppState,
    project: &ProjectRow,
    summary: &str,
    goal: Option<&str>,
    assumptions: &[String],
    acceptance: &[String],
    tasks: &[domain::NormalizedTask],
) -> Result<()> {
    let goal = goal
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(project.goal.trim())
        .to_string();
    if project.goal_locked_at.is_some() && goal.trim() != project.goal.trim() {
        return crate::replan::reject_goal_change(state, project).await;
    }
    let taken = if project.replan_active != 0 {
        sqlx::query_scalar::<_, String>("SELECT key FROM tasks WHERE project_id = ? AND stage = ?")
            .bind(&project.id)
            .bind(TASK_STAGE_COMPLETED)
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .collect()
    } else {
        load_task_keys(&state.db, &project.id).await?
    };
    let (tasks, _) = domain::reserve_plan_keys(tasks.to_vec(), &taken);
    let replan_impact = if project.replan_active != 0 {
        Some(crate::replan::impact(state, project, acceptance, &tasks).await?)
    } else {
        None
    };
    let proposal = AnalysisProposal {
        kind: "plan".into(),
        summary: summary.to_string(),
        goal: Some(goal.clone()),
        assumptions: assumptions.to_vec(),
        acceptance: acceptance.to_vec(),
        tasks: tasks
            .iter()
            .map(|task| ProposalTask {
                key: task.key.clone(),
                title: task.title.clone(),
                description: task.description.clone(),
                acceptance: task.acceptance.clone(),
                deps: task.deps.clone(),
            })
            .collect(),
        replan_impact,
    };
    let proposal_json = serde_json::to_string(&proposal)?;
    let confirm_mode = latest_confirm_mode(state, &project.id).await?;
    // 首次分析和重规划使用相同的确认策略。
    let auto = confirm_mode == CONFIRM_AUTO;
    if project.replan_active != 0
        && !crate::replan::store_draft(state, project, &proposal_json).await?
    {
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            None,
            "plan_ignored",
            "重规划代际已变化，忽略过期分析方案",
        )
        .await;
        return Ok(());
    }
    if auto {
        apply_analysis_plan(state, project, &goal, &tasks, summary, &proposal_json).await?;
        if project.replan_active != 0 {
            emit(
                &state.db,
                &state.bus,
                Some(&project.id),
                None,
                "replan_applied",
                format!(
                    "重规划草稿已自动应用，取消未完成任务并创建 {} 个新任务",
                    tasks.len()
                ),
            )
            .await;
        }
        return Ok(());
    }
    let _guard = state.schedule_lock.lock().await;
    if !crate::replan::analysis_is_current(state, project).await? {
        return Ok(());
    }
    let current = crate::replan::load_project(&state.db, &project.id)
        .await?
        .unwrap_or_else(|| project.clone());
    if project.goal_locked_at.is_some()
        && (current.replan_generation != project.replan_generation
            || current.goal != project.goal
            || current.goal.trim() != goal.trim())
    {
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            None,
            "plan_ignored",
            "项目目标或分析代际已变化，忽略过期方案",
        )
        .await;
        return Ok(());
    }
    let content = analysis_proposal_content(
        summary,
        Some(&goal),
        assumptions,
        acceptance,
        &tasks,
        &[],
        &[],
    );
    insert_message(&state.db, &project.id, "planner", &content).await?;
    if project.replan_active != 0 {
        sqlx::query(
            "UPDATE projects SET replan_proposal = ?, stage = ?, \
             status = CASE WHEN status IN ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
             blocked_reason = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(&proposal_json)
        .bind(STAGE_ANALYSIS)
        .bind(STATUS_BLOCKED)
        .bind(now())
        .bind(&project.id)
        .execute(&state.db)
        .await?;
    } else {
        sqlx::query(
            "UPDATE projects SET goal = ?, proposal = ?, stage = ?, \
             status = CASE WHEN status IN ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
             blocked_reason = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(&goal)
        .bind(&proposal_json)
        .bind(STAGE_ANALYSIS)
        .bind(STATUS_BLOCKED)
        .bind(now())
        .bind(&project.id)
        .execute(&state.db)
        .await?;
    }
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "analysis_ready",
        if proposal
            .replan_impact
            .as_ref()
            .is_some_and(|impact| impact.high_impact)
        {
            "高影响重规划方案已生成，等待人工确认"
        } else {
            "规划者已给出项目方案，等待人工确认"
        },
    )
    .await;
    Ok(())
}

pub(super) async fn handle_analysis_failure(
    state: &AppState,
    project: &ProjectRow,
    reason: &str,
    agent: Option<&AgentRow>,
    process_failure: bool,
) -> Result<()> {
    let failure = plan_project_failure(
        state,
        project,
        reason,
        agent.map(|agent| (agent, process_failure)),
    )
    .await?;
    let _guard = state.schedule_lock.lock().await;
    if !crate::replan::analysis_is_current(state, project).await? {
        return Ok(());
    }
    if project.replan_active == 0 && has_active_tasks(&state.db, &project.id).await? {
        return apply_stale_result(
            state,
            project,
            "plan_failed",
            format!("项目分析失败：{reason}（项目已有活动任务，保持在执行阶段）"),
        )
        .await;
    }
    sqlx::query(
        "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = ?, replan_proposal = NULL, \
         avoid_agent_id = CASE WHEN ? THEN ? ELSE avoid_agent_id END, \
         avoid_agent_role = CASE WHEN ? THEN ? ELSE avoid_agent_role END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_ANALYSIS)
    .bind(failure.status())
    .bind(&failure.blocked_reason)
    .bind(failure.switch)
    .bind(failure.switch.then(|| agent.map(|agent| agent.id.as_str())).flatten())
    .bind(failure.switch)
    .bind(failure.switch.then_some(ROLE_PLANNER))
    .bind(now())
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        failure.event_kind("analysis_failed"),
        format!("项目分析{}：{reason}", failure.label()),
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn remaps_proposal_keys_and_dependencies() {
        let raw = r#"{"kind":"plan","summary":"s","tasks":[
            {"key":"T1","title":"a","description":"b","acceptance":[],"deps":[]},
            {"key":"T2","title":"c","description":"d","acceptance":[],"deps":["T1"]}
        ]}"#;
        let mapping = HashMap::from([
            ("T1".to_string(), "T4".to_string()),
            ("T2".to_string(), "T5".to_string()),
        ]);

        let rewritten = remap_proposal_keys(raw, &mapping).unwrap();
        let proposal: AnalysisProposal = serde_json::from_str(&rewritten).unwrap();

        assert_eq!(proposal.tasks[0].key, "T4");
        assert_eq!(proposal.tasks[1].key, "T5");
        assert_eq!(proposal.tasks[1].deps, vec!["T4"]);
    }

    #[test]
    fn empty_mapping_keeps_proposal_raw() {
        let raw = r#"{"kind":"plan","summary":"s","tasks":[]}"#;
        assert_eq!(remap_proposal_keys(raw, &HashMap::new()).unwrap(), raw);
    }

    #[tokio::test]
    async fn latest_confirm_mode_reads_the_project_row_at_completion() {
        let root = std::env::temp_dir().join(format!("domo-confirm-mode-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = crate::db::connect(&root.join("data/domo.db"))
            .await
            .unwrap();
        let timestamp = "2026-01-01T00:00:00Z";
        sqlx::query(
            "INSERT INTO projects (id, name, goal, repo_path, base_branch, branch, workspace, \
             stage, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("prj_reload")
        .bind("test")
        .bind("goal")
        .bind("/tmp/repo")
        .bind("main")
        .bind("domo/p-test")
        .bind("/tmp/workspace")
        .bind(STAGE_ANALYSIS)
        .bind(STATUS_RUNNING)
        .bind(timestamp)
        .bind(timestamp)
        .execute(&db)
        .await
        .unwrap();
        let cfg = crate::config::Config {
            data_root: root.join("data"),
            workspace_root: root.join("workspace"),
            log_root: root.join("log"),
            web_dist: root.join("web"),
            bind: "127.0.0.1".into(),
            port: 0,
            port_explicit: false,
            job_timeout: Duration::ZERO,
            stall_timeout: Duration::ZERO,
        };
        let (notify, _receiver) = tokio::sync::mpsc::channel(1);
        let state = AppState::new(db, cfg, crate::events::EventBus::new(), notify);

        assert_eq!(
            latest_confirm_mode(&state, "prj_reload").await.unwrap(),
            CONFIRM_MANUAL
        );
        sqlx::query("UPDATE projects SET confirm_mode = ? WHERE id = ?")
            .bind(CONFIRM_AUTO)
            .bind("prj_reload")
            .execute(&state.db)
            .await
            .unwrap();
        assert_eq!(
            latest_confirm_mode(&state, "prj_reload").await.unwrap(),
            CONFIRM_AUTO
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
