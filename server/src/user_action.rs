//! User-facing next steps are separate from agent guidance and audit evidence.
//! Notices are read from their source results, and only exposed while that result
//! still owns the current blockage. Legacy results are never guessed or rewritten.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::{ProjectRow, TaskRow, STATUS_BLOCKED};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserAction {
    pub summary: String,
    pub why: String,
    pub owner: String,
    pub steps: Vec<String>,
    pub after: String,
}

pub fn parse(value: &Value) -> Result<Option<UserAction>, String> {
    let Some(value) = value.get("user_action").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let mut action: UserAction = serde_json::from_value(value.clone()).map_err(|_| {
        "user_action 必须包含 summary、why、owner、steps 字符串数组和 after".to_string()
    })?;
    for text in [
        &mut action.summary,
        &mut action.why,
        &mut action.owner,
        &mut action.after,
    ] {
        *text = text.trim().to_string();
        if text.is_empty() {
            return Err("user_action 的 summary、why、owner、after 不能为空".into());
        }
    }
    if action.summary.chars().count() > 120 || action.summary.contains(['\n', '\r']) {
        return Err("user_action.summary 必须是一行不超过 120 字的简明说明".into());
    }
    if action.steps.is_empty() || action.steps.len() > 8 {
        return Err("user_action.steps 必须包含 1–8 个具体操作".into());
    }
    for step in &mut action.steps {
        *step = step.trim().to_string();
        if step.is_empty() {
            return Err("user_action.steps 不能包含空操作".into());
        }
    }
    Ok(Some(action))
}

/// New agent results must explain human-facing waits. Historical audit reads
/// still use the ordinary parser, so an absent notice cannot rewrite history.
pub fn validate_live(role: &str, raw: &str) -> Result<(), String> {
    let value = crate::domain::extract_json(raw).ok_or("结果不是合法 JSON")?;
    let required = match role {
        "replan_review" => value["decision"] == "wait",
        "verifier" => value["next_action"] == "wait",
        "project_acceptance" => matches!(value["next_action"].as_str(), Some("wait" | "arbitrate")),
        _ => false,
    };
    if required && parse(&value)?.is_none() {
        return Err("等待用户或外部条件时必须填写 user_action（summary、why、owner、steps、after），分别说明缺项、影响、处理方、具体操作和完成后如何继续；不要复制 Agent 技术指导".into());
    }
    Ok(())
}

pub fn review_note(parent: bool, reason: &str, guidance: &str) -> String {
    format!(
        "{}决策：{reason}\n执行指导：{guidance}",
        if parent { "父任务" } else { "规划者" }
    )
}

pub fn for_project(project: &ProjectRow) -> Option<UserAction> {
    if project.status.as_deref() != Some(STATUS_BLOCKED)
        || project.stage != "summarizing"
        || project.replan_active != 0
        || project.acceptance_passed != 0
        || project.report.is_some()
    {
        return None;
    }
    let value: Value = serde_json::from_str(project.acceptance.as_deref()?).ok()?;
    if !matches!(value["next_action"].as_str(), Some("wait" | "arbitrate")) {
        return None;
    }
    let action = parse(&value).ok().flatten()?;
    (project.blocked_reason.as_deref() == Some(action.summary.as_str())).then_some(action)
}

pub async fn for_task(
    pool: &sqlx::SqlitePool,
    task: &TaskRow,
) -> Result<Option<UserAction>, sqlx::Error> {
    Ok(task_explanation(pool, task)
        .await?
        .map(|explanation| explanation.action))
}

pub struct TaskExplanation {
    pub action: UserAction,
    pub reason: String,
    pub guidance: String,
}

fn explanation(value: &Value) -> Option<TaskExplanation> {
    Some(TaskExplanation {
        action: parse(value).ok().flatten()?,
        reason: value["reason"].as_str().unwrap_or_default().to_string(),
        guidance: value["guidance"].as_str().unwrap_or_default().to_string(),
    })
}

pub async fn task_explanation(
    pool: &sqlx::SqlitePool,
    task: &TaskRow,
) -> Result<Option<TaskExplanation>, sqlx::Error> {
    if task.status.as_deref() != Some(STATUS_BLOCKED)
        || task.stage == "completed"
        || task.human_gate.is_some()
    {
        return Ok(None);
    }
    // A waiting owner decision takes precedence over the executor that requested it.
    let review: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT r.guidance, j.result FROM replan_requests r JOIN jobs j ON j.id = r.review_job_id \
         WHERE r.task_id = ? AND r.state = 'waiting' AND j.status = 'succeeded'",
    )
    .bind(&task.id)
    .fetch_optional(pool)
    .await?;
    if let Some((note, raw)) = review {
        if task.last_error.as_deref() == Some(note.as_str()) {
            return Ok(raw
                .and_then(|raw| crate::domain::extract_json(&raw))
                .and_then(|value| explanation(&value)));
        }
        return Ok(None);
    }
    let job: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT role, status, result FROM jobs WHERE task_id = ? ORDER BY rowid DESC LIMIT 1",
    )
    .bind(&task.id)
    .fetch_optional(pool)
    .await?;
    let Some((role, status, Some(raw))) = job else {
        return Ok(None);
    };
    if role != "verifier" || status != "succeeded" {
        return Ok(None);
    }
    let Some(value) = crate::domain::extract_json(&raw) else {
        return Ok(None);
    };
    let Ok(crate::domain::ParsedResult::Verdict {
        pass: false,
        reason,
        action,
        ..
    }) = crate::domain::parse_result(&role, &raw)
    else {
        return Ok(None);
    };
    if action.next_action != "wait" {
        return Ok(None);
    }
    let note = format!("{reason}\n处理指导：{}", action.guidance);
    if task.last_error.as_deref() != Some(note.as_str()) {
        return Ok(None);
    }
    Ok(explanation(&value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_actionable_content_without_rewriting_legacy_results() {
        assert_eq!(
            parse(&json!({"reason": "legacy technical report"})).unwrap(),
            None
        );
        let valid = json!({"user_action": {
            "summary": "请补充原始销售数据", "why": "缺少原始记录，无法核对汇总结果。",
            "owner": "数据提供方", "steps": ["上传所需月份的原始 CSV，并告知路径。"],
            "after": "在任务中记录数据路径并继续；AI 将核验数据和重新验收。"
        }});
        assert_eq!(parse(&valid).unwrap().unwrap().steps.len(), 1);
        for (role, mut value) in [
            ("replan_review", json!({"decision":"wait"})),
            ("verifier", json!({"next_action":"wait"})),
            ("project_acceptance", json!({"next_action":"arbitrate"})),
        ] {
            assert!(validate_live(role, &value.to_string()).is_err());
            value["user_action"] = valid["user_action"].clone();
            validate_live(role, &value.to_string()).unwrap();
        }
        for invalid in [
            json!({}),
            json!({"summary": "空操作", "why": "原因", "owner": "用户", "steps": [], "after": "继续"}),
            json!({"summary": "多行\n说明", "why": "原因", "owner": "用户", "steps": ["操作"], "after": "继续"}),
        ] {
            assert!(parse(&json!({"user_action": invalid})).is_err());
        }
    }
}
