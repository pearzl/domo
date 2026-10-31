//! OpenCode CLI adapter. Prompts reach the CLI over stdin because they can
//! exceed the Windows command line limit; the session is derived from the job.

use anyhow::Result;

use crate::models::AGENT_OPENCODE;

use super::adapter::{AgentAdapter, AgentConfig, AgentRun, AgentSpec, TurnContext};
use super::process::run_process;

#[derive(Debug, Default)]
pub struct OpencodeAdapter;

impl AgentAdapter for OpencodeAdapter {
    const KIND: &'static str = AGENT_OPENCODE;

    fn requires_command_template(_config: &AgentConfig) -> bool {
        false
    }

    fn validate(_config: &AgentConfig) -> Result<()> {
        Ok(())
    }

    fn session_for_job(job_id: &str, existing: Option<&str>) -> Option<String> {
        Some(
            existing
                .filter(|session_id| !session_id.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| crate::opencode::session_id(job_id)),
        )
    }

    async fn run_turn(&self, context: TurnContext<'_>) -> Result<AgentRun> {
        let bin = crate::opencode::bin();
        let session_id = context
            .spec
            .session_id
            .clone()
            .unwrap_or_else(|| crate::opencode::session_id(&context.spec.job_id));
        let args = opencode_args(&context.config.model, &session_id, context.spec);
        // Prompts routinely exceed the 32767-character Windows command line, so
        // they reach the CLI over stdin.
        let mut run = run_process(
            &bin,
            &args,
            context.spec,
            context.timeout,
            Some(context.spec.prompt.as_bytes().to_vec()),
        )
        .await?;
        run.session_id = Some(session_id);
        Ok(run)
    }
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::adapter::args_contain_pair;
    use super::*;
    use crate::models::JOB_ROLE_EXECUTOR;

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
            progress: crate::agents::JobProgress::new(),
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
    fn session_for_job_derives_from_the_job_id_and_keeps_a_continuation() {
        assert_eq!(
            OpencodeAdapter::session_for_job("job_test", None),
            Some(crate::opencode::session_id("job_test"))
        );
        assert_eq!(
            OpencodeAdapter::session_for_job("job_test", Some("session-1")).as_deref(),
            Some("session-1")
        );
    }
}
