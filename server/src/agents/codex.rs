//! Codex CLI adapter. Arguments are built in-process; the CLI reports its
//! thread id and turn usage as JSONL on stdout. A configured command template
//! takes precedence and runs through the custom CLI adapter.

use std::path::Path;

use anyhow::Result;

use crate::models::AGENT_CODEX;
use crate::usage::UsageRecord;

use super::adapter::{
    bin_override, derived_session_id, json_i64, AgentAdapter, AgentConfig, AgentRun, AgentSpec,
    TurnContext,
};
use super::cli::{prompt_for_run, CliAdapter};
use super::process::run_process;

#[derive(Debug, Default)]
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    const KIND: &'static str = AGENT_CODEX;

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
        codex_turn(context.config.model.trim(), context.spec, context.timeout).await
    }
}

async fn codex_turn(
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
        codex_resume_args(model, resume_id.unwrap_or_default())
    } else {
        codex_args(model, &spec.job_dir)
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

pub(super) fn codex_bin() -> String {
    bin_override("DOMO_CODEX_BIN", "codex")
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
        let dir = std::env::temp_dir().join(format!("domo-codex-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = AgentConfig {
            kind: AGENT_CODEX.into(),
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
        let run = CodexAdapter
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
    fn codex_args_omit_model_flag_when_model_is_empty() {
        let args = codex_args("", Path::new("/tmp/job"));
        assert_eq!(args.first().map(String::as_str), Some("exec"));
        assert!(args.contains(&"--json".to_string()));
        assert!(args_contain_pair(&args, "--sandbox", "workspace-write"));
        assert!(args_contain_pair(&args, "--add-dir", "/tmp/job"));
        assert!(!args.contains(&"--model".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("-"));
    }

    #[test]
    fn codex_args_include_model_when_present() {
        let args = codex_args(" gpt-5-codex ", Path::new("/tmp/job"));
        assert!(args.contains(&"--json".to_string()));
        assert!(args_contain_pair(&args, "--sandbox", "workspace-write"));
        assert!(args_contain_pair(&args, "--add-dir", "/tmp/job"));
        assert!(args_contain_pair(&args, "--model", "gpt-5-codex"));
        assert_eq!(args.last().map(String::as_str), Some("-"));
    }

    #[test]
    fn resume_args_keep_the_agent_session_id() {
        let codex = codex_resume_args("gpt-test", "thread-1");
        assert_eq!(codex[0], "exec");
        assert_eq!(codex[1], "resume");
        assert!(codex.contains(&"--json".to_string()));
        assert!(codex.contains(&"thread-1".to_string()));
        assert_eq!(codex.last().map(String::as_str), Some("-"));
    }

    #[test]
    fn parses_session_ids_from_agent_streams() {
        let codex = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}\n",
            "{\"type\":\"turn.completed\"}\n"
        );
        assert_eq!(parse_codex_session_id(codex).as_deref(), Some("thread-1"));
    }

    #[test]
    fn codex_usage_maps_turn_totals_and_details() {
        let stdout = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":21349,",
            "\"cached_input_tokens\":13952,\"cache_write_input_tokens\":7,",
            "\"output_tokens\":130,\"reasoning_output_tokens\":11}}\n"
        );
        let usage = parse_codex_usage(stdout).expect("usage");
        assert_eq!(usage.input_tokens, Some(21349));
        assert_eq!(usage.output_tokens, Some(130));
        assert_eq!(usage.cache_read_tokens, Some(13952));
        assert_eq!(usage.cache_write_tokens, Some(7));
        assert_eq!(usage.reasoning_tokens, Some(11));
        assert_eq!(usage.cost, None);
    }

    #[test]
    fn usage_is_unknown_without_reporting_events() {
        let stdout = concat!(
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\"}}\n",
            "not json\n"
        );
        assert_eq!(parse_codex_usage(stdout), None);
    }

    #[test]
    fn bin_overrides_follow_environment() {
        std::env::remove_var("DOMO_CODEX_BIN");
        assert_eq!(codex_bin(), "codex");

        std::env::set_var("DOMO_CODEX_BIN", "/opt/codex-cli");
        assert_eq!(codex_bin(), "/opt/codex-cli");

        std::env::remove_var("DOMO_CODEX_BIN");
    }
}
