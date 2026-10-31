//! Agent adapters. A job is one process (or one in-process mock) that changes
//! the workspace and writes a result JSON file. `kinds` is the single registry
//! of supported agent types; `execute` dispatches to the matching adapter.

mod adapter;
mod legacy;
mod mock;
mod process;

use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::models::{
    AgentRow, AGENT_CLI, AGENT_CODEX, AGENT_MOCK, AGENT_OPENCODE, AGENT_WORKBUDDY,
};

pub use adapter::{
    AgentAdapter, AgentConfig, AgentRun, AgentSpec, FinishContext, JobProgress, JobUsage,
    TurnContext,
};
pub use mock::MockAdapter;
pub use process::kill_job_dir;
#[allow(unused_imports)]
pub use process::kill_process_group;

const KINDS: [&str; 5] = [
    AGENT_MOCK,
    AGENT_OPENCODE,
    AGENT_CODEX,
    AGENT_WORKBUDDY,
    AGENT_CLI,
];

/// Agent kinds accepted by the registry, in display order.
pub fn kinds() -> &'static [&'static str] {
    &KINDS
}

pub async fn execute(agent: &AgentRow, spec: &AgentSpec, timeout: Duration) -> Result<AgentRun> {
    let kind = agent.kind.as_str();
    if !kinds().contains(&kind) {
        return Err(anyhow!("未知 Agent 类型: {kind}"));
    }
    match kind {
        AGENT_MOCK => {
            let config = AgentConfig::from_row(agent);
            run_with::<MockAdapter>(&config, spec, timeout).await
        }
        AGENT_OPENCODE => legacy::run_opencode(agent, spec, timeout).await,
        AGENT_CLI => legacy::run_cli(agent, spec, timeout).await,
        AGENT_CODEX => legacy::run_codex(agent, spec, timeout).await,
        AGENT_WORKBUDDY => legacy::run_workbuddy(agent, spec, timeout).await,
        other => Err(anyhow!("未知 Agent 类型: {other}")),
    }
}

