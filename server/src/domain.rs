//! Domain rules: identifiers, DAG validation, and the JSON protocol used between
//! DOMO and agent jobs.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

use crate::models::{
    parse_string_list, JOB_ROLE_GOAL, JOB_ROLE_SUMMARY, ROLE_EXECUTOR, ROLE_VERIFIER,
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

#[derive(Debug, Clone)]
pub enum ParsedResult {
    Questions {
        questions: Vec<String>,
        summary: String,
    },
    Plan {
        summary: String,
        tasks: Vec<NormalizedTask>,
    },
    ExecutorDone {
        summary: String,
    },
    ExecutorFailed {
        reason: String,
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
        JOB_ROLE_GOAL => parse_planner_result(&value),
        ROLE_EXECUTOR => parse_executor_result(&value),
        ROLE_VERIFIER => parse_verifier_result(&value),
        JOB_ROLE_SUMMARY => parse_report_result(&value),
        other => Err(format!("未知作业角色: {other}")),
    }
}

fn parse_planner_result(value: &Value) -> Result<ParsedResult, String> {
    let summary = string_field(value, "summary").unwrap_or_default();
    let questions = string_array(value.get("questions"));
    if !questions.is_empty() && value.get("tasks").is_none() {
        return Ok(ParsedResult::Questions { questions, summary });
    }
    let tasks_value = value
        .get("tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "规划结果缺少 tasks 数组".to_string())?;
    let mut raw_tasks = Vec::new();
    for item in tasks_value {
        let acceptance = string_array(item.get("acceptance"));
        raw_tasks.push(PlanTask {
            key: string_field(item, "key").unwrap_or_default(),
            title: string_field(item, "title").unwrap_or_default(),
            description: string_field(item, "description").unwrap_or_default(),
            acceptance,
            deps: string_array(item.get("deps").or_else(|| item.get("depends_on"))),
        });
    }
    let tasks = validate_plan(raw_tasks)?;
    Ok(ParsedResult::Plan { summary, tasks })
}

fn parse_executor_result(value: &Value) -> Result<ParsedResult, String> {
    let failed = value
        .get("failed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || value
            .get("kind")
            .and_then(|v| v.as_str())
            .map(|v| v == "failed" || v == "blocked")
            .unwrap_or(false);
    let reason = string_field(value, "reason")
        .or_else(|| string_field(value, "summary"))
        .or_else(|| string_field(value, "error"))
        .unwrap_or_default();
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

pub fn deps_complete(deps: &[String], completed: &HashSet<String>) -> bool {
    deps.iter().all(|dep| completed.contains(dep))
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
    fn parses_planner_plan_from_fence() {
        let raw = r#"done
```json
{"kind":"plan","summary":"s","tasks":[{"key":"T1","title":"a","description":"b","acceptance":["c"],"deps":[]}]}
```"#;
        match parse_result(JOB_ROLE_GOAL, raw).unwrap() {
            ParsedResult::Plan { tasks, .. } => assert_eq!(tasks.len(), 1),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_questions() {
        let raw = r#"{"kind":"questions","questions":["what?"]}"#;
        match parse_result(JOB_ROLE_GOAL, raw).unwrap() {
            ParsedResult::Questions { questions, .. } => assert_eq!(questions, vec!["what?"]),
            other => panic!("unexpected: {other:?}"),
        }
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
    fn deps_complete_checks_all() {
        let completed: HashSet<String> = ["A".to_string()].into_iter().collect();
        assert!(deps_complete(&["A".into()], &completed));
        assert!(!deps_complete(&["A".into(), "B".into()], &completed));
    }

    #[test]
    fn slug_is_stable() {
        assert_eq!(slug("Hello World!"), "hello-world");
        assert_eq!(slug("***"), "task");
    }
}
