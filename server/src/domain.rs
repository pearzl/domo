//! Domain rules: identifiers, DAG validation, and the JSON protocol used between
//! DOMO and agent jobs.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

use crate::models::{
    parse_string_list, JOB_ROLE_ANALYSIS, JOB_ROLE_DIRECT, JOB_ROLE_SUMMARY, ROLE_EXECUTOR,
    ROLE_VERIFIER,
};

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

pub fn short_id(id: &str) -> String {
    id.rsplit('_')
        .next()
        .unwrap_or(id)
        .chars()
        .take(8)
        .collect()
}

pub fn slug(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "task".into()
    } else {
        trimmed.chars().take(40).collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlanTask {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct NormalizedTask {
    pub key: String,
    pub title: String,
    pub description: String,
    pub acceptance: Vec<String>,
    pub deps: Vec<String>,
}

pub const MAX_DERIVED_TASKS: usize = 8;

#[derive(Debug, Clone)]
pub enum ParsedResult {
    Questions {
        questions: Vec<String>,
        summary: String,
    },
    AnalysisPlan {
        summary: String,
        goal: Option<String>,
        assumptions: Vec<String>,
        acceptance: Vec<String>,
        tasks: Vec<NormalizedTask>,
        skip_confirmation: bool,
    },
    AnalysisDirect {
        summary: String,
        goal: Option<String>,
        plan: Vec<String>,
        verification: Vec<String>,
        skip_confirmation: bool,
    },
    DirectDone {
        summary: String,
        report: String,
    },
    ExecutorDone {
        summary: String,
    },
    ExecutorFailed {
        reason: String,
    },
    ExecutorReplan {
        reason: String,
    },
    ExecutorDerive {
        summary: String,
        tasks: Vec<NormalizedTask>,
    },
    Verdict {
        pass: bool,
        reason: String,
        issues: Vec<String>,
    },
    Report {
        summary: String,
    },
}

pub fn extract_json(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Some(value);
    }
    if let Some(start) = trimmed.rfind("```json") {
        let rest = &trimmed[start + 7..];
        if let Some(end) = rest.find("```") {
            if let Ok(value) = serde_json::from_str::<Value>(rest[..end].trim()) {
                return Some(value);
            }
        }
    }
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                if let Ok(value) = serde_json::from_str::<Value>(&trimmed[start..=end]) {
                    return Some(value);
                }
            }
        }
    }
    None
}

pub fn parse_result(role: &str, raw: &str) -> Result<ParsedResult, String> {
    let value = extract_json(raw).ok_or_else(|| "结果不是合法 JSON".to_string())?;
    match role {
        JOB_ROLE_ANALYSIS => parse_analysis_result(&value),
        JOB_ROLE_DIRECT => parse_direct_result(&value),
        ROLE_EXECUTOR => parse_executor_result(&value),
        ROLE_VERIFIER => parse_verifier_result(&value),
        JOB_ROLE_SUMMARY => parse_report_result(&value),
        other => Err(format!("未知作业角色: {other}")),
    }
}

