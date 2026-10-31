//! Agent adapters. A job is one process (or one in-process mock) that changes
//! the workspace and writes a result JSON file. `kinds` is the single registry
//! of supported agent types; `execute` dispatches to the matching adapter.

mod adapter;
mod cli;
mod codex;
mod mock;
mod opencode;
mod process;
mod workbuddy;

use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::models::AgentRow;

pub use adapter::{
    AgentAdapter, AgentConfig, AgentRun, AgentSpec, FinishContext, JobProgress, JobUsage,
    TurnContext,
};
pub use cli::CliAdapter;
pub use codex::CodexAdapter;
pub use mock::MockAdapter;
pub use opencode::OpencodeAdapter;
pub use process::kill_job_dir;
pub use workbuddy::WorkbuddyAdapter;

/// Declares the registered adapter types once. `KINDS` and every dispatch hook
/// below are generated from this list, so adding an agent kind means writing
/// its adapter module and naming it here.
macro_rules! adapter_registry {
    ($($adapter:ty),+ $(,)?) => {
        /// Every supported agent kind, in display order.
        const KINDS: &[&str] = &[$(<$adapter as AgentAdapter>::KIND),+];

        fn dispatch_requires_command_template(
            kind: &str,
            config: &AgentConfig,
        ) -> Option<bool> {
            $(
                if kind == <$adapter as AgentAdapter>::KIND {
                    return Some(<$adapter as AgentAdapter>::requires_command_template(config));
                }
            )+
            None
        }

        fn dispatch_validate_config(kind: &str, config: &AgentConfig) -> Option<Result<()>> {
            $(
                if kind == <$adapter as AgentAdapter>::KIND {
                    return Some(<$adapter as AgentAdapter>::validate(config));
                }
            )+
            None
        }

        async fn dispatch_execute(
            agent: &AgentRow,
            spec: &AgentSpec,
            timeout: Duration,
        ) -> Option<Result<AgentRun>> {
            let config = AgentConfig::from_row(agent);
            $(
                if agent.kind == <$adapter as AgentAdapter>::KIND {
                    return Some(run_with::<$adapter>(&config, spec, timeout).await);
                }
            )+
            None
        }
    };
}

adapter_registry!(
    MockAdapter,
    OpencodeAdapter,
    CodexAdapter,
    WorkbuddyAdapter,
    CliAdapter,
);

/// Agent kinds accepted by the registry, in display order.
pub fn kinds() -> &'static [&'static str] {
    KINDS
}

/// Whether the adapter registered for `kind` needs an explicit command
/// template. Unknown kinds report `false`; callers validate the kind first.
pub fn requires_command_template(kind: &str) -> bool {
    let config = AgentConfig {
        kind: kind.to_string(),
        ..AgentConfig::default()
    };
    dispatch_requires_command_template(kind, &config).unwrap_or(false)
}

/// Rejects settings the adapter registered for `kind` cannot run. Unknown
/// kinds report `Ok(())`; callers validate the kind first.
pub fn validate_config(kind: &str, command: &[String]) -> Result<()> {
    let config = AgentConfig {
        kind: kind.to_string(),
        command: command.to_vec(),
        ..AgentConfig::default()
    };
    dispatch_validate_config(kind, &config).unwrap_or(Ok(()))
}

pub async fn execute(agent: &AgentRow, spec: &AgentSpec, timeout: Duration) -> Result<AgentRun> {
    match dispatch_execute(agent, spec, timeout).await {
        Some(result) => result,
        None => Err(anyhow!("未知 Agent 类型: {}", agent.kind)),
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
    use std::time::Duration;

    use super::*;

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
    fn registry_lists_each_adapter_kind_once() {
        let kinds = kinds();
        assert_eq!(kinds.len(), 5);
        for kind in [
            <MockAdapter as AgentAdapter>::KIND,
            <OpencodeAdapter as AgentAdapter>::KIND,
            <CodexAdapter as AgentAdapter>::KIND,
            <WorkbuddyAdapter as AgentAdapter>::KIND,
            <CliAdapter as AgentAdapter>::KIND,
        ] {
            assert_eq!(kinds.iter().filter(|item| **item == kind).count(), 1);
        }
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
