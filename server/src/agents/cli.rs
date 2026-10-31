//! Custom CLI adapter. The command template and model arguments are expanded
//! from the agent settings; placeholders decide argument position and stdin.

use anyhow::{anyhow, Result};

use crate::models::AGENT_CLI;

use super::adapter::{AgentAdapter, AgentConfig, AgentRun, AgentSpec, TurnContext};
use super::process::run_process;

#[derive(Debug, Default)]
pub struct CliAdapter;

impl AgentAdapter for CliAdapter {
    const KIND: &'static str = AGENT_CLI;

    fn requires_command_template(_config: &AgentConfig) -> bool {
        true
    }

    fn validate(config: &AgentConfig) -> Result<()> {
        if config.command.is_empty() {
            return Err(anyhow!("CLI Agent 必须配置命令模板"));
        }
        Ok(())
    }

    fn session_for_job(_job_id: &str, existing: Option<&str>) -> Option<String> {
        existing.map(str::to_string)
    }

    async fn run_turn(&self, context: TurnContext<'_>) -> Result<AgentRun> {
        run_command_template(context).await
    }
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

async fn run_command_template(context: TurnContext<'_>) -> Result<AgentRun> {
    let command = &context.config.command;
    if command.is_empty() {
        return Err(anyhow!("CLI Agent 未配置命令模板"));
    }
    let model_args = &context.config.model_args;
    let model = context.config.model.trim();
    let session_id = context.spec.session_id.clone().unwrap_or_default();
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
    let prompt = prompt_for_run(context.spec, resume_aware);
    let mut args = Vec::new();
    let mut has_prompt = false;
    for arg in &command[1..] {
        match arg.as_str() {
            "{prompt}" => {
                args.push(prompt.clone());
                has_prompt = true;
            }
            "{session_id}" => args.push(session_id.clone()),
            "{job_dir}" => args.push(context.spec.job_dir.to_string_lossy().to_string()),
            "{cwd}" => args.push(context.spec.cwd.to_string_lossy().to_string()),
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
    let mut run = run_process(&command[0], &args, context.spec, context.timeout, stdin).await?;
    if resume_aware {
        run.session_id = context.spec.session_id.clone();
    }
    Ok(run)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::adapter::{AgentSpec, JobProgress};
    use super::*;
    use crate::models::JOB_ROLE_EXECUTOR;

    fn spec(continuation: bool, session_id: Option<&str>) -> AgentSpec {
        AgentSpec {
            job_id: "job_test".into(),
            role: JOB_ROLE_EXECUTOR.into(),
            cwd: PathBuf::from("/tmp/repo"),
            job_dir: PathBuf::from("/tmp/job"),
            prompt: "继续".into(),
            original_prompt: "原始提示".into(),
            session_id: session_id.map(str::to_string),
            continuation,
            progress: JobProgress::new(),
        }
    }

    #[test]
    fn validate_rejects_an_empty_command_template() {
        let error = CliAdapter::validate(&AgentConfig::default()).unwrap_err();
        assert_eq!(error.to_string(), "CLI Agent 必须配置命令模板");
        assert!(CliAdapter::requires_command_template(
            &AgentConfig::default()
        ));
    }

    #[test]
    fn a_fresh_run_receives_the_original_prompt_before_the_continuation() {
        let run = spec(true, Some("session-1"));
        assert_eq!(prompt_for_run(&run, false), "原始提示\n\n继续");
        assert_eq!(prompt_for_run(&run, true), "继续");
    }
}
