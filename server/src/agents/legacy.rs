//! Agent kinds that still run through their original per-kind functions.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::models::AgentRow;
use crate::usage::UsageRecord;

use super::adapter::{AgentRun, AgentSpec};
use super::process::run_process;

pub(super) async fn run_opencode(
    agent: &AgentRow,
    spec: &AgentSpec,
    timeout: Duration,
) -> Result<AgentRun> {
    let bin = crate::opencode::bin();
    let session_id = spec
        .session_id
        .clone()
        .unwrap_or_else(|| crate::opencode::session_id(&spec.job_id));
    let args = opencode_args(&agent.model, &session_id, spec);
    // Prompts routinely exceed the 32767-character Windows command line, so
    // they reach the CLI over stdin.
    let mut run = run_process(
        &bin,
        &args,
        spec,
        timeout,
        Some(spec.prompt.as_bytes().to_vec()),
    )
    .await?;
    run.session_id = Some(session_id);
    Ok(run)
}

pub(super) fn opencode_args(model: &str, session_id: &str, spec: &AgentSpec) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--auto".into(),
        "--format".into(),
        "json".into(),
        "--session".into(),
        session_id.to_string(),
        // An explicit title skips the separate title-generation model call, so
        // the session ledger contains only this job's work.
        "--title".into(),
        format!("DOMO {} {}", spec.role, spec.job_id),
    ];
    let model = model.trim();
    if !model.is_empty() {
        args.push("--model".into());
        args.push(model.to_string());
    }
    args
}

/// Continuation prompts are sent as-is when the agent can resume the original
/// session. A fresh process receives the original prompt first.
pub(super) fn prompt_for_run(spec: &AgentSpec, can_resume: bool) -> String {
    if spec.continuation && !can_resume {
        format!("{}\n\n{}", spec.original_prompt, spec.prompt)
    } else {
        spec.prompt.clone()
    }
}

pub(super) async fn run_cli(
    agent: &AgentRow,
    spec: &AgentSpec,
    timeout: Duration,
) -> Result<AgentRun> {
    let command: Vec<String> = agent
        .command_json
        .as_deref()
        .map(|raw| serde_json::from_str::<Vec<String>>(raw).unwrap_or_default())
        .unwrap_or_default();
    if command.is_empty() {
        return Err(anyhow!("CLI Agent 未配置命令模板"));
    }
    let model_args: Vec<String> = agent
        .model_args_json
        .as_deref()
        .map(|raw| serde_json::from_str::<Vec<String>>(raw).unwrap_or_default())
        .unwrap_or_default();
    let model = agent.model.trim();
    let session_id = spec.session_id.clone().unwrap_or_default();
    let resume_aware = command
        .iter()
        .chain(model_args.iter())
        .any(|arg| arg.contains("{session_id}"));
    let mut expanded_model_args = Vec::new();
    if !model.is_empty() {
        expanded_model_args = model_args
            .iter()
            .map(|arg| {
                arg.replace("{model}", model)
                    .replace("{session_id}", &session_id)
            })
            .collect();
    } else if model_args.iter().any(|arg| arg.contains("{session_id}")) {
        expanded_model_args = model_args
            .iter()
            .map(|arg| arg.replace("{session_id}", &session_id))
            .collect();
    }
    let prompt = prompt_for_run(spec, resume_aware);
    let mut args = Vec::new();
    let mut has_prompt = false;
    for arg in &command[1..] {
        match arg.as_str() {
            "{prompt}" => {
                args.push(prompt.clone());
                has_prompt = true;
            }
            "{session_id}" => args.push(session_id.clone()),
            "{job_dir}" => args.push(spec.job_dir.to_string_lossy().to_string()),
            "{cwd}" => args.push(spec.cwd.to_string_lossy().to_string()),
            "{model}" => {
                if !model.is_empty() {
                    args.push(model.to_string());
                }
            }
            "{model_args}" => args.extend(expanded_model_args.iter().cloned()),
            other => args.push(other.replace("{model}", model)),
        }
    }
    if !model.is_empty()
        && !command
            .iter()
            .any(|arg| arg.contains("{model}") || arg == "{model_args}")
    {
        args.extend(expanded_model_args);
    }
    let stdin = if has_prompt {
        None
    } else {
        Some(prompt.into_bytes())
    };
    let mut run = run_process(&command[0], &args, spec, timeout, stdin).await?;
    if resume_aware {
        run.session_id = spec.session_id.clone();
    }
    Ok(run)
}