fn parse_analysis_result(value: &Value) -> Result<ParsedResult, String> {
    let kind = value
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let summary = string_field(value, "summary").unwrap_or_default();
    let questions = string_array(value.get("questions"));
    let has_tasks = value.get("tasks").is_some();
    if kind == "questions" || (!questions.is_empty() && !has_tasks) {
        if questions.is_empty() {
            return Err("项目分析结果缺少 questions 数组".to_string());
        }
        return Ok(ParsedResult::Questions { questions, summary });
    }
    let skip_confirmation = value
        .get("skip_confirmation")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let goal = string_field(value, "goal");
    if kind == "direct" {
        return Ok(ParsedResult::AnalysisDirect {
            summary,
            goal,
            plan: string_array(value.get("plan")),
            verification: string_array(value.get("verification")),
            skip_confirmation,
        });
    }
    if !matches!(kind, "" | "plan") {
        return Err(format!("未知项目分析结果类型: {kind}"));
    }
    let tasks_value = value
        .get("tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "项目分析结果缺少 tasks 数组".to_string())?;
    let mut raw_tasks = Vec::new();
    for item in tasks_value {
        raw_tasks.push(PlanTask {
            key: string_field(item, "key").unwrap_or_default(),
            title: string_field(item, "title").unwrap_or_default(),
            description: string_field(item, "description").unwrap_or_default(),
            acceptance: string_array(item.get("acceptance")),
            deps: string_array(item.get("deps").or_else(|| item.get("depends_on"))),
        });
    }
    let tasks = validate_plan(raw_tasks)?;
    Ok(ParsedResult::AnalysisPlan {
        summary,
        goal,
        assumptions: string_array(value.get("assumptions")),
        acceptance: string_array(value.get("acceptance")),
        tasks,
        skip_confirmation,
    })
}

fn parse_direct_result(value: &Value) -> Result<ParsedResult, String> {
    let kind = value
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !matches!(kind, "direct_done" | "report" | "done") {
        return Err(format!("直接完成结果类型不合法: {kind}"));
    }
    let report =
        string_field(value, "report").ok_or_else(|| "直接完成结果缺少 report".to_string())?;
    let summary = string_field(value, "summary").unwrap_or_else(|| "直接完成".into());
    Ok(ParsedResult::DirectDone { summary, report })
}

fn parse_executor_result(value: &Value) -> Result<ParsedResult, String> {
    let kind = value.get("kind").and_then(|v| v.as_str());
    let reason = string_field(value, "reason")
        .or_else(|| string_field(value, "summary"))
        .or_else(|| string_field(value, "error"))
        .unwrap_or_default();
    if kind == Some("replan") {
        return Ok(ParsedResult::ExecutorReplan {
            reason: if reason.is_empty() {
                "执行者请求重新规划".into()
            } else {
                reason
            },
        });
    }
    if kind == Some("derive") {
        let tasks_value = value
            .get("tasks")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "派生结果缺少 tasks 数组".to_string())?;
        let mut raw_tasks = Vec::new();
        for item in tasks_value {
            raw_tasks.push(PlanTask {
                key: string_field(item, "key").unwrap_or_default(),
                title: string_field(item, "title").unwrap_or_default(),
                description: string_field(item, "description").unwrap_or_default(),
                acceptance: string_array(item.get("acceptance")),
                deps: string_array(item.get("deps").or_else(|| item.get("depends_on"))),
            });
        }
        let tasks = validate_derive(raw_tasks)?;
        return Ok(ParsedResult::ExecutorDerive {
            summary: if reason.is_empty() {
                "执行者请求派生子任务".into()
            } else {
                reason
            },
            tasks,
        });
    }
    let failed = value
        .get("failed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || kind
            .map(|v| v == "failed" || v == "blocked")
            .unwrap_or(false);
    if failed {
        return Ok(ParsedResult::ExecutorFailed {
            reason: if reason.is_empty() {
                "执行者报告失败".into()
            } else {
                reason
            },
        });
    }
    let summary = string_field(value, "summary")
        .or_else(|| string_field(value, "result"))
        .unwrap_or_else(|| "执行完成".into());
    Ok(ParsedResult::ExecutorDone { summary })
}

fn parse_verifier_result(value: &Value) -> Result<ParsedResult, String> {
    let pass = value
        .get("pass")
        .and_then(|v| v.as_bool())
        .or_else(|| {
            value
                .get("verdict")
                .and_then(|v| v.as_str())
                .map(|v| v == "pass" || v == "passed")
        })
        .ok_or_else(|| "验收结果缺少 pass 布尔值".to_string())?;
    let reason = string_field(value, "reason")
        .or_else(|| string_field(value, "summary"))
        .unwrap_or_else(|| {
            if pass {
                "验收通过".into()
            } else {
                "验收未通过".into()
            }
        });
    let mut issues = string_array(value.get("issues"));
    if issues.is_empty() && !pass && !reason.is_empty() {
        issues.push(reason.clone());
    }
    Ok(ParsedResult::Verdict {
        pass,
        reason,
        issues,
    })
}

fn parse_report_result(value: &Value) -> Result<ParsedResult, String> {
    let summary = string_field(value, "report")
        .or_else(|| string_field(value, "summary"))
        .ok_or_else(|| "总结结果缺少 report 内容".to_string())?;
    Ok(ParsedResult::Report { summary })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => {
                    let text = text.trim();
                    (!text.is_empty()).then(|| text.to_string())
                }
                other => Some(other.to_string()),
            })
            .collect(),
        Some(Value::String(text)) => parse_string_list(text),
        _ => Vec::new(),
    }
}

