//! API 模块的单元测试。
use super::agents::{require_command_if_needed, validate_kind};
use super::tasks::update_analysis_policy;
use super::verification::{latest_failed_job, task_verifications, VERIFICATION_HISTORY_LIMIT};
use crate::agents;
use crate::models::*;
use crate::state::AppState;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use std::path::PathBuf;

fn bad_message(kind: &str) -> String {
    validate_kind(kind).expect_err("未知类型应被拒绝").message
}

fn cli_kind() -> &'static str {
    <agents::CliAdapter as agents::AgentAdapter>::KIND
}

#[test]
fn validate_kind_accepts_every_registered_type() {
    for kind in agents::kinds() {
        assert_eq!(validate_kind(kind).unwrap(), *kind);
    }
}

#[test]
fn validate_kind_rejects_unknown_type_and_lists_options() {
    let message = bad_message("unknown");
    let mut cursor = 0;
    for kind in agents::kinds() {
        let index = message
            .find(kind)
            .unwrap_or_else(|| panic!("错误信息缺少 {kind}: {message}"));
        assert!(
            index >= cursor,
            "错误信息未按注册表顺序列出 {kind}: {message}"
        );
        cursor = index + kind.len();
    }
}

#[test]
fn only_the_cli_adapter_requires_a_command_template() {
    let empty: [String; 0] = [];
    for kind in agents::kinds() {
        let result = require_command_if_needed(kind, &empty);
        if *kind == cli_kind() {
            let error = result.unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
            assert_eq!(error.message, "CLI Agent 必须配置命令模板");
        } else {
            result.unwrap();
        }
    }
}

#[test]
fn custom_cli_accepts_non_empty_command_template() {
    let command = vec!["codex".to_string(), "exec".to_string()];
    assert!(require_command_if_needed(cli_kind(), &command).is_ok());
}

