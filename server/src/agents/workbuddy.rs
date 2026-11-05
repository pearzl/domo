//! WorkBuddy CLI adapter. Arguments are built in-process; the CLI reports its
//! session id and result usage as JSONL on stdout. A configured command
//! template takes precedence and runs through the custom CLI adapter.

use std::path::Path;

use anyhow::Result;

use crate::models::AGENT_WORKBUDDY;
use crate::usage::UsageRecord;

use super::adapter::{
    bin_override, derived_session_id, json_i64, AgentAdapter, AgentConfig, AgentRun, AgentSpec,
    TurnContext,
};
use super::cli::{prompt_for_run, CliAdapter};
use super::process::run_process;

#[derive(Debug, Default)]
pub struct WorkbuddyAdapter;

impl AgentAdapter for WorkbuddyAdapter {
    const KIND: &'static str = AGENT_WORKBUDDY;

    fn requires_command_template(_config: &AgentConfig) -> bool {
        false
    }

    fn validate(_config: &AgentConfig) -> Result<()> {
        Ok(())
    }

    fn session_for_job(job_id: &str, existing: Option<&str>) -> Option<String> {
        Some(
            existing
                .map(str::to_string)
                .unwrap_or_else(|| derived_session_id(job_id)),
        )
    }

    async fn run_turn(&self, context: TurnContext<'_>) -> Result<AgentRun> {
        if !context.config.command.is_empty() {
            return CliAdapter.run_turn(context).await;
        }
        workbuddy_turn(context.config.model.trim(), context.spec, context.timeout).await
    }
}

async fn workbuddy_turn(
    model: &str,
    spec: &AgentSpec,
    timeout: std::time::Duration,
) -> Result<AgentRun> {
    let resume_id = spec
        .session_id
        .as_deref()
        .filter(|session_id| !session_id.trim().is_empty());
    let can_resume = spec.continuation && resume_id.is_some();
    let args = if can_resume {
        workbuddy_resume_args(model, &spec.job_dir, resume_id.unwrap_or_default())
    } else {
        workbuddy_args(model, &spec.job_dir)
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
    run.reported_error = parse_workbuddy_error(&run.stdout);
    Ok(run)
}

/// WorkBuddy reports execution failures inside the final `result` event while
/// the process still exits 0. The reason is the first line of the first
/// structured error so a provider payload never becomes the job error.
pub(crate) fn parse_workbuddy_error(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .filter(|value| value.get("type").and_then(serde_json::Value::as_str) == Some("result"))
        .filter_map(|value| workbuddy_result_error(&value))
        .last()
}

pub(crate) fn workbuddy_result_error(value: &serde_json::Value) -> Option<String> {
    let subtype = value
        .get("subtype")
        .and_then(serde_json::Value::as_str)
        .filter(|subtype| !subtype.is_empty());
    let is_error = value.get("is_error").and_then(serde_json::Value::as_bool) == Some(true);
    if !is_error && !subtype.is_some_and(|subtype| subtype != "success") {
        return None;
    }
    value
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find_map(|item| item.as_str().and_then(first_line))
        })
        .or_else(|| {
            value
                .get("errors_info")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.iter().find_map(info_error))
        })
        .or_else(|| {
            value
                .get("result")
                .and_then(serde_json::Value::as_str)
                .and_then(first_line)
        })
        .or_else(|| {
            Some(match subtype {
                Some(subtype) => format!("WorkBuddy 执行失败：{subtype}"),
                None => "WorkBuddy 报告执行错误".to_string(),
            })
        })
}

