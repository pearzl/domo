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

/// Every supported agent kind, in display order. Adding an agent type means
/// implementing `AgentAdapter` and adding its `KIND` here.
const KINDS: [&str; 5] = [
    <MockAdapter as AgentAdapter>::KIND,
    <OpencodeAdapter as AgentAdapter>::KIND,
    <CodexAdapter as AgentAdapter>::KIND,
    <WorkbuddyAdapter as AgentAdapter>::KIND,
    <CliAdapter as AgentAdapter>::KIND,
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
    let config = AgentConfig::from_row(agent);
    if kind == <MockAdapter as AgentAdapter>::KIND {
        return run_with::<MockAdapter>(&config, spec, timeout).await;
    }
    if kind == <OpencodeAdapter as AgentAdapter>::KIND {
        return run_with::<OpencodeAdapter>(&config, spec, timeout).await;
    }
    if kind == <CliAdapter as AgentAdapter>::KIND {
        return run_with::<CliAdapter>(&config, spec, timeout).await;
    }
    if kind == <CodexAdapter as AgentAdapter>::KIND {
        return run_with::<CodexAdapter>(&config, spec, timeout).await;
    }
    if kind == <WorkbuddyAdapter as AgentAdapter>::KIND {
        return run_with::<WorkbuddyAdapter>(&config, spec, timeout).await;
    }
    Err(anyhow!("未知 Agent 类型: {kind}"))
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