pub(super) fn configured_command(agent: &AgentRow) -> Option<Vec<String>> {
    let command: Vec<String> = agent
        .command_json
        .as_deref()
        .map(|raw| serde_json::from_str::<Vec<String>>(raw).unwrap_or_default())
        .unwrap_or_default();
    (!command.is_empty()).then_some(command)
}

pub(super) async fn run_codex(
    agent: &AgentRow,
    spec: &AgentSpec,
    timeout: Duration,
) -> Result<AgentRun> {
    if configured_command(agent).is_some() {
        return run_cli(agent, spec, timeout).await;
    }
    let resume_id = spec
        .session_id
        .as_deref()
        .filter(|session_id| !session_id.trim().is_empty());
    let can_resume = spec.continuation && resume_id.is_some();
    let args = if can_resume {
        codex_resume_args(&agent.model, resume_id.unwrap_or_default())
    } else {
        codex_args(&agent.model, &spec.job_dir)
    };
    let prompt = prompt_for_run(spec, can_resume);
    let mut run = run_process(
        &codex_bin(),
        &args,
        spec,
        timeout,
        Some(prompt.into_bytes()),
    )
    .await?;
    run.session_id = parse_codex_session_id(&run.stdout)
        .or_else(|| can_resume.then(|| resume_id.unwrap_or_default().to_string()));
    run.usage = parse_codex_usage(&run.stdout);
    Ok(run)
}

pub(super) async fn run_workbuddy(
    agent: &AgentRow,
    spec: &AgentSpec,
    timeout: Duration,
) -> Result<AgentRun> {
    if configured_command(agent).is_some() {
        return run_cli(agent, spec, timeout).await;
    }
    let resume_id = spec
        .session_id
        .as_deref()
        .filter(|session_id| !session_id.trim().is_empty());
    let can_resume = spec.continuation && resume_id.is_some();
    let args = if can_resume {
        workbuddy_resume_args(&agent.model, &spec.job_dir, resume_id.unwrap_or_default())
    } else {
        workbuddy_args(&agent.model, &spec.job_dir)
    };
    let prompt = prompt_for_run(spec, can_resume);
    let mut run = run_process(
        &workbuddy_bin(),
        &args,
        spec,
        timeout,
        Some(prompt.into_bytes()),
    )
    .await?;
    run.session_id = parse_workbuddy_session_id(&run.stdout)
        .or_else(|| can_resume.then(|| resume_id.unwrap_or_default().to_string()));
    run.usage = parse_workbuddy_usage(&run.stdout);
    Ok(run)
}

pub(super) fn codex_bin() -> String {
    bin_override("DOMO_CODEX_BIN", "codex")
}

pub(super) fn workbuddy_bin() -> String {
    bin_override("DOMO_WORKBUDDY_BIN", "codebuddy")
}