fn info_error(info: &serde_json::Value) -> Option<String> {
    info.get("details")
        .and_then(serde_json::Value::as_str)
        .and_then(first_line)
        .or_else(|| {
            info.get("message")
                .and_then(serde_json::Value::as_str)
                .and_then(first_line)
        })
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

pub(super) fn workbuddy_bin() -> String {
    bin_override("DOMO_WORKBUDDY_BIN", "codebuddy")
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    #[cfg(unix)]
    use std::time::Duration;

    use super::super::adapter::args_contain_pair;
    #[cfg(unix)]
    use super::super::adapter::{AgentSpec, JobProgress, TurnContext};
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn configured_command_runs_through_the_cli_adapter() {
        let dir = std::env::temp_dir().join(format!("domo-workbuddy-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = AgentConfig {
            kind: AGENT_WORKBUDDY.into(),
            model: String::new(),
            command: vec![
                "sh".into(),
                "-c".into(),
                "printf 'cli:%s' \"$1\"".into(),
                "sh".into(),
                "{prompt}".into(),
            ],
            model_args: Vec::new(),
        };
        let spec = AgentSpec {
            job_id: "job_test".into(),
            role: crate::models::JOB_ROLE_EXECUTOR.into(),
            cwd: dir.clone(),
            job_dir: dir.clone(),
            prompt: "hello".into(),
            original_prompt: "hello".into(),
            session_id: None,
            continuation: false,
            progress: JobProgress::new(),
        };
        let run = WorkbuddyAdapter
            .run_turn(TurnContext {
                config: &config,
                spec: &spec,
                timeout: Duration::from_secs(10),
            })
            .await
            .unwrap();
        assert_eq!(run.stdout, "cli:hello");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workbuddy_args_cover_stream_protocol_flags() {
        let args = workbuddy_args("", Path::new("/tmp/job"));
        assert!(args.contains(&"-p".to_string()));
        assert!(args_contain_pair(&args, "--output-format", "stream-json"));
        assert!(args.contains(&"--verbose".to_string()));
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(args_contain_pair(&args, "--add-dir", "/tmp/job"));
        assert!(!args.contains(&"--model".to_string()));

        let with_model = workbuddy_args("hy3", Path::new("/tmp/job"));
        assert!(args_contain_pair(&with_model, "--model", "hy3"));
    }

    #[test]
    fn resume_args_keep_the_agent_session_id() {
        let workbuddy = workbuddy_resume_args("hy3", Path::new("/tmp/job"), "session-1");
        assert!(workbuddy.contains(&"--resume".to_string()));
        assert!(workbuddy.contains(&"session-1".to_string()));
        assert!(workbuddy.contains(&"--add-dir".to_string()));
    }

    #[test]
    fn parses_session_ids_from_agent_streams() {
        let workbuddy = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"session-1\"}\n",
            "{\"type\":\"result\",\"subtype\":\"success\"}\n"
        );
        assert_eq!(
            parse_workbuddy_session_id(workbuddy).as_deref(),
            Some("session-1")
        );
    }

    #[test]
    fn workbuddy_usage_maps_result_totals_and_cost() {
        let stdout = concat!(
            "{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":10,",
            "\"output_tokens\":2}},\"session_id\":\"s\"}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,",
            "\"total_cost_usd\":0.0125,\"usage\":{\"input_tokens\":73243,",
            "\"output_tokens\":274,\"cache_creation_input_tokens\":3,",
            "\"cache_read_input_tokens\":49088}}\n"
        );
        let usage = parse_workbuddy_usage(stdout).expect("usage");
        assert_eq!(usage.input_tokens, Some(73243));
        assert_eq!(usage.output_tokens, Some(274));
        assert_eq!(usage.cache_read_tokens, Some(49088));
        assert_eq!(usage.cache_write_tokens, Some(3));
        assert_eq!(usage.reasoning_tokens, None);
        assert_eq!(usage.cost, Some(0.0125));
    }

    #[test]
    fn workbuddy_usage_falls_back_to_last_assistant_message() {
        let stdout = concat!(
            "{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":10,",
            "\"output_tokens\":2}},\"session_id\":\"s\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":40,",
            "\"output_tokens\":5,\"cache_read_input_tokens\":12}},\"session_id\":\"s\"}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}\n"
        );
        let usage = parse_workbuddy_usage(stdout).expect("usage");
        assert_eq!(usage.input_tokens, Some(40));
        assert_eq!(usage.output_tokens, Some(5));
        assert_eq!(usage.cache_read_tokens, Some(12));
        assert_eq!(usage.cost, None);
    }

    #[test]
    fn parses_quota_failure_from_the_result_event() {
        let stdout = concat!(
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            "\n",
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["429 您的使用量已超出频率限制…\n更多说明"],"errors_info":[{"status":429,"category":"quota","details":"429 您的使用量已超出频率限制…"}]}"#,
            "\n"
        );
        assert_eq!(
            parse_workbuddy_error(stdout).as_deref(),
            Some("429 您的使用量已超出频率限制…")
        );
    }

    #[test]
    fn parses_request_timeout_from_the_result_event() {
        let stdout = concat!(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["Request timed out\nupstream did not respond"]}"#,
            "\n"
        );
        assert_eq!(
            parse_workbuddy_error(stdout).as_deref(),
            Some("Request timed out")
        );
    }

    #[test]
    fn result_error_falls_back_to_structured_details() {
        let stdout = concat!(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors_info":[{"status":400,"details":"400 model service info not found\n可用模型列表"}]}"#,
            "\n"
        );
        assert_eq!(
            parse_workbuddy_error(stdout).as_deref(),
            Some("400 model service info not found")
        );
    }

    #[test]
    fn successful_result_reports_no_error() {
        let stdout = concat!(
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            "\n",
            r#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#,
            "\n"
        );
        assert_eq!(parse_workbuddy_error(stdout), None);
    }

    #[test]
    fn usage_is_unknown_without_reporting_events() {
        let stdout = concat!(
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\"}}\n",
            "not json\n"
        );
        assert_eq!(parse_workbuddy_usage(stdout), None);
    }

    #[test]
    fn bin_overrides_follow_environment() {
        std::env::remove_var("DOMO_WORKBUDDY_BIN");
        assert_eq!(workbuddy_bin(), "codebuddy");

        std::env::set_var("DOMO_WORKBUDDY_BIN", "/opt/codebuddy-cli");
        assert_eq!(workbuddy_bin(), "/opt/codebuddy-cli");

        std::env::remove_var("DOMO_WORKBUDDY_BIN");
    }
}