pub fn validate_plan(raw_tasks: Vec<PlanTask>) -> Result<Vec<NormalizedTask>, String> {
    if raw_tasks.is_empty() {
        return Err("规划结果没有任何任务".into());
    }
    if raw_tasks.len() > 20 {
        return Err("单个项目最多支持 20 个任务".into());
    }
    let mut tasks = Vec::new();
    let mut used_keys = HashSet::new();
    for (index, raw) in raw_tasks.into_iter().enumerate() {
        let mut key = raw.key.trim().to_string();
        if key.is_empty() {
            key = format!("T{}", index + 1);
        }
        if key.len() > 32 {
            return Err(format!("任务 key 过长: {key}"));
        }
        if !used_keys.insert(key.clone()) {
            return Err(format!("任务 key 重复: {key}"));
        }
        let title = raw.title.trim().to_string();
        if title.is_empty() {
            return Err(format!("任务 {key} 缺少标题"));
        }
        let description = raw.description.trim().to_string();
        if description.is_empty() {
            return Err(format!("任务 {key} 缺少描述"));
        }
        let mut acceptance: Vec<String> = raw
            .acceptance
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
        if acceptance.is_empty() {
            acceptance.push("实现任务描述中的要求，并通过项目已有测试".into());
        }
        let deps: Vec<String> = raw
            .deps
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
        tasks.push(NormalizedTask {
            key,
            title,
            description,
            acceptance,
            deps,
        });
    }
    let keys: HashSet<String> = tasks.iter().map(|task| task.key.clone()).collect();
    for task in &tasks {
        for dep in &task.deps {
            if dep == &task.key {
                return Err(format!("任务 {} 不能依赖自身", task.key));
            }
            if !keys.contains(dep) {
                return Err(format!("任务 {} 依赖了不存在的任务 {}", task.key, dep));
            }
        }
    }
    detect_cycle(&tasks)?;
    Ok(tasks)
}

pub fn validate_derive(raw_tasks: Vec<PlanTask>) -> Result<Vec<NormalizedTask>, String> {
    if raw_tasks.is_empty() {
        return Err("派生请求至少需要 1 个子任务".into());
    }
    if raw_tasks.len() > MAX_DERIVED_TASKS {
        return Err(format!("单次派生最多支持 {MAX_DERIVED_TASKS} 个子任务"));
    }
    let mut tasks = Vec::new();
    let mut used_keys = HashSet::new();
    for raw in raw_tasks {
        let key = raw.key.trim().to_string();
        if key.is_empty() {
            return Err("派生任务缺少 key".into());
        }
        if key.len() > 32 {
            return Err(format!("派生任务 key 过长: {key}"));
        }
        if !used_keys.insert(key.clone()) {
            return Err(format!("派生任务 key 重复: {key}"));
        }
        let title = raw.title.trim().to_string();
        if title.is_empty() {
            return Err(format!("派生任务 {key} 缺少标题"));
        }
        let description = raw.description.trim().to_string();
        if description.is_empty() {
            return Err(format!("派生任务 {key} 缺少描述"));
        }
        let acceptance: Vec<String> = raw
            .acceptance
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
        if acceptance.is_empty() {
            return Err(format!("派生任务 {key} 缺少验收标准"));
        }
        let deps: Vec<String> = raw
            .deps
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
        if !deps.is_empty() {
            return Err(format!("派生任务 {key} 的 deps 必须为空"));
        }
        tasks.push(NormalizedTask {
            key,
            title,
            description,
            acceptance,
            deps,
        });
    }
    Ok(tasks)
}