/// Runs one adapter statically. The adapter validates its settings, executes
/// the turn, then settles usage through its own accounting rule.
async fn run_with<A: AgentAdapter>(
    config: &AgentConfig,
    spec: &AgentSpec,
    timeout: Duration,
) -> Result<AgentRun> {
    A::validate(config)?;
    let adapter = A::default();
    let mut run = adapter
        .run_turn(TurnContext {
            config,
            spec,
            timeout,
        })
        .await?;
    let usage = adapter
        .finish_job(FinishContext {
            config,
            spec,
            run: &run,
        })
        .await;
    if let JobUsage::Authoritative(usage) = usage {
        run.usage = usage;
    }
    Ok(run)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::legacy::{
        codex_args, codex_bin, codex_resume_args, opencode_args, parse_codex_session_id,
        parse_codex_usage, parse_workbuddy_session_id, parse_workbuddy_usage, workbuddy_args,
        workbuddy_bin, workbuddy_resume_args,
    };
    use super::process::{process_start_time, run_process};
    use super::*;
    use crate::models::JOB_ROLE_EXECUTOR;

    #[cfg(windows)]
    #[test]
    fn windows_process_start_time_is_available() {
        let pid = std::process::id();
        assert!(process_start_time(pid).is_some());
    }

    #[test]
    fn kill_job_dir_ignores_a_reused_pid() {
        let dir = std::env::temp_dir().join(format!("domo-kill-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pid"),
            format!("{}:not-the-recorded-start-time", std::process::id()),
        )
        .unwrap();
        assert!(!kill_job_dir(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kill_job_dir_kills_a_verified_process() {
        let dir = std::env::temp_dir().join(format!("domo-kill-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping", "-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        #[cfg(unix)]
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let start = process_start_time(pid).expect("child start time");
        std::fs::write(dir.join("pid"), format!("{pid}:{start}")).unwrap();

        assert!(kill_job_dir(&dir));
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn progress_starts_idle_at_zero_and_grows() {
        let progress = JobProgress::new();
        assert!(progress.idle_for() < Duration::from_millis(100));
        std::thread::sleep(Duration::from_millis(40));
        assert!(progress.idle_for() >= Duration::from_millis(30));
        progress.touch();
        assert!(progress.idle_for() < Duration::from_millis(100));
    }

    #[test]
    fn output_updates_idle_window_and_byte_counters() {
        let progress = JobProgress::new();
        std::thread::sleep(Duration::from_millis(40));
        progress.record_stdout(7);
        progress.record_stderr(3);
        assert_eq!(progress.output_bytes(), (7, 3));
        assert!(progress.idle_for() < Duration::from_millis(100));
    }

    #[test]
    fn resume_args_keep_the_agent_session_id() {
        let codex = codex_resume_args("gpt-test", "thread-1");
        assert_eq!(codex[0], "exec");
        assert_eq!(codex[1], "resume");
        assert!(codex.contains(&"--json".to_string()));
        assert!(codex.contains(&"thread-1".to_string()));
        assert_eq!(codex.last().map(String::as_str), Some("-"));

        let workbuddy = workbuddy_resume_args("hy3", Path::new("/tmp/job"), "session-1");
        assert!(workbuddy.contains(&"--resume".to_string()));
        assert!(workbuddy.contains(&"session-1".to_string()));
        assert!(workbuddy.contains(&"--add-dir".to_string()));
    }

    #[test]
    fn parses_session_ids_from_agent_streams() {
        let codex = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}\n",
            "{\"type\":\"turn.completed\"}\n"
        );
        assert_eq!(parse_codex_session_id(codex).as_deref(), Some("thread-1"));

        let workbuddy = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"session-1\"}\n",
            "{\"type\":\"result\",\"subtype\":\"success\"}\n"
        );
        assert_eq!(
            parse_workbuddy_session_id(workbuddy).as_deref(),
            Some("session-1")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawns_with_cwd_and_pwd_aligned() {
        let dir = std::env::temp_dir().join(format!("domo-pwd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = std::fs::canonicalize(&dir).unwrap();
        let spec = AgentSpec {
            job_id: "job_test".into(),
            role: JOB_ROLE_EXECUTOR.into(),
            cwd: cwd.clone(),
            job_dir: cwd.clone(),
            prompt: String::new(),
            original_prompt: String::new(),
            session_id: Some(crate::opencode::session_id("job_test")),
            continuation: false,
            progress: JobProgress::new(),
        };
        let args = vec![
            "-c".to_string(),
            "printf '%s\\n%s\\n' \"$PWD\" \"$(pwd)\"".to_string(),
        ];
        let run = run_process("sh", &args, &spec, Duration::from_secs(10), None)
            .await
            .unwrap();
        let expected = cwd.to_string_lossy();
        let mut lines = run.stdout.lines();
        assert_eq!(lines.next(), Some(expected.as_ref()));
        assert_eq!(lines.next(), Some(expected.as_ref()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn args_contain_pair(args: &[String], first: &str, second: &str) -> bool {
        args.windows(2)
            .any(|window| window[0] == first && window[1] == second)
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
    fn opencode_args_keep_the_prompt_out_of_the_command_line() {
        let spec = AgentSpec {
            job_id: "job_test".into(),
            role: JOB_ROLE_EXECUTOR.into(),
            cwd: PathBuf::from("/tmp/repo"),
            job_dir: PathBuf::from("/tmp/job"),
            prompt: "x".repeat(40_000),
            original_prompt: String::new(),
            session_id: None,
            continuation: false,
            progress: JobProgress::new(),
        };
        let session_id = crate::opencode::session_id("job_test");
        let args = opencode_args(" opencode/big ", &session_id, &spec);
        assert_eq!(args.first().map(String::as_str), Some("run"));
        assert!(args_contain_pair(&args, "--format", "json"));
        assert!(args_contain_pair(&args, "--session", &session_id));
        assert!(args_contain_pair(&args, "--model", "opencode/big"));
        assert!(!args.iter().any(|arg| arg.len() > 1024));
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
    fn usage_is_unknown_without_reporting_events() {
        let stdout = concat!(
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\"}}\n",
            "not json\n"
        );
        assert_eq!(parse_codex_usage(stdout), None);
        assert_eq!(parse_workbuddy_usage(stdout), None);
    }

    #[test]
    fn bin_overrides_follow_environment() {
        std::env::remove_var("DOMO_CODEX_BIN");
        std::env::remove_var("DOMO_WORKBUDDY_BIN");
        assert_eq!(codex_bin(), "codex");
        assert_eq!(workbuddy_bin(), "codebuddy");

        std::env::set_var("DOMO_CODEX_BIN", "/opt/codex-cli");
        std::env::set_var("DOMO_WORKBUDDY_BIN", "/opt/codebuddy-cli");
        assert_eq!(codex_bin(), "/opt/codex-cli");
        assert_eq!(workbuddy_bin(), "/opt/codebuddy-cli");

        std::env::remove_var("DOMO_CODEX_BIN");
        std::env::remove_var("DOMO_WORKBUDDY_BIN");
    }

    #[test]
    fn interrupted_runs_are_process_failures() {
        let run = AgentRun {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            timed_out: false,
            stalled: true,
            session_id: None,
            usage: None,
        };
        assert!(run.process_failure());
        let clean = AgentRun {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            timed_out: false,
            stalled: false,
            session_id: None,
            usage: None,
        };
        assert!(!clean.process_failure());
    }
}
