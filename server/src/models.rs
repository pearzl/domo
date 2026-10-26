//! Persisted records and HTTP payloads. Database rows keep JSON columns as text;
//! view types expose decoded values to the API.

use serde::{Deserialize, Serialize};

pub const ROLE_PLANNER: &str = "planner";
pub const ROLE_EXECUTOR: &str = "executor";
pub const ROLE_VERIFIER: &str = "verifier";

pub const AGENT_MOCK: &str = "mock";
pub const AGENT_OPENCODE: &str = "opencode";
pub const AGENT_CLI: &str = "cli";

pub const STAGE_GOAL_CONFIRM: &str = "goal_confirm";
pub const STAGE_PLANNING: &str = "planning";
pub const STAGE_EXECUTING: &str = "executing";
pub const STAGE_SUMMARIZING: &str = "summarizing";
pub const STAGE_COMPLETED: &str = "completed";

pub const STATUS_BLOCKED: &str = "blocked";
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_QUEUED: &str = "queued";
pub const STATUS_PAUSING: &str = "pausing";
pub const STATUS_PAUSED: &str = "paused";
pub const STATUS_ARCHIVED: &str = "archived";

pub const TASK_STAGE_PENDING: &str = "pending";
pub const TASK_STAGE_EXECUTING: &str = "executing";
pub const TASK_STAGE_VERIFYING: &str = "verifying";
pub const TASK_STAGE_COMPLETED: &str = "completed";

pub const JOB_STATUS_RUNNING: &str = "running";
pub const JOB_STATUS_SUCCEEDED: &str = "succeeded";
pub const JOB_STATUS_FAILED: &str = "failed";
pub const JOB_STATUS_CANCELED: &str = "canceled";

pub const JOB_ROLE_GOAL: &str = "goal";
pub const JOB_ROLE_EXECUTOR: &str = ROLE_EXECUTOR;
pub const JOB_ROLE_VERIFIER: &str = ROLE_VERIFIER;
pub const JOB_ROLE_SUMMARY: &str = "summary";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AgentRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub model: String,
    pub roles: String,
    pub priority: i64,
    pub max_concurrency: i64,
    pub enabled: i64,
    pub command_json: Option<String>,
    pub model_args_json: Option<String>,
    pub cooldown_until: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentView {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub model: String,
    pub roles: Vec<String>,
    pub priority: i64,
    pub max_concurrency: i64,
    pub enabled: bool,
    pub command: Vec<String>,
    pub model_args: Vec<String>,
    pub cooldown_until: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl AgentView {
    pub fn from_row(row: AgentRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            kind: row.kind,
            model: row.model,
            roles: parse_string_list(&row.roles),
            priority: row.priority,
            max_concurrency: row.max_concurrency.max(1),
            enabled: row.enabled != 0,
            command: row
                .command_json
                .as_deref()
                .map(parse_string_list)
                .unwrap_or_default(),
            model_args: row
                .model_args_json
                .as_deref()
                .map(parse_string_list)
                .unwrap_or_default(),
            cooldown_until: row.cooldown_until,
            last_error: row.last_error,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    pub goal: String,
    pub repo_path: String,
    pub base_branch: String,
    pub branch: String,
    pub workspace: String,
    pub stage: String,
    pub status: Option<String>,
    pub blocked_reason: Option<String>,
    pub report: Option<String>,
    pub report_feedback: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TaskRow {
    pub id: String,
    pub project_id: String,
    pub seq: i64,
    pub key: String,
    pub title: String,
    pub description: String,
    pub acceptance: String,
    pub deps: String,
    pub stage: String,
    pub status: Option<String>,
    pub agent_id: Option<String>,
    pub last_agent_id: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub attempts: i64,
    pub rejections: i64,
    pub last_error: Option<String>,
    pub result_summary: Option<String>,
    pub verifier_issues: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct JobRow {
    pub id: String,
    pub project_id: String,
    pub task_id: Option<String>,
    pub role: String,
    pub agent_id: Option<String>,
    pub status: String,
    pub attempt: i64,
    pub prompt: String,
    pub result: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub error: Option<String>,
    pub exit_code: Option<i64>,
    pub pid: Option<i64>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct MessageRow {
    pub id: String,
    pub project_id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct EventRow {
    pub id: i64,
    pub project_id: Option<String>,
    pub task_id: Option<String>,
    pub kind: String,
    pub message: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub goal: String,
    pub repo_path: String,
    pub base_branch: String,
    pub branch: String,
    pub stage: String,
    pub status: Option<String>,
    pub blocked_reason: Option<String>,
    pub task_total: i64,
    pub task_completed: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub id: String,
    pub project_id: String,
    pub seq: i64,
    pub key: String,
    pub title: String,
    pub description: String,
    pub acceptance: Vec<String>,
    pub deps: Vec<String>,
    pub stage: String,
    pub status: Option<String>,
    pub agent_id: Option<String>,
    pub last_agent_id: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub attempts: i64,
    pub rejections: i64,
    pub last_error: Option<String>,
    pub result_summary: Option<String>,
    pub verifier_issues: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl TaskView {
    pub fn from_row(row: TaskRow) -> Self {
        Self {
            id: row.id,
            project_id: row.project_id,
            seq: row.seq,
            key: row.key,
            title: row.title,
            description: row.description,
            acceptance: parse_string_list(&row.acceptance),
            deps: parse_string_list(&row.deps),
            stage: row.stage,
            status: row.status,
            agent_id: row.agent_id,
            last_agent_id: row.last_agent_id,
            worktree: row.worktree,
            branch: row.branch,
            attempts: row.attempts,
            rejections: row.rejections,
            last_error: row.last_error,
            result_summary: row.result_summary,
            verifier_issues: parse_string_list(&row.verifier_issues),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JobSummary {
    pub id: String,
    pub project_id: String,
    pub task_id: Option<String>,
    pub role: String,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub status: String,
    pub attempt: i64,
    pub error: Option<String>,
    pub exit_code: Option<i64>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobView {
    pub id: String,
    pub project_id: String,
    pub task_id: Option<String>,
    pub role: String,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub status: String,
    pub attempt: i64,
    pub prompt: String,
    pub result: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub error: Option<String>,
    pub exit_code: Option<i64>,
    pub pid: Option<i64>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectDetail {
    pub project: ProjectRow,
    pub tasks: Vec<TaskView>,
    pub jobs: Vec<JobSummary>,
    pub messages: Vec<MessageRow>,
    pub events: Vec<EventRow>,
    pub agents: Vec<AgentView>,
}

#[derive(Debug, Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    pub goal: String,
    pub repo_path: String,
    pub base_branch: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConfirmGoalRequest {
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReportRequest {
    pub action: String,
    pub feedback: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAgentRequest {
    pub name: String,
    pub kind: String,
    pub model: Option<String>,
    pub roles: Option<Vec<String>>,
    pub priority: Option<i64>,
    pub max_concurrency: Option<i64>,
    pub enabled: Option<bool>,
    pub command: Option<Vec<String>>,
    pub model_args: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAgentRequest {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub model: Option<String>,
    pub roles: Option<Vec<String>>,
    pub priority: Option<i64>,
    pub max_concurrency: Option<i64>,
    pub enabled: Option<bool>,
    pub command: Option<Vec<String>>,
    pub model_args: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
}

pub fn parse_string_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

pub fn string_list_json(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".into())
}