pub fn validate_derive_scope(
    parent_is_derived: bool,
    active_keys: &HashSet<String>,
    tasks: &[NormalizedTask],
) -> Result<(), String> {
    if parent_is_derived {
        return Err("派生任务不能再次派生".into());
    }
    if tasks.is_empty() {
        return Err("派生请求至少需要 1 个子任务".into());
    }
    if tasks.len() > MAX_DERIVED_TASKS {
        return Err(format!("单次派生最多支持 {MAX_DERIVED_TASKS} 个子任务"));
    }
    let mut seen = HashSet::new();
    for task in tasks {
        if !task.deps.is_empty() {
            return Err(format!("派生任务 {} 的 deps 必须为空", task.key));
        }
        if task.description.trim().is_empty() {
            return Err(format!("派生任务 {} 缺少描述", task.key));
        }
        if task.acceptance.is_empty() {
            return Err(format!("派生任务 {} 缺少验收标准", task.key));
        }
        if active_keys.contains(&task.key) {
            return Err(format!("派生任务 key 已存在: {}", task.key));
        }
        if !seen.insert(task.key.clone()) {
            return Err(format!("派生任务 key 重复: {}", task.key));
        }
    }
    Ok(())
}

fn detect_cycle(tasks: &[NormalizedTask]) -> Result<(), String> {
    let map: HashMap<&str, &NormalizedTask> =
        tasks.iter().map(|task| (task.key.as_str(), task)).collect();
    fn visit<'a>(
        key: &'a str,
        map: &HashMap<&'a str, &'a NormalizedTask>,
        visiting: &mut HashSet<&'a str>,
        done: &mut HashSet<&'a str>,
    ) -> Result<(), String> {
        if done.contains(key) {
            return Ok(());
        }
        if !visiting.insert(key) {
            return Err(format!("任务依赖存在环: {key}"));
        }
        if let Some(task) = map.get(key) {
            for dep in &task.deps {
                visit(dep, map, visiting, done)?;
            }
        }
        visiting.remove(key);
        done.insert(key);
        Ok(())
    }
    let mut visiting = HashSet::new();
    let mut done = HashSet::new();
    for task in tasks {
        visit(&task.key, &map, &mut visiting, &mut done)?;
    }
    Ok(())
}

/// Validates one human-added task against the project's active tasks. The
/// returned task keeps the project DAG acyclic and falls back to the plan
/// defaults for missing acceptance criteria.
pub fn validate_manual_task(
    active: &[NormalizedTask],
    key: &str,
    title: &str,
    description: &str,
    acceptance: Vec<String>,
    deps: Vec<String>,
) -> Result<NormalizedTask, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("任务 key 不能为空".into());
    }
    if key.len() > 32 {
        return Err(format!("任务 key 过长: {key}"));
    }
    if active.iter().any(|task| task.key == key) {
        return Err(format!("任务 key 已存在: {key}"));
    }
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err(format!("任务 {key} 缺少标题"));
    }
    let description = description.trim().to_string();
    if description.is_empty() {
        return Err(format!("任务 {key} 缺少描述"));
    }
    let mut acceptance: Vec<String> = acceptance
        .into_iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect();
    if acceptance.is_empty() {
        acceptance.push("实现任务描述中的要求，并通过项目已有测试".into());
    }
    let deps: Vec<String> = deps
        .into_iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect();
    let keys: HashSet<String> = active.iter().map(|task| task.key.clone()).collect();
    for dep in &deps {
        if dep == key {
            return Err(format!("任务 {key} 不能依赖自身"));
        }
        if !keys.contains(dep) {
            return Err(format!("任务 {key} 依赖了不存在的任务 {dep}"));
        }
    }
    let mut tasks = active.to_vec();
    tasks.push(NormalizedTask {
        key: key.to_string(),
        title: title.clone(),
        description: description.clone(),
        acceptance: acceptance.clone(),
        deps: deps.clone(),
    });
    detect_cycle(&tasks)?;
    Ok(NormalizedTask {
        key: key.to_string(),
        title,
        description,
        acceptance,
        deps,
    })
}

