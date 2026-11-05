//! Business-facing contracts for one agent type. A job is one process (or one
//! in-process mock) that changes the workspace and writes a result JSON file.

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;

use crate::models::AgentRow;
use crate::usage::UsageRecord;

/// Liveness signal for one job. Reader tasks update it whenever the agent
/// writes to stdout/stderr; the runner compares it against the stall threshold.
#[derive(Debug, Clone)]
pub struct JobProgress {
    inner: std::sync::Arc<JobProgressInner>,
}

#[derive(Debug)]
struct JobProgressInner {
    started: std::time::Instant,
    last_activity_ms: AtomicU64,
    stdout_bytes: AtomicU64,
    stderr_bytes: AtomicU64,
}

impl JobProgress {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(JobProgressInner {
                started: std::time::Instant::now(),
                last_activity_ms: AtomicU64::new(0),
                stdout_bytes: AtomicU64::new(0),
                stderr_bytes: AtomicU64::new(0),
            }),
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.inner.started.elapsed().as_millis() as u64
    }

    pub fn touch(&self) {
        self.inner
            .last_activity_ms
            .store(self.elapsed_ms(), Ordering::Relaxed);
    }

    pub fn idle_for(&self) -> Duration {
        Duration::from_millis(
            self.elapsed_ms()
                .saturating_sub(self.inner.last_activity_ms.load(Ordering::Relaxed)),
        )
    }

    pub fn record_stdout(&self, bytes: usize) {
        self.inner
            .stdout_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
        self.touch();
    }

    pub fn record_stderr(&self, bytes: usize) {
        self.inner
            .stderr_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
        self.touch();
    }

    pub fn output_bytes(&self) -> (u64, u64) {
        (
            self.inner.stdout_bytes.load(Ordering::Relaxed),
            self.inner.stderr_bytes.load(Ordering::Relaxed),
        )
    }
}

impl Default for JobProgress {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct AgentRun {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i64>,
    pub timed_out: bool,
    pub stalled: bool,
    pub session_id: Option<String>,
    pub usage: Option<UsageRecord>,
    /// Execution failure reported inside the agent's output stream. A CLI can
    /// exit 0 and still fail, so this reason outranks the exit code.
    pub reported_error: Option<String>,
}

impl AgentRun {
    /// True when the process did not finish on its own terms, regardless of
    /// whether the watchdog or the OS ended it.
    pub fn process_failure(&self) -> bool {
        self.timed_out || self.stalled || self.exit_code != Some(0)
    }
}

#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub job_id: String,
    pub role: String,
    pub cwd: PathBuf,
    pub job_dir: PathBuf,
    pub prompt: String,
    pub original_prompt: String,
    pub session_id: Option<String>,
    pub continuation: bool,
    pub progress: JobProgress,
}

/// Agent settings an adapter needs before a run starts.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct AgentConfig {
    pub kind: String,
    pub model: String,
    pub command: Vec<String>,
    pub model_args: Vec<String>,
}

impl AgentConfig {
    pub fn from_row(agent: &AgentRow) -> Self {
        Self {
            kind: agent.kind.clone(),
            model: agent.model.clone(),
            command: parse_arg_list(agent.command_json.as_deref()),
            model_args: parse_arg_list(agent.model_args_json.as_deref()),
        }
    }
}

fn parse_arg_list(raw: Option<&str>) -> Vec<String> {
    raw.map(|raw| serde_json::from_str::<Vec<String>>(raw).unwrap_or_default())
        .unwrap_or_default()
}

/// Executable for a CLI-backed adapter, overridable per kind through the
/// environment so tests and alternate installations can point at their own bin.
pub(super) fn bin_override(key: &str, fallback: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

pub(super) fn json_i64(value: Option<&serde_json::Value>) -> Option<i64> {
    value.and_then(|item| item.as_i64())
}

/// Session id for CLI templates that require one. The job id already carries a
/// UUID, so the derived value is stable per job execution and never collides
/// across retries.
pub(super) fn derived_session_id(job_id: &str) -> String {
    let suffix = job_id.strip_prefix("job_").unwrap_or(job_id);
    let hex: String = suffix.chars().filter(|ch| ch.is_ascii_hexdigit()).collect();
    if hex.len() >= 32 {
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    } else {
        format!("ses_domo{suffix}")
    }
}

#[cfg(test)]
pub(super) fn args_contain_pair(args: &[String], first: &str, second: &str) -> bool {
    args.windows(2)
        .any(|window| window[0] == first && window[1] == second)
}

/// Usage an adapter attributes to one job. `PerTurn` keeps whatever the run
/// reported; `Authoritative` replaces it with the adapter's own record.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub enum JobUsage {
    PerTurn,
    Authoritative(Option<UsageRecord>),
}

/// Input for one turn of work. The adapter owns argument expansion and prompt
/// delivery, so callers only describe the job and its settings.
#[allow(dead_code)]
pub struct TurnContext<'a> {
    pub config: &'a AgentConfig,
    pub spec: &'a AgentSpec,
    pub timeout: Duration,
}

/// Input for usage settlement after the final turn.
#[allow(dead_code)]
pub struct FinishContext<'a> {
    pub agent: &'a AgentRow,
    pub job_id: &'a str,
    pub session_id: Option<&'a str>,
    pub process_failed: bool,
    pub shutting_down: bool,
}

/// One agent kind, distinguished only by business capabilities.
#[allow(dead_code)]
pub trait AgentAdapter: Default + Send + Sync + 'static {
    /// Registry key for this agent kind.
    const KIND: &'static str;

    /// Whether a run needs a configured command template.
    fn requires_command_template(config: &AgentConfig) -> bool;

    /// Rejects settings this adapter cannot run.
    fn validate(config: &AgentConfig) -> Result<()>;

    /// Session id a job runs under, given an existing continuation session.
    fn session_for_job(job_id: &str, existing: Option<&str>) -> Option<String>;

    fn supports_resume(_config: &AgentConfig) -> bool {
        false
    }

    fn diagnose(_config: &AgentConfig, run: &AgentRun) -> super::diagnostic::Diagnosis {
        super::diagnostic::diagnose(super::diagnostic::Protocol::Generic, run)
    }

    /// Recover an actual or explicitly assigned session after process loss.
    fn recover_session(
        config: &AgentConfig,
        job_id: &str,
        stdout: &str,
        previous: Option<&str>,
    ) -> Option<String> {
        if !Self::supports_resume(config) {
            return None;
        }
        super::diagnostic::session(stdout).or_else(|| Self::session_for_job(job_id, previous))
    }

    /// Runs one turn, including this kind's argument and prompt delivery.
    fn run_turn<'a>(
        &'a self,
        context: TurnContext<'a>,
    ) -> impl Future<Output = Result<AgentRun>> + Send + 'a;

    /// Reports the usage this adapter accounts for the job.
    fn finish_job<'a>(
        &'a self,
        _context: FinishContext<'a>,
    ) -> impl Future<Output = JobUsage> + Send + 'a {
        async { JobUsage::PerTurn }
    }
}
