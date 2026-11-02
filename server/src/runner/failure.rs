use anyhow::Result;

use crate::domain::{self, now, FailureKind};
use crate::models::*;
use crate::state::AppState;

use super::common::parse_json_list;

/// A missing result is terminal after its continuation budget. A malformed
/// result can be corrected on retry and stays within the task attempt budget.
pub(super) fn protocol_failure_blocks(reason: &str) -> bool {
    reason.contains("没有写 result.json")
}

pub(super) struct AgentFailureEffect {
    pub(super) switch: bool,
}

pub(super) async fn apply_agent_failure(
    state: &AppState,
    agent: &AgentRow,
    reason: &str,
    kind: FailureKind,
) -> Result<AgentFailureEffect> {
    let mut switch = false;
    let mut transient_failures = agent.transient_failures;
    let mut cooldown: Option<String> = None;
    match kind {
        FailureKind::RateLimit => {
            let until = chrono::Utc::now() + domain::rate_limit_cooldown(reason);
            cooldown = Some(until.to_rfc3339());
            transient_failures = 0;
            switch = true;
        }
        FailureKind::Transient => {
            let next = transient_failures + 1;
            transient_failures = next;
            if next >= domain::transient_retry_limit() {
                cooldown = Some((chrono::Utc::now() + chrono::Duration::minutes(30)).to_rfc3339());
                switch = true;
            }
        }
        FailureKind::Permanent => {
            transient_failures = 0;
            switch = true;
        }
        FailureKind::Protocol => {
            transient_failures = 0;
        }
        FailureKind::Task => {}
    }
    sqlx::query(
        "UPDATE agents SET last_error = ?, failure_kind = COALESCE(?, failure_kind), \
         transient_failures = ?, cooldown_until = COALESCE(?, cooldown_until), updated_at = ? \
         WHERE id = ?",
    )
    .bind(reason)
    .bind(kind.is_agent_failure().then(|| kind.as_str()))
    .bind(transient_failures)
    .bind(cooldown)
    .bind(now())
    .bind(&agent.id)
    .execute(&state.db)
    .await?;
    Ok(AgentFailureEffect { switch })
}

/// Failure handling shared by project jobs. Their skip state lives on the
/// project row instead of a task, and there is no attempt budget.
pub(super) struct ProjectFailure {
    kind: FailureKind,
    pub(super) switch: bool,
    blocked: bool,
    pub(super) blocked_reason: Option<String>,
}

impl ProjectFailure {
    pub(super) fn status(&self) -> &'static str {
        if self.blocked {
            STATUS_BLOCKED
        } else {
            STATUS_RUNNING
        }
    }

    pub(super) fn label(&self) -> String {
        let kind = failure_kind_label(self.kind);
        if self.blocked && self.kind == FailureKind::Protocol {
            format!("失败（{kind}），等待人工处理")
        } else if self.blocked {
            format!("失败（{kind}，没有其他可用 Agent），等待人工处理")
        } else if self.switch {
            format!("失败（{kind}），已切换 Agent 重试")
        } else {
            format!("失败（{kind}），将在原 Agent 上重试")
        }
    }

    pub(super) fn event_kind(&self, failed: &str) -> &'static str {
        if self.blocked {
            "project_blocked"
        } else if self.switch {
            "project_retry"
        } else {
            match failed {
                "direct_failed" => "direct_failed",
                "summary_failed" => "summary_retry",
                "acceptance_failed" => "acceptance_retry",
                _ => "analysis_retry",
            }
        }
    }
}

pub(super) async fn plan_project_failure(
    state: &AppState,
    project: &ProjectRow,
    reason: &str,
    agent: Option<(&AgentRow, bool)>,
) -> Result<ProjectFailure> {
    let Some((agent, process_failure)) = agent else {
        return Ok(ProjectFailure {
            kind: FailureKind::Task,
            switch: false,
            blocked: true,
            blocked_reason: Some(reason.to_string()),
        });
    };
    let kind = domain::classify_failure(reason, process_failure);
    let effect = apply_agent_failure(state, agent, reason, kind).await?;
    // Project jobs (analysis, direct, project acceptance and summary) all
    // dispatch from the planner group of the project pool.
    let blocked = kind == FailureKind::Protocol
        || (kind.is_agent_failure()
            && effect.switch
            && !has_usable_agent(state, &project.id, ROLE_PLANNER, Some(&agent.id)).await?);
    Ok(ProjectFailure {
        kind,
        switch: effect.switch,
        blocked,
        blocked_reason: blocked.then(|| reason.to_string()),
    })
}

pub(super) async fn has_usable_agent(
    state: &AppState,
    project_id: &str,
    role: &str,
    exclude: Option<&str>,
) -> Result<bool> {
    crate::db::ensure_project_agents(&state.db, project_id).await?;
    let rows = sqlx::query_as::<_, ProjectAgentRow>(
        "SELECT * FROM project_agents WHERE project_id = ? AND role = ? AND enabled = 1",
    )
    .bind(project_id)
    .bind(role)
    .fetch_all(&state.db)
    .await?;
    let agents = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents")
        .fetch_all(&state.db)
        .await?;
    let now = chrono::Utc::now();
    for row in rows {
        if exclude == Some(row.agent_id.as_str()) {
            continue;
        }
        let Some(agent) = agents.iter().find(|agent| agent.id == row.agent_id) else {
            continue;
        };
        if agent.enabled == 0 {
            continue;
        }
        if !parse_json_list(&agent.roles)
            .iter()
            .any(|item| item == role)
        {
            continue;
        }
        let cooling = agent
            .cooldown_until
            .as_deref()
            .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
            .map(|until| until.with_timezone(&chrono::Utc) > now)
            .unwrap_or(false);
        if cooling {
            continue;
        }
        if crate::usage::agent_gate(
            &state.db,
            &agent.id,
            agent.availability_json.as_deref(),
            agent.usage_limit_json.as_deref(),
            agent.usage_reset_at.as_deref(),
            now,
        )
        .await?
        .is_some()
        {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

/// A graceful task pause only takes effect when the in-flight job ends, so
/// transitions that would normally enqueue the next stage land in paused.
pub(super) async fn task_pause_intent(
    state: &AppState,
    task_id: &str,
) -> Result<Option<&'static str>> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM tasks WHERE id = ?")
        .bind(task_id)
        .fetch_optional(&state.db)
        .await?;
    Ok(match status.as_deref() {
        Some(STATUS_PAUSING) | Some(STATUS_PAUSED) => Some(STATUS_PAUSED),
        _ => None,
    })
}

pub(super) fn failure_kind_label(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::RateLimit => "限流",
        FailureKind::Transient => "供应商波动",
        FailureKind::Permanent => "永久错误",
        FailureKind::Protocol => "协议失败",
        FailureKind::Task => "任务失败",
    }
}