pub fn deps_complete(deps: &[String], completed: &HashSet<String>) -> bool {
    deps.iter().all(|dep| completed.contains(dep))
}

/// Failure categories drive agent selection: rate limits and exhausted
/// transient retries cool an agent down, permanent errors move to the next
/// agent immediately, and task-level failures use the task retry budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    RateLimit,
    Transient,
    Permanent,
    Task,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureKind::RateLimit => crate::models::FAILURE_RATE_LIMIT,
            FailureKind::Transient => crate::models::FAILURE_TRANSIENT,
            FailureKind::Permanent => crate::models::FAILURE_PERMANENT,
            FailureKind::Task => crate::models::FAILURE_TASK,
        }
    }

    pub fn is_agent_failure(self) -> bool {
        !matches!(self, FailureKind::Task)
    }
}

pub fn classify_failure(reason: &str, process_failure: bool) -> FailureKind {
    let lowered = reason.to_lowercase();
    if contains_any(
        &lowered,
        &[
            "429",
            "rate limit",
            "ratelimit",
            "too many requests",
            "quota",
            "限流",
            "请求过多",
        ],
    ) {
        return FailureKind::RateLimit;
    }
    if contains_any(
        &lowered,
        &[
            "永久错误",
            "permanent",
            "unauthorized",
            "forbidden",
            "invalid api key",
            "invalid_request",
            "401",
            "403",
            "配置无效",
            "不可恢复",
        ],
    ) {
        return FailureKind::Permanent;
    }
    if process_failure
        || contains_any(
            &lowered,
            &[
                "timeout",
                "timed out",
                "超时",
                "temporar",
                "unavailable",
                "overloaded",
                "connection",
                "network",
                "502",
                "503",
                "504",
                "broken pipe",
                "provider",
            ],
        )
    {
        return FailureKind::Transient;
    }
    FailureKind::Task
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

/// Rate limits may carry their own retry window. The default is 30 minutes.
pub fn rate_limit_cooldown(reason: &str) -> chrono::Duration {
    let default_secs = std::env::var("DOMO_RATE_LIMIT_COOLDOWN_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(30 * 60);
    let secs = parse_retry_after(reason).unwrap_or(default_secs);
    chrono::Duration::seconds(secs.clamp(1, 24 * 60 * 60))
}

pub fn transient_retry_limit() -> i64 {
    std::env::var("DOMO_TRANSIENT_RETRY_LIMIT")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(3)
}

fn parse_retry_after(reason: &str) -> Option<i64> {
    let lowered = reason.to_lowercase();
    for marker in [
        "retry after",
        "retry_after",
        "retry-after",
        "reset after",
        "reset_after",
        "reset-after",
    ] {
        if let Some(position) = lowered.find(marker) {
            if let Some(secs) = parse_amount(&lowered[position + marker.len()..]) {
                return Some(secs);
            }
        }
    }
    for marker in ["分钟后", "分钟", "minute", "min ", "hour", "小时"] {
        if let Some(position) = lowered.find(marker) {
            let start = lowered[..position]
                .rfind(|ch: char| !ch.is_ascii_digit())
                .map(|index| index + 1)
                .unwrap_or(0);
            if let Ok(value) = lowered[start..position].trim().parse::<i64>() {
                let multiplier = if marker.contains("小时") || marker.contains("hour") {
                    3600
                } else {
                    60
                };
                return Some(value * multiplier);
            }
        }
    }
    None
}

fn parse_amount(text: &str) -> Option<i64> {
    let mut chars = text.chars().skip_while(|ch| {
        ch.is_whitespace() || *ch == '=' || *ch == ':' || *ch == '：' || *ch == '后'
    });
    let mut digits = String::new();
    for ch in chars.by_ref() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            break;
        }
    }
    let value = digits.parse::<i64>().ok()?;
    let unit: String = chars
        .by_ref()
        .skip_while(|ch| ch.is_whitespace())
        .take_while(|ch| {
            ch.is_ascii_alphabetic() || *ch == '小' || *ch == '时' || *ch == '分' || *ch == '钟'
        })
        .collect();
    let unit = unit.to_lowercase();
    if unit.starts_with('h') || unit.contains('时') {
        Some(value * 3600)
    } else if unit.starts_with('m') || unit.contains("分") {
        Some(value * 60)
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(key: &str, deps: &[&str]) -> PlanTask {
        PlanTask {
            key: key.into(),
            title: format!("task {key}"),
            description: "do something".into(),
            acceptance: vec!["works".into()],
            deps: deps.iter().map(|v| v.to_string()).collect(),
        }
    }

    #[test]
    fn rejects_cycle() {
        let result = validate_plan(vec![task("A", &["B"]), task("B", &["A"])]);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_missing_dependency() {
        let result = validate_plan(vec![task("A", &["Z"])]);
        assert!(result.is_err());
    }

    #[test]
    fn accepts_dag_and_fills_keys() {
        let mut first = task("", &[]);
        first.key.clear();
        let mut second = task("B", &["T1"]);
        second.key.clear();
        let result = validate_plan(vec![first, second]).unwrap();
        assert_eq!(result[0].key, "T1");
        assert_eq!(result[1].key, "T2");
    }

    #[test]
    fn parses_analysis_plan_from_fence() {
        let raw = r#"done
```json
{"kind":"plan","summary":"s","goal":"g","assumptions":["a"],"acceptance":["b"],"tasks":[{"key":"T1","title":"a","description":"b","acceptance":["c"],"deps":[]}],"skip_confirmation":true}
```"#;
        match parse_result(JOB_ROLE_ANALYSIS, raw).unwrap() {
            ParsedResult::AnalysisPlan {
                goal,
                assumptions,
                acceptance,
                tasks,
                skip_confirmation,
                ..
            } => {
                assert_eq!(goal.as_deref(), Some("g"));
                assert_eq!(assumptions, vec!["a"]);
                assert_eq!(acceptance, vec!["b"]);
                assert_eq!(tasks.len(), 1);
                assert!(skip_confirmation);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_questions() {
        let raw = r#"{"kind":"questions","questions":["what?"]}"#;
        match parse_result(JOB_ROLE_ANALYSIS, raw).unwrap() {
            ParsedResult::Questions { questions, .. } => assert_eq!(questions, vec!["what?"]),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_analysis_direct() {
        let direct = r#"{"kind":"direct","summary":"s","goal":"g","plan":["p"],"verification":["v"],"skip_confirmation":false}"#;
        match parse_result(JOB_ROLE_ANALYSIS, direct).unwrap() {
            ParsedResult::AnalysisDirect {
                goal,
                plan,
                verification,
                ..
            } => {
                assert_eq!(goal.as_deref(), Some("g"));
                assert_eq!(plan, vec!["p"]);
                assert_eq!(verification, vec!["v"]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_direct_completion_report() {
        let raw = r#"{"kind":"direct_done","summary":"s","report":"report body"}"#;
        match parse_result(JOB_ROLE_DIRECT, raw).unwrap() {
            ParsedResult::DirectDone { report, .. } => assert_eq!(report, "report body"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_empty_analysis_plan() {
        let raw = r#"{"kind":"plan","tasks":[]}"#;
        assert!(parse_result(JOB_ROLE_ANALYSIS, raw).is_err());
    }

    #[test]
    fn parses_verdict() {
        let raw = r#"{"pass":false,"reason":"tests fail","issues":["a"]}"#;
        match parse_result(ROLE_VERIFIER, raw).unwrap() {
            ParsedResult::Verdict { pass, issues, .. } => {
                assert!(!pass);
                assert_eq!(issues, vec!["a"]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_executor_replan_request() {
        let raw = r#"{"kind":"replan","reason":"原计划与仓库现状冲突"}"#;
        match parse_result(ROLE_EXECUTOR, raw).unwrap() {
            ParsedResult::ExecutorReplan { reason } => {
                assert_eq!(reason, "原计划与仓库现状冲突")
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    fn derive_task(key: &str, deps: &[&str]) -> PlanTask {
        PlanTask {
            key: key.into(),
            title: format!("派生 {key}"),
            description: "完成派生子任务".into(),
            acceptance: vec!["产物存在".into()],
            deps: deps.iter().map(|v| v.to_string()).collect(),
        }
    }

    fn normalized(key: &str, deps: &[&str]) -> NormalizedTask {
        NormalizedTask {
            key: key.into(),
            title: format!("派生 {key}"),
            description: "完成派生子任务".into(),
            acceptance: vec!["产物存在".into()],
            deps: deps.iter().map(|v| v.to_string()).collect(),
        }
    }

    #[test]
    fn parses_executor_derive_request() {
        let raw = r#"{"kind":"derive","summary":"拆分","tasks":[
            {"key":"T1.1","title":"a","description":"b","acceptance":["c"],"deps":[]},
            {"key":"T1.2","title":"d","description":"e","acceptance":["f"],"deps":[]}
        ]}"#;
        match parse_result(ROLE_EXECUTOR, raw).unwrap() {
            ParsedResult::ExecutorDerive { summary, tasks } => {
                assert_eq!(summary, "拆分");
                assert_eq!(tasks.len(), 2);
                assert!(tasks.iter().all(|task| task.deps.is_empty()));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_derive_with_dependencies() {
        let raw = r#"{"kind":"derive","summary":"拆分","tasks":[
            {"key":"T1.1","title":"a","description":"b","acceptance":["c"],"deps":["T1"]}
        ]}"#;
        assert!(parse_result(ROLE_EXECUTOR, raw).is_err());
    }

    #[test]
    fn rejects_derive_over_limit() {
        let tasks: Vec<PlanTask> = (1..=MAX_DERIVED_TASKS + 1)
            .map(|index| derive_task(&format!("T1.{index}"), &[]))
            .collect();
        assert!(validate_derive(tasks).is_err());
    }

    #[test]
    fn rejects_derive_duplicate_keys() {
        let tasks = vec![derive_task("T1.1", &[]), derive_task("T1.1", &[])];
        assert!(validate_derive(tasks).is_err());
    }

    #[test]
    fn rejects_derive_missing_fields() {
        let mut missing_description = derive_task("T1.1", &[]);
        missing_description.description.clear();
        assert!(validate_derive(vec![missing_description]).is_err());

        let mut missing_acceptance = derive_task("T1.2", &[]);
        missing_acceptance.acceptance.clear();
        assert!(validate_derive(vec![missing_acceptance]).is_err());

        let mut missing_key = derive_task("", &[]);
        missing_key.key.clear();
        assert!(validate_derive(vec![missing_key]).is_err());
    }

    #[test]
    fn rejects_derive_from_derived_task() {
        let active: HashSet<String> = HashSet::new();
        let error = validate_derive_scope(true, &active, &[normalized("T1.1", &[])]);
        assert!(error.is_err());
    }

    #[test]
    fn rejects_derive_key_collision_with_active_task() {
        let active: HashSet<String> = ["T2".to_string()].into_iter().collect();
        let error = validate_derive_scope(false, &active, &[normalized("T2", &[])]);
        assert!(error.is_err());
    }

    #[test]
    fn accepts_parallel_sibling_derivation() {
        let active: HashSet<String> = ["T1".to_string()].into_iter().collect();
        let result = validate_derive_scope(
            false,
            &active,
            &[normalized("T1.1", &[]), normalized("T1.2", &[])],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn deps_complete_checks_all() {
        let completed: HashSet<String> = ["A".to_string()].into_iter().collect();
        assert!(deps_complete(&["A".into()], &completed));
        assert!(!deps_complete(&["A".into(), "B".into()], &completed));
    }

    #[test]
    fn manual_task_accepts_valid_task() {
        let active = vec![normalized("T1", &[]), normalized("T2", &["T1"])];
        let task = validate_manual_task(
            &active,
            "T3",
            "补充任务",
            "完成补充工作",
            vec![" 产物存在 ".into()],
            vec![" T2 ".into(), "".into()],
        )
        .unwrap();
        assert_eq!(task.key, "T3");
        assert_eq!(task.deps, vec!["T2".to_string()]);
        assert_eq!(task.acceptance, vec!["产物存在".to_string()]);
    }

    #[test]
    fn manual_task_fills_default_acceptance() {
        let task = validate_manual_task(&[], "T1", "标题", "描述", Vec::new(), Vec::new()).unwrap();
        assert!(!task.acceptance.is_empty());
    }

    #[test]
    fn manual_task_rejects_cycle() {
        let active = vec![normalized("B", &["A"])];
        let error =
            validate_manual_task(&active, "A", "标题", "描述", Vec::new(), vec!["B".into()])
                .unwrap_err();
        assert!(error.contains("环"), "unexpected error: {error}");
    }

    #[test]
    fn manual_task_rejects_duplicate_key() {
        let active = vec![normalized("T1", &[])];
        let error = validate_manual_task(&active, "T1", "标题", "描述", Vec::new(), Vec::new())
            .unwrap_err();
        assert!(error.contains("已存在"), "unexpected error: {error}");
    }

    #[test]
    fn manual_task_rejects_missing_dependency() {
        let error = validate_manual_task(&[], "T1", "标题", "描述", Vec::new(), vec!["T9".into()])
            .unwrap_err();
        assert!(error.contains("不存在"), "unexpected error: {error}");
    }

    #[test]
    fn manual_task_rejects_self_dependency() {
        let error = validate_manual_task(&[], "T1", "标题", "描述", Vec::new(), vec!["T1".into()])
            .unwrap_err();
        assert!(error.contains("自身"), "unexpected error: {error}");
    }

    #[test]
    fn manual_task_rejects_missing_fields() {
        assert!(validate_manual_task(&[], "", "标题", "描述", Vec::new(), Vec::new()).is_err());
        assert!(validate_manual_task(&[], "T1", " ", "描述", Vec::new(), Vec::new()).is_err());
        assert!(validate_manual_task(&[], "T1", "标题", " ", Vec::new(), Vec::new()).is_err());
    }

    #[test]
    fn slug_is_stable() {
        assert_eq!(slug("Hello World!"), "hello-world");
        assert_eq!(slug("***"), "task");
    }

    #[test]
    fn classifies_rate_limit_transient_and_permanent() {
        assert_eq!(
            classify_failure("HTTP 429 too many requests", false),
            FailureKind::RateLimit
        );
        assert_eq!(
            classify_failure("provider temporarily unavailable", false),
            FailureKind::Transient
        );
        assert_eq!(
            classify_failure("Agent 作业超时", true),
            FailureKind::Transient
        );
        assert_eq!(
            classify_failure("Agent 作业卡死：连续 60 秒无输出或进度", true),
            FailureKind::Transient
        );
        assert_eq!(
            classify_failure("永久错误：Agent 配置无效", false),
            FailureKind::Permanent
        );
        assert_eq!(
            classify_failure("mock 首次执行失败", false),
            FailureKind::Task
        );
    }

    #[test]
    fn parses_rate_limit_cooldown_from_message() {
        assert_eq!(
            rate_limit_cooldown("HTTP 429; retry after 900 seconds").num_seconds(),
            900
        );
        assert_eq!(
            rate_limit_cooldown("429 retry_after=120").num_seconds(),
            120
        );
        assert!(rate_limit_cooldown("429 rate limit").num_seconds() >= 60);
    }
}