pub(super) fn bin_override(key: &str, fallback: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

pub(super) fn codex_args(model: &str, job_dir: &Path) -> Vec<String> {
    let mut args = vec![
        "exec".to_string(),
        "--json".to_string(),
        "--sandbox".to_string(),
        "workspace-write".to_string(),
        "--add-dir".to_string(),
        job_dir.to_string_lossy().to_string(),
    ];
    let model = model.trim();
    if !model.is_empty() {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    args.push("-".to_string());
    args
}

pub(super) fn workbuddy_args(model: &str, job_dir: &Path) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--dangerously-skip-permissions".to_string(),
        "--add-dir".to_string(),
        job_dir.to_string_lossy().to_string(),
    ];
    let model = model.trim();
    if !model.is_empty() {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    args
}

pub(super) fn codex_resume_args(model: &str, session_id: &str) -> Vec<String> {
    let mut args = vec![
        "exec".to_string(),
        "resume".to_string(),
        "--json".to_string(),
    ];
    let model = model.trim();
    if !model.is_empty() {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    args.push(session_id.to_string());
    args.push("-".to_string());
    args
}

pub(super) fn workbuddy_resume_args(model: &str, job_dir: &Path, session_id: &str) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--dangerously-skip-permissions".to_string(),
        "--add-dir".to_string(),
        job_dir.to_string_lossy().to_string(),
        "--resume".to_string(),
        session_id.to_string(),
    ];
    let model = model.trim();
    if !model.is_empty() {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    args
}

pub(super) fn parse_codex_session_id(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if value.get("type").and_then(|item| item.as_str()) != Some("thread.started") {
            continue;
        }
        if let Some(thread_id) = value.get("thread_id").and_then(|item| item.as_str()) {
            if !thread_id.trim().is_empty() {
                return Some(thread_id.to_string());
            }
        }
    }
    None
}

pub(super) fn parse_workbuddy_session_id(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if let Some(session_id) = value.get("session_id").and_then(|item| item.as_str()) {
            if !session_id.trim().is_empty() {
                return Some(session_id.to_string());
            }
        }
    }
    None
}

pub(super) fn codex_usage_record(
    usage: &serde_json::Map<String, serde_json::Value>,
) -> UsageRecord {
    UsageRecord {
        input_tokens: json_i64(usage.get("input_tokens")),
        output_tokens: json_i64(usage.get("output_tokens")),
        cache_read_tokens: json_i64(usage.get("cached_input_tokens")),
        cache_write_tokens: json_i64(usage.get("cache_write_input_tokens")),
        reasoning_tokens: json_i64(usage.get("reasoning_output_tokens")),
        cost: None,
        duration_ms: None,
    }
}

/// Codex reports one `turn.completed` per turn; its usage covers the whole turn.
/// Lines that are not JSON or carry no usage are ignored.
pub(super) fn parse_codex_usage(stdout: &str) -> Option<UsageRecord> {
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if value.get("type").and_then(|item| item.as_str()) != Some("turn.completed") {
            continue;
        }
        let Some(usage) = value.get("usage").and_then(|item| item.as_object()) else {
            continue;
        };
        let record = codex_usage_record(usage);
        if record.known() {
            return Some(record);
        }
    }
    None
}

/// WorkBuddy reports the session total in the `result` event. When that event
/// lacks usage, the last assistant message carries the per-request counts.
pub(super) fn parse_workbuddy_usage(stdout: &str) -> Option<UsageRecord> {
    let mut result_usage = None;
    let mut result_cost = None;
    let mut last_assistant_usage = None;
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        match value.get("type").and_then(|item| item.as_str()) {
            Some("result") => {
                if let Some(usage) = value.get("usage").and_then(|item| item.as_object()) {
                    result_usage = Some(usage.clone());
                }
                result_cost = value.get("total_cost_usd").and_then(|item| item.as_f64());
            }
            Some("assistant") => {
                if let Some(usage) = value
                    .get("message")
                    .and_then(|message| message.get("usage"))
                    .and_then(|item| item.as_object())
                {
                    last_assistant_usage = Some(usage.clone());
                }
            }
            _ => {}
        }
    }
    let usage = result_usage.or(last_assistant_usage)?;
    let record = UsageRecord {
        input_tokens: json_i64(usage.get("input_tokens")),
        output_tokens: json_i64(usage.get("output_tokens")),
        cache_read_tokens: json_i64(usage.get("cache_read_input_tokens")),
        cache_write_tokens: json_i64(usage.get("cache_creation_input_tokens")),
        reasoning_tokens: None,
        cost: result_cost,
        duration_ms: None,
    };
    record.known().then_some(record)
}

pub(super) fn json_i64(value: Option<&serde_json::Value>) -> Option<i64> {
    value.and_then(|item| item.as_i64())
}