async fn verification_test_state() -> (AppState, PathBuf) {
    let root = std::env::temp_dir().join(format!("domo-verifications-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let db = crate::db::connect(&root.join("data/domo.db"))
        .await
        .unwrap();
    let timestamp = "2026-01-01T00:00:00Z";
    sqlx::query(
        "INSERT INTO projects (id, name, goal, repo_path, base_branch, branch, workspace, \
         stage, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("prj_test")
    .bind("test")
    .bind("goal")
    .bind("/tmp/repo")
    .bind("main")
    .bind("domo/p-test")
    .bind("/tmp/workspace")
    .bind(STAGE_EXECUTING)
    .bind(STATUS_RUNNING)
    .bind(timestamp)
    .bind(timestamp)
    .execute(&db)
    .await
    .unwrap();
    for (id, key) in [("tsk_a", "T1"), ("tsk_empty", "T2")] {
        sqlx::query(
            "INSERT INTO tasks (id, project_id, seq, queue_order, key, title, description, \
             acceptance, deps, stage, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind("prj_test")
        .bind(1)
        .bind(1)
        .bind(key)
        .bind(key)
        .bind("description")
        .bind("[]")
        .bind("[]")
        .bind(TASK_STAGE_EXECUTING)
        .bind(timestamp)
        .bind(timestamp)
        .execute(&db)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO agents (id, name, kind, model, roles, created_at, updated_at) \
         VALUES (?, ?, ?, '', ?, ?, ?)",
    )
    .bind("agt_verifier")
    .bind("验收 Agent")
    .bind(AGENT_MOCK)
    .bind(string_list_json(&[ROLE_VERIFIER.to_string()]))
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
        job_timeout: std::time::Duration::ZERO,
        stall_timeout: std::time::Duration::ZERO,
    };
    let (notify, _receiver) = tokio::sync::mpsc::channel(1);
    let state = AppState::new(db, cfg, crate::events::EventBus::new(), notify);
    (state, root)
}

#[tokio::test]
async fn user_actions_belong_to_the_current_blockage_not_historical_jobs() {
    let (state, root) = verification_test_state().await;
    let notice = serde_json::json!({
        "summary": "请提供原始销售数据", "why": "缺少原始记录，无法核对报表。",
        "owner": "数据提供方", "steps": ["上传 CSV，并告知文件路径。"],
        "after": "记录路径并继续，AI 将核验数据并完成报表。"
    });
    let result = serde_json::json!({"pass": false, "reason": "原始记录缺项", "issues": [],
        "next_action": "wait", "guidance": "核对 CSV 字段并重新计算", "user_action": notice});
    sqlx::query("INSERT INTO jobs(id, project_id, task_id, role, status, prompt, result, created_at) VALUES('notice_job', 'prj_test', 'tsk_a', 'verifier', 'succeeded', '', ?, 't')")
        .bind(result.to_string()).execute(&state.db).await.unwrap();
    let cause = "原始记录缺项\n处理指导：核对 CSV 字段并重新计算";
    sqlx::query("UPDATE tasks SET status = 'blocked', last_error = ? WHERE id = 'tsk_a'")
        .bind(cause)
        .execute(&state.db)
        .await
        .unwrap();
    async fn action(state: &AppState) -> Option<crate::user_action::UserAction> {
        let task: TaskRow = sqlx::query_as("SELECT * FROM tasks WHERE id = 'tsk_a'")
            .fetch_one(&state.db)
            .await
            .unwrap();
        crate::user_action::for_task(&state.db, &task)
            .await
            .unwrap()
    }
    assert_eq!(action(&state).await.unwrap().summary, "请提供原始销售数据");
    for (status, error) in [
        ("queued", cause),
        ("paused", cause),
        ("blocked", "另一个错误"),
    ] {
        sqlx::query("UPDATE tasks SET status = ?, last_error = ? WHERE id = 'tsk_a'")
            .bind(status)
            .bind(error)
            .execute(&state.db)
            .await
            .unwrap();
        assert!(action(&state).await.is_none());
    }
    sqlx::query("UPDATE tasks SET status = 'blocked', last_error = ? WHERE id = 'tsk_a'")
        .bind(cause)
        .execute(&state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO jobs(id, project_id, task_id, role, status, prompt, created_at) VALUES('new_job', 'prj_test', 'tsk_a', 'verifier', 'running', '', 't')")
        .execute(&state.db).await.unwrap();
    assert!(
        action(&state).await.is_none(),
        "a newer attempt invalidates the old notice"
    );
    let snapshot = serde_json::json!({"next_action": "wait", "user_action": notice});
    sqlx::query("UPDATE projects SET stage = 'summarizing', status = 'blocked', acceptance = ?, blocked_reason = ? WHERE id = 'prj_test'")
        .bind(snapshot.to_string()).bind("请提供原始销售数据").execute(&state.db).await.unwrap();
    let mut project: ProjectRow = sqlx::query_as("SELECT * FROM projects WHERE id = 'prj_test'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert!(crate::user_action::for_project(&project).is_some());
    project.blocked_reason = Some("新的阻塞".into());
    assert!(crate::user_action::for_project(&project).is_none());
    state.db.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn default_team_reorders_wait_for_membership_lock() {
    let (state, root) = verification_test_state().await;
    for id in ["executor_a", "executor_b", "executor_c"] {
        sqlx::query("INSERT INTO agents(id, name, kind, roles, created_at, updated_at) VALUES(?, ?, 'mock', '[\"executor\"]', 't', 't')")
            .bind(id).bind(id).execute(&state.db).await.unwrap();
    }
    crate::db::ensure_default_team(&state.db).await.unwrap();
    let members: Vec<String> = sqlx::query_scalar(
        "SELECT agent_id FROM default_team WHERE role = 'executor' ORDER BY priority",
    )
    .fetch_all(&state.db)
    .await
    .unwrap();
    let guard = state.schedule_lock.lock().await;
    let mut requests = Vec::new();
    for id in members.iter().take(2) {
        let state = state.clone();
        let id = id.clone();
        requests.push(tokio::spawn(async move {
            let _ = super::agents::update_default_team_agent(
                State(state),
                Path(("executor".into(), id)),
                Json(ProjectAgentUpdateRequest {
                    direction: Some("down".into()),
                    position: None,
                    enabled: None,
                }),
            )
            .await
            .unwrap();
        }));
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(requests.iter().all(|request| !request.is_finished()));
    drop(guard);
    for request in requests {
        request.await.unwrap();
    }
    let priorities: Vec<i64> = sqlx::query_scalar(
        "SELECT priority FROM default_team WHERE role = 'executor' ORDER BY priority",
    )
    .fetch_all(&state.db)
    .await
    .unwrap();
    assert_eq!(priorities, (0..members.len() as i64).collect::<Vec<_>>());
    state.db.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn queued_resume_commands_cannot_revive_an_archived_project() {
    let (state, root) = verification_test_state().await;
    sqlx::query("UPDATE projects SET status = 'paused' WHERE id = 'prj_test'")
        .execute(&state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET status = 'paused' WHERE id = 'tsk_a'")
        .execute(&state.db)
        .await
        .unwrap();
    let guard = state.schedule_lock.lock().await;
    let project_state = state.clone();
    let project_request = tokio::spawn(async move {
        super::tasks::resume_project(State(project_state), Path("prj_test".into())).await
    });
    let task_state = state.clone();
    let task_request = tokio::spawn(async move {
        super::tasks::resume_task(State(task_state), Path(("prj_test".into(), "tsk_a".into())))
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(!project_request.is_finished() && !task_request.is_finished());
    sqlx::query("UPDATE projects SET status = 'archived' WHERE id = 'prj_test'")
        .execute(&state.db)
        .await
        .unwrap();
    drop(guard);
    assert_eq!(
        project_request.await.unwrap().unwrap_err().status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        task_request.await.unwrap().unwrap_err().status,
        StatusCode::BAD_REQUEST
    );
    let status: String = sqlx::query_scalar("SELECT status FROM projects WHERE id = 'prj_test'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(status, STATUS_ARCHIVED);
    let task_status: String = sqlx::query_scalar("SELECT status FROM tasks WHERE id = 'tsk_a'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(task_status, STATUS_PAUSED);
    state.db.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn queued_analysis_commands_validate_latest_lifecycle() {
    let (state, root) = verification_test_state().await;
    set_project_analysis_state(&state, STATUS_RUNNING).await;
    let guard = state.schedule_lock.lock().await;
    let message_state = state.clone();
    let message = tokio::spawn(async move {
        super::tasks::post_analysis_message(
            State(message_state),
            Path("prj_test".into()),
            Json(AnalysisMessageRequest {
                message: "不应进入已归档项目".into(),
                confirm_mode: None,
            }),
        )
        .await
    });
    let policy_state = state.clone();
    let policy = tokio::spawn(async move {
        update_analysis_policy(
            State(policy_state),
            Path("prj_test".into()),
            Json(AnalysisPolicyRequest {
                mode: CONFIRM_AUTO.into(),
            }),
        )
        .await
    });
    let confirm_state = state.clone();
    let confirm = tokio::spawn(async move {
        super::tasks::confirm_analysis(
            State(confirm_state),
            Path("prj_test".into()),
            Json(ConfirmAnalysisRequest::default()),
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(!message.is_finished() && !policy.is_finished() && !confirm.is_finished());
    sqlx::query("UPDATE projects SET status = 'archived' WHERE id = 'prj_test'")
        .execute(&state.db)
        .await
        .unwrap();
    drop(guard);
    for request in [message, policy, confirm] {
        let error = request.await.unwrap().unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("归档"));
    }
    let messages: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE project_id = 'prj_test'")
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(messages, 0);
    let mode: String =
        sqlx::query_scalar("SELECT confirm_mode FROM projects WHERE id = 'prj_test'")
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(mode, CONFIRM_MANUAL);
    state.db.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn canceled_task_rejects_delayed_resume_and_result() {
    let (state, root) = verification_test_state().await;
    sqlx::query("UPDATE tasks SET status = 'paused' WHERE id = 'tsk_a'")
        .execute(&state.db)
        .await
        .unwrap();
    let project = super::helpers::require_project(&state, "prj_test")
        .await
        .unwrap();
    let task = super::helpers::load_project_task(&state, "prj_test", "tsk_a")
        .await
        .unwrap();
    let guard = state.schedule_lock.lock().await;
    let request_state = state.clone();
    let request = tokio::spawn(async move {
        super::tasks::resume_task(
            State(request_state),
            Path(("prj_test".into(), "tsk_a".into())),
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    sqlx::query("UPDATE tasks SET status = 'canceled' WHERE id = 'tsk_a'")
        .execute(&state.db)
        .await
        .unwrap();
    drop(guard);
    assert_eq!(
        request.await.unwrap().unwrap_err().status,
        StatusCode::BAD_REQUEST
    );
    let transition = crate::transitions::Transitions::lock(&state).await;
    assert!(transition
        .task_result(&project, &task)
        .await
        .unwrap()
        .is_none());
    drop(transition);
    state.db.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn result_scope_allows_draining_but_rejects_replaced_contracts() {
    let (state, root) = verification_test_state().await;
    let project = super::helpers::require_project(&state, "prj_test")
        .await
        .unwrap();
    let task = super::helpers::load_project_task(&state, "prj_test", "tsk_a")
        .await
        .unwrap();
    let transition = crate::transitions::Transitions::lock(&state).await;
    sqlx::query("UPDATE projects SET stage = 'analysis', replan_active = 1, replan_generation = 1 WHERE id = 'prj_test'").execute(&state.db).await.unwrap();
    assert!(transition
        .task_result(&project, &task)
        .await
        .unwrap()
        .is_some());
    sqlx::query("INSERT INTO contracts(project_id, generation, goal, source, created_at) VALUES('prj_test', 1, 'new goal', 'plan', 't')").execute(&state.db).await.unwrap();
    sqlx::query("UPDATE projects SET contract_id = (SELECT MAX(id) FROM contracts), stage = 'executing', replan_active = 0 WHERE id = 'prj_test'").execute(&state.db).await.unwrap();
    assert!(transition.project_result(&project).await.unwrap().is_none());
    assert!(transition
        .task_result(&project, &task)
        .await
        .unwrap()
        .is_none());
    drop(transition);
    state.db.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn terminal_state_guards_preserve_audit_and_family_cancellation() {
    let (state, root) = verification_test_state().await;
    sqlx::query("UPDATE tasks SET stage = 'completed', status = NULL WHERE id = 'tsk_a'")
        .execute(&state.db)
        .await
        .unwrap();
    assert!(sqlx::query(
        "UPDATE tasks SET stage = 'executing', status = 'queued' WHERE id = 'tsk_a'"
    )
    .execute(&state.db)
    .await
    .is_err());
    // Completed children may leave the effective plan with their whole family.
    sqlx::query("UPDATE tasks SET status = 'canceled' WHERE id = 'tsk_a'")
        .execute(&state.db)
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE tasks SET status = NULL WHERE id = 'tsk_a'")
            .execute(&state.db)
            .await
            .is_err()
    );
    sqlx::query("UPDATE tasks SET artifacts_released_at = 't' WHERE id = 'tsk_a'")
        .execute(&state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE projects SET status = 'archived' WHERE id = 'prj_test'")
        .execute(&state.db)
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE projects SET status = 'running' WHERE id = 'prj_test'")
            .execute(&state.db)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE projects SET stage = 'analysis' WHERE id = 'prj_test'")
            .execute(&state.db)
            .await
            .is_err()
    );
    sqlx::query("UPDATE projects SET updated_at = 't' WHERE id = 'prj_test'")
        .execute(&state.db)
        .await
        .unwrap();
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'project_state_changed' AND json_extract(payload, '$.status') = 'running'").fetch_one(&state.db).await.unwrap();
    assert_eq!(events, 0);
    state.db.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn ordinary_pause_keeps_inflight_results_eligible() {
    let (state, root) = verification_test_state().await;
    let project = super::helpers::require_project(&state, "prj_test")
        .await
        .unwrap();
    let task = super::helpers::load_project_task(&state, "prj_test", "tsk_a")
        .await
        .unwrap();
    let transition = crate::transitions::Transitions::lock(&state).await;
    for status in [STATUS_PAUSING, STATUS_PAUSED] {
        sqlx::query("UPDATE projects SET status = ? WHERE id = 'prj_test'")
            .bind(status)
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("UPDATE tasks SET status = ? WHERE id = 'tsk_a'")
            .bind(status)
            .execute(&state.db)
            .await
            .unwrap();
        assert!(transition.project_result(&project).await.unwrap().is_some());
        assert!(transition
            .task_result(&project, &task)
            .await
            .unwrap()
            .is_some());
    }
    sqlx::query("UPDATE tasks SET stage = 'completed', status = NULL WHERE id = 'tsk_a'")
        .execute(&state.db)
        .await
        .unwrap();
    assert!(transition
        .task_result(&project, &task)
        .await
        .unwrap()
        .is_none());
    drop(transition);
    state.db.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn stale_analysis_plan_cannot_apply_before_goal_confirmation() {
    let (state, root) = verification_test_state().await;
    set_project_analysis_state(&state, STATUS_RUNNING).await;
    let project = super::helpers::require_project(&state, "prj_test")
        .await
        .unwrap();
    let _guard = state.schedule_lock.lock().await;
    sqlx::query("UPDATE projects SET replan_generation = replan_generation + 1, confirm_mode = 'auto' WHERE id = 'prj_test'").execute(&state.db).await.unwrap();
    let error = crate::runner::apply_analysis_plan_locked(
        &state,
        &project,
        &project.goal,
        &[],
        "stale",
        "{}",
    )
    .await
    .unwrap_err();
    assert!(error.is::<crate::replan::StaleReplan>());
    let current = super::helpers::require_project(&state, "prj_test")
        .await
        .unwrap();
    assert_eq!(current.stage, STAGE_ANALYSIS);
    assert!(current.goal_locked_at.is_none());
    drop(_guard);
    state.db.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn late_workspace_failure_does_not_block_a_canceled_task_or_archived_project() {
    let (state, root) = verification_test_state().await;
    let project = super::helpers::require_project(&state, "prj_test")
        .await
        .unwrap();
    insert_running_analysis_job(&state).await;
    sqlx::query(
        "UPDATE jobs SET task_id = 'tsk_a', role = 'executor' WHERE id = 'job_analysis_running'",
    )
    .execute(&state.db)
    .await
    .unwrap();
    let job: JobRow = sqlx::query_as("SELECT * FROM jobs WHERE id = 'job_analysis_running'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET status = 'canceled' WHERE id = 'tsk_a'")
        .execute(&state.db)
        .await
        .unwrap();
    crate::resources::block_workspace(&state, &project, &job, "late failure")
        .await
        .unwrap();
    let current = super::helpers::require_project(&state, "prj_test")
        .await
        .unwrap();
    assert_eq!(current.status.as_deref(), Some(STATUS_RUNNING));
    assert!(current.blocked_reason.is_none());
    let task = super::helpers::load_project_task(&state, "prj_test", "tsk_a")
        .await
        .unwrap();
    assert_eq!(task.status.as_deref(), Some(STATUS_CANCELED));
    assert!(task.last_error.is_none());
    sqlx::query("UPDATE projects SET status = 'archived' WHERE id = 'prj_test'")
        .execute(&state.db)
        .await
        .unwrap();
    crate::resources::block_workspace(&state, &project, &job, "late failure")
        .await
        .unwrap();
    let current = super::helpers::require_project(&state, "prj_test")
        .await
        .unwrap();
    assert_eq!(current.status.as_deref(), Some(STATUS_ARCHIVED));
    assert!(current.blocked_reason.is_none());
    state.db.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn continuation_ledger_counts_only_unaccounted_usage() {
    let (state, root) = verification_test_state().await;
    sqlx::query("INSERT INTO job_runs(job_id, project_id, role, input_tokens, cost, duration_ms, usage_known, created_at, snapshot) VALUES('continued', 'prj_test', 'executor', 10, 0.25, 100, 1, '2026-01-01T00:00:00Z', '{}')")
        .execute(&state.db).await.unwrap();
    let current = crate::usage::UsageRecord {
        input_tokens: Some(50),
        cost: Some(0.75),
        duration_ms: Some(300),
        ..Default::default()
    };
    let delta = crate::continuation::usage_delta(&state, "continued", Some(current))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delta.input_tokens, Some(40));
    assert_eq!(delta.cost, Some(0.5));
    assert_eq!(delta.duration_ms, Some(200));
    assert_eq!(delta.output_tokens, None);
    assert!(crate::continuation::usage_delta(&state, "continued", None)
        .await
        .unwrap()
        .is_none());
    state.db.close().await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn resume_reservation_rejects_superseded_jobs_atomically() {
    let (state, root) = verification_test_state().await;
    for (id, status, timestamp) in [
        ("old_failure", JOB_STATUS_FAILED, "2026-01-01T00:00:01Z"),
        ("new_failure", JOB_STATUS_FAILED, "2026-01-01T00:00:01Z"),
    ] {
        insert_verification_job(
            &state,
            id,
            "tsk_a",
            ROLE_EXECUTOR,
            status,
            1,
            None,
            false,
            timestamp,
            None,
        )
        .await;
    }
    let mut conn = state.db.acquire().await.unwrap();
    assert!(!super::jobs::mark_job_resumed(&mut conn, "old_failure")
        .await
        .unwrap());
    sqlx::query("UPDATE jobs SET status = 'canceled', failure_kind = 'user_stopped' WHERE id = 'old_failure'")
        .execute(&mut *conn).await.unwrap();
    assert!(!super::jobs::mark_job_resumed(&mut conn, "old_failure")
        .await
        .unwrap());
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = 'old_failure'")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(status, JOB_STATUS_CANCELED);
    sqlx::query("UPDATE jobs SET error = 'current diagnostic' WHERE id = 'new_failure'")
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(super::jobs::mark_job_resumed(&mut conn, "new_failure")
        .await
        .unwrap());
    assert!(!super::jobs::mark_job_resumed(&mut conn, "new_failure")
        .await
        .unwrap());
    let error: Option<String> =
        sqlx::query_scalar("SELECT error FROM jobs WHERE id = 'new_failure'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(error.as_deref(), Some("current diagnostic"));
    drop(conn);
    state.db.close().await;
    let _ = std::fs::remove_dir_all(root);
}

#[allow(clippy::too_many_arguments)]
async fn insert_verification_job(
    state: &AppState,
    id: &str,
    task_id: &str,
    role: &str,
    status: &str,
    attempt: i64,
    result: Option<&str>,
    usage_known: bool,
    created_at: &str,
    agent_id: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO jobs (id, project_id, task_id, role, agent_id, status, attempt, prompt, \
         result, usage_known, started_at, finished_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, '', ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind("prj_test")
    .bind(task_id)
    .bind(role)
    .bind(agent_id)
    .bind(status)
    .bind(attempt)
    .bind(result)
    .bind(i64::from(usage_known))
    .bind(created_at)
    .bind(created_at)
    .bind(created_at)
    .execute(&state.db)
    .await
    .unwrap();
}

#[tokio::test]
async fn retry_target_requires_the_latest_role_job_to_be_failed_or_explicitly_stopped() {
    let (state, root) = verification_test_state().await;
    assert!(
        latest_failed_job(&state, "prj_test", Some("tsk_a"), ROLE_EXECUTOR)
            .await
            .unwrap()
            .is_none()
    );
    for (id, role, timestamp) in [
        ("job_old", ROLE_EXECUTOR, "2026-01-01T00:00:00Z"),
        ("job_latest", ROLE_EXECUTOR, "2026-01-01T00:00:01Z"),
        ("job_verifier", ROLE_VERIFIER, "2026-01-01T00:00:02Z"),
    ] {
        insert_verification_job(
            &state,
            id,
            "tsk_a",
            role,
            JOB_STATUS_FAILED,
            1,
            None,
            false,
            timestamp,
            Some("agt_verifier"),
        )
        .await;
    }
    for (status, reason, eligible) in [
        (JOB_STATUS_FAILED, None, true),
        (JOB_STATUS_SUCCEEDED, None, false),
        (JOB_STATUS_CANCELED, None, false),
        (JOB_STATUS_CANCELED, Some("user_stopped"), true),
        (JOB_STATUS_CANCELED, Some("other"), false),
        (JOB_STATUS_RUNNING, None, false),
    ] {
        sqlx::query("UPDATE jobs SET status = ?, failure_kind = ? WHERE id = 'job_latest'")
            .bind(status)
            .bind(reason)
            .execute(&state.db)
            .await
            .unwrap();
        let target = latest_failed_job(&state, "prj_test", Some("tsk_a"), ROLE_EXECUTOR)
            .await
            .unwrap();
        assert_eq!(
            target.as_ref().map(|job| job.id.as_str()),
            eligible.then_some("job_latest"),
            "latest status: {status}, reason: {reason:?}"
        );
    }
    state.db.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn verification_history_parses_orders_and_reports_unknown_results() {
    let (state, root) = verification_test_state().await;
    insert_verification_job(
        &state,
        "job_executor",
        "tsk_a",
        JOB_ROLE_EXECUTOR,
        JOB_STATUS_SUCCEEDED,
        1,
        Some(r#"{"kind":"done","summary":"执行完成"}"#),
        true,
        "2026-01-01T00:00:00Z",
        Some("agt_verifier"),
    )
    .await;
    insert_verification_job(
        &state,
        "job_v1",
        "tsk_a",
        JOB_ROLE_VERIFIER,
        JOB_STATUS_SUCCEEDED,
        1,
        Some(r#"{"pass":true,"reason":"符合验收标准","issues":[]}"#),
        true,
        "2026-01-01T00:00:01Z",
        Some("agt_verifier"),
    )
    .await;
    insert_verification_job(
        &state,
        "job_v2",
        "tsk_a",
        JOB_ROLE_VERIFIER,
        JOB_STATUS_SUCCEEDED,
        1,
        Some("不是 JSON"),
        false,
        "2026-01-01T00:00:03Z",
        Some("agt_verifier"),
    )
    .await;
    insert_verification_job(
        &state,
        "job_v3",
        "tsk_a",
        JOB_ROLE_VERIFIER,
        JOB_STATUS_FAILED,
        2,
        Some(r#"{"reason":"验收作业超时"}"#),
        false,
        "2026-01-01T00:00:04Z",
        Some("agt_verifier"),
    )
    .await;
    insert_verification_job(
        &state,
        "job_v4",
        "tsk_a",
        JOB_ROLE_VERIFIER,
        JOB_STATUS_SUCCEEDED,
        2,
        Some(r#"{"pass":false,"reason":"测试失败","issues":["a","b"],"next_action":"execute","guidance":"修复测试"}"#),
        true,
        "2026-01-01T00:00:05Z",
        None,
    )
    .await;
    insert_verification_job(
        &state,
        "job_v0",
        "tsk_a",
        JOB_ROLE_VERIFIER,
        JOB_STATUS_FAILED,
        1,
        None,
        false,
        "2025-12-31T23:59:59Z",
        Some("agt_verifier"),
    )
    .await;
    insert_verification_job(
        &state,
        "job_executor_2",
        "tsk_a",
        JOB_ROLE_EXECUTOR,
        JOB_STATUS_SUCCEEDED,
        2,
        Some(r#"{"kind":"done","summary":"再次执行完成"}"#),
        true,
        "2026-01-01T00:00:06Z",
        Some("agt_verifier"),
    )
    .await;
    insert_verification_job(
        &state,
        "job_v5",
        "tsk_a",
        JOB_ROLE_VERIFIER,
        JOB_STATUS_SUCCEEDED,
        3,
        Some(r#"{"pass":true,"reason":"第二轮通过","issues":[]}"#),
        true,
        "2026-01-01T00:00:07Z",
        Some("agt_verifier"),
    )
    .await;

    for (verifier, executor) in [
        ("job_v1", "job_executor"),
        ("job_v2", "job_executor"),
        ("job_v3", "job_executor"),
        ("job_v4", "job_executor"),
        ("job_v5", "job_executor_2"),
    ] {
        sqlx::query("UPDATE jobs SET execution_job_id = ? WHERE id = ?")
            .bind(executor)
            .bind(verifier)
            .execute(&state.db)
            .await
            .unwrap();
    }
    let response = task_verifications(
        State(state.clone()),
        Path(("prj_test".to_string(), "tsk_a".to_string())),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(response.total, 6);
    assert!(!response.truncated);
    let ids: Vec<&str> = response
        .verifications
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["job_v0", "job_v1", "job_v2", "job_v3", "job_v4", "job_v5"]
    );
    let rounds: Vec<i64> = response
        .verifications
        .iter()
        .map(|item| item.round)
        .collect();
    assert_eq!(rounds, vec![0, 1, 1, 1, 1, 2]);
    let attempts: Vec<i64> = response
        .verifications
        .iter()
        .map(|item| item.attempt)
        .collect();
    assert_eq!(attempts, vec![1, 1, 1, 2, 2, 3]);

    let unanchored = &response.verifications[0];
    assert_eq!(unanchored.round, 0);
    assert_eq!(unanchored.status, JOB_STATUS_FAILED);
    assert!(!unanchored.parsed);

    let passed = &response.verifications[1];
    assert_eq!(passed.passed, Some(true));
    assert_eq!(passed.reason.as_deref(), Some("符合验收标准"));
    assert!(passed.parsed);
    assert!(passed.parse_error.is_none());
    assert_eq!(passed.agent_name.as_deref(), Some("验收 Agent"));
    assert!(passed.usage_known);

    let unparsed = &response.verifications[2];
    assert_eq!(unparsed.passed, None);
    assert!(!unparsed.parsed);
    assert_eq!(unparsed.raw_excerpt.as_deref(), Some("不是 JSON"));
    assert!(unparsed
        .parse_error
        .as_deref()
        .unwrap_or_default()
        .starts_with("无法解析"));
    assert!(!unparsed.usage_known);
    assert_eq!(unparsed.agent_name.as_deref(), Some("验收 Agent"));

    let missing_pass = &response.verifications[3];
    assert_eq!(missing_pass.status, JOB_STATUS_FAILED);
    assert!(!missing_pass.parsed);
    assert_eq!(
        missing_pass.raw_excerpt.as_deref(),
        Some(r#"{"reason":"验收作业超时"}"#)
    );

    let rejected = &response.verifications[4];
    assert_eq!(rejected.passed, Some(false));
    assert_eq!(rejected.reason.as_deref(), Some("测试失败"));
    assert_eq!(rejected.issues, vec!["a", "b"]);
    assert_eq!(rejected.agent_name, None);

    let second_round = &response.verifications[5];
    assert_eq!(second_round.round, 2);
    assert_eq!(second_round.passed, Some(true));
    assert_eq!(second_round.reason.as_deref(), Some("第二轮通过"));

    let empty = task_verifications(
        State(state.clone()),
        Path(("prj_test".to_string(), "tsk_empty".to_string())),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(empty.total, 0);
    assert!(empty.verifications.is_empty());

    let missing = task_verifications(
        State(state.clone()),
        Path(("prj_test".to_string(), "tsk_missing".to_string())),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn verification_history_keeps_the_newest_entries_and_marks_truncation() {
    let (state, root) = verification_test_state().await;
    for (index, created_at) in [
        (1, "2025-12-31T23:00:00Z"),
        (2, "2026-01-01T00:20:00Z"),
        (3, "2026-01-01T00:40:00Z"),
    ] {
        insert_verification_job(
            &state,
            &format!("job_x{index}"),
            "tsk_a",
            JOB_ROLE_EXECUTOR,
            JOB_STATUS_SUCCEEDED,
            index,
            Some(r#"{"kind":"done","summary":"执行完成"}"#),
            true,
            created_at,
            None,
        )
        .await;
    }
    for index in 1..=55 {
        insert_verification_job(
            &state,
            &format!("job_{index:02}"),
            "tsk_a",
            JOB_ROLE_VERIFIER,
            JOB_STATUS_SUCCEEDED,
            index,
            None,
            false,
            &format!("2026-01-01T00:{index:02}:00Z"),
            None,
        )
        .await;
        let execution = if index < 20 {
            "job_x1"
        } else if index < 40 {
            "job_x2"
        } else {
            "job_x3"
        };
        sqlx::query("UPDATE jobs SET execution_job_id = ? WHERE id = ?")
            .bind(execution)
            .bind(format!("job_{index:02}"))
            .execute(&state.db)
            .await
            .unwrap();
    }

    let response = task_verifications(
        State(state.clone()),
        Path(("prj_test".to_string(), "tsk_a".to_string())),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(response.total, 55);
    assert_eq!(response.limit, VERIFICATION_HISTORY_LIMIT);
    assert!(response.truncated);
    assert_eq!(
        response.verifications.len(),
        VERIFICATION_HISTORY_LIMIT as usize
    );
    assert_eq!(response.verifications[0].id, "job_06");
    assert_eq!(response.verifications[0].round, 1);
    assert_eq!(response.verifications[13].id, "job_19");
    assert_eq!(response.verifications[13].round, 1);
    assert_eq!(response.verifications[14].id, "job_20");
    assert_eq!(response.verifications[14].round, 2);
    assert_eq!(response.verifications[33].id, "job_39");
    assert_eq!(response.verifications[33].round, 2);
    assert_eq!(response.verifications[34].id, "job_40");
    assert_eq!(response.verifications[34].round, 3);
    assert_eq!(response.verifications[49].id, "job_55");
    assert_eq!(response.verifications[49].round, 3);
    let rounds: Vec<i64> = response
        .verifications
        .iter()
        .map(|item| item.round)
        .collect();
    assert!(rounds.iter().all(|round| (1..=3).contains(round)));
    assert!(rounds.windows(2).all(|pair| pair[0] <= pair[1]));

    let _ = std::fs::remove_dir_all(root);
}

async fn set_project_analysis_state(state: &AppState, status: &str) {
    sqlx::query("UPDATE projects SET stage = ?, status = ? WHERE id = ?")
        .bind(STAGE_ANALYSIS)
        .bind(status)
        .bind("prj_test")
        .execute(&state.db)
        .await
        .unwrap();
}

async fn insert_running_analysis_job(state: &AppState) {
    let timestamp = "2026-01-01T00:00:00Z";
    sqlx::query(
        "INSERT INTO jobs (id, project_id, task_id, role, agent_id, status, attempt, prompt, \
         result, usage_known, started_at, finished_at, created_at) \
         VALUES (?, ?, NULL, ?, NULL, ?, 1, '', NULL, 0, ?, NULL, ?)",
    )
    .bind("job_analysis_running")
    .bind("prj_test")
    .bind(JOB_ROLE_ANALYSIS)
    .bind(JOB_STATUS_RUNNING)
    .bind(timestamp)
    .bind(timestamp)
    .execute(&state.db)
    .await
    .unwrap();
}

#[tokio::test]
async fn analysis_policy_updates_while_analysis_job_is_running() {
    let (state, root) = verification_test_state().await;
    set_project_analysis_state(&state, STATUS_RUNNING).await;
    insert_running_analysis_job(&state).await;

    let detail = update_analysis_policy(
        State(state.clone()),
        Path("prj_test".to_string()),
        Json(AnalysisPolicyRequest {
            mode: CONFIRM_AUTO.into(),
        }),
    )
    .await
    .expect("分析作业运行中应允许调整确认方式")
    .0;
    assert_eq!(detail.project.confirm_mode, CONFIRM_AUTO);
    let stored: String = sqlx::query_scalar("SELECT confirm_mode FROM projects WHERE id = ?")
        .bind("prj_test")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(stored, CONFIRM_AUTO);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn analysis_policy_rejects_paused_projects() {
    for status in [STATUS_PAUSED, STATUS_PAUSING] {
        let (state, root) = verification_test_state().await;
        set_project_analysis_state(&state, status).await;

        let error = update_analysis_policy(
            State(state.clone()),
            Path("prj_test".to_string()),
            Json(AnalysisPolicyRequest {
                mode: CONFIRM_AUTO.into(),
            }),
        )
        .await
        .expect_err("暂停中的项目应拒绝调整确认方式")
        .message;
        assert_eq!(error, "项目已暂停，请先恢复");

        let stored: String = sqlx::query_scalar("SELECT confirm_mode FROM projects WHERE id = ?")
            .bind("prj_test")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(stored, CONFIRM_MANUAL);

        let _ = std::fs::remove_dir_all(root);
    }
}

#[tokio::test]
async fn verification_link_is_authoritative_even_when_timestamps_disagree() {
    let (state, root) = verification_test_state().await;
    for (id, role, created_at) in [
        ("execution", JOB_ROLE_EXECUTOR, "2026-01-02T00:00:00Z"),
        ("verification", JOB_ROLE_VERIFIER, "2026-01-01T00:00:00Z"),
    ] {
        insert_verification_job(
            &state,
            id,
            "tsk_a",
            role,
            JOB_STATUS_SUCCEEDED,
            1,
            None,
            false,
            created_at,
            None,
        )
        .await;
    }
    sqlx::query("UPDATE jobs SET execution_job_id = 'execution' WHERE id = 'verification'")
        .execute(&state.db)
        .await
        .unwrap();
    let response = task_verifications(
        State(state.clone()),
        Path(("prj_test".into(), "tsk_a".into())),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(
        response.verifications[0].execution_job_id.as_deref(),
        Some("execution")
    );
    assert_eq!(response.verifications[0].round, 1);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn empty_task_pool_is_not_repopulated_by_reads() {
    let (state, root) = verification_test_state().await;
    crate::db::ensure_project_agents(&state.db, "prj_test")
        .await
        .unwrap();
    let mut tx = state.db.begin().await.unwrap();
    crate::db::initialize_task_agents_tx(&mut tx, "tsk_a")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let initial = crate::db::load_task_agent_pool(&state.db, "prj_test", "tsk_a")
        .await
        .unwrap();
    assert!(initial.roles.iter().any(|role| !role.entries.is_empty()));
    sqlx::query("DELETE FROM task_agents WHERE task_id = 'tsk_a'")
        .execute(&state.db)
        .await
        .unwrap();
    for _ in 0..2 {
        let empty = crate::db::load_task_agent_pool(&state.db, "prj_test", "tsk_a")
            .await
            .unwrap();
        assert!(empty.roles.iter().all(|role| role.entries.is_empty()));
    }
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn running_project_exposes_the_same_task_attention_in_list_and_detail() {
    let (state, root) = verification_test_state().await;
    sqlx::query(
        "UPDATE tasks SET stage = 'verifying', status = 'blocked', human_gate = 'milestone', \
        last_error = '等待人工验收' WHERE id = 'tsk_a'",
    )
    .execute(&state.db)
    .await
    .unwrap();
    let detail = super::detail::load_detail(&state, "prj_test")
        .await
        .unwrap();
    let list = super::projects::list_projects(State(state.clone()))
        .await
        .unwrap()
        .0;
    assert_eq!(detail.project.status.as_deref(), Some(STATUS_RUNNING));
    assert_eq!(detail.attention_items.len(), 1);
    let item = &detail.attention_items[0];
    assert!(item.project_advancing);
    assert_eq!(item.kind, ProjectAttention::Review);
    assert_eq!(item.task.as_ref().unwrap().id, "tsk_a");
    assert_eq!(
        serde_json::to_value(&detail.attention_items).unwrap(),
        serde_json::to_value(&list[0].attention_items).unwrap()
    );
    let _ = std::fs::remove_dir_all(root);
}
