//! Persisted records and HTTP payloads. Database rows keep JSON columns as text;
//! view types expose decoded values to the API.

use serde::{Deserialize, Serialize};

use crate::failure::FailureSummary;
use crate::usage::{AvailabilityConfig, ProjectUsage, UsageLimitConfig, UsageWindow};

pub const ROLE_PLANNER: &str = "planner";
pub const ROLE_EXECUTOR: &str = "executor";
pub const ROLE_VERIFIER: &str = "verifier";

pub const AGENT_MOCK: &str = "mock";
pub const AGENT_OPENCODE: &str = "opencode";
pub const AGENT_CLI: &str = "cli";
pub const AGENT_CODEX: &str = "codex";
pub const AGENT_WORKBUDDY: &str = "workbuddy";

pub const STAGE_ANALYSIS: &str = "analysis";
pub const STAGE_EXECUTING: &str = "executing";
pub const STAGE_SUMMARIZING: &str = "summarizing";
pub const STAGE_COMPLETED: &str = "completed";

pub const STATUS_BLOCKED: &str = "blocked";
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_QUEUED: &str = "queued";
pub const STATUS_PAUSING: &str = "pausing";
pub const STATUS_PAUSED: &str = "paused";
pub const STATUS_ARCHIVED: &str = "archived";
pub const STATUS_CANCELED: &str = "canceled";

pub const REPLAN_SOURCE_HUMAN: &str = "human";
pub const REPLAN_SOURCE_EXECUTOR: &str = "executor";

/// 项目报告评议中，人工提交的意见以该角色持久化到 messages，
/// 供多轮重写时进入总结作业的提示词，并作为回溯审计记录。
pub const REPORT_MESSAGE_ROLE: &str = "review";

pub const HUMAN_GATE_MILESTONE: &str = "milestone";
pub const HUMAN_GATE_MILESTONE_REPLANNING: &str = "milestone_replanning";

pub const MILESTONE_DISPOSITION_RETRY: &str = "retry";
pub const MILESTONE_DISPOSITION_REPLAN: &str = "replan";

pub const ARBITRATION_RETRY: &str = "retry";
pub const ARBITRATION_SWITCH_AGENT: &str = "switch_agent";
pub const ARBITRATION_GUIDE: &str = "guide";

pub const TASK_STAGE_PENDING: &str = "pending";
pub const TASK_STAGE_EXECUTING: &str = "executing";
pub const TASK_STAGE_VERIFYING: &str = "verifying";
pub const TASK_STAGE_COMPLETED: &str = "completed";

pub const JOB_STATUS_RUNNING: &str = "running";
pub const JOB_STATUS_SUCCEEDED: &str = "succeeded";
pub const JOB_STATUS_FAILED: &str = "failed";
pub const JOB_STATUS_CANCELED: &str = "canceled";

pub const FAILURE_RATE_LIMIT: &str = "rate_limit";
pub const FAILURE_TRANSIENT: &str = "transient";
pub const FAILURE_PERMANENT: &str = "permanent";
pub const FAILURE_TASK: &str = "task";

pub const JOB_ROLE_ANALYSIS: &str = "analysis";
pub const JOB_ROLE_DIRECT: &str = "direct";
pub const JOB_ROLE_EXECUTOR: &str = ROLE_EXECUTOR;
pub const JOB_ROLE_VERIFIER: &str = ROLE_VERIFIER;
pub const JOB_ROLE_SUMMARY: &str = "summary";

pub const CONFIRM_MANUAL: &str = "manual";
pub const CONFIRM_AUTO: &str = "auto";

pub const COMPLETION_TASKS: &str = "tasks";
pub const COMPLETION_DIRECT: &str = "direct";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AgentRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub model: String,
    pub roles: String,
    #[allow(dead_code)]
    priority: i64,
    pub max_concurrency: i64,
    pub enabled: i64,
    pub command_json: Option<String>,
    pub model_args_json: Option<String>,
    pub cooldown_until: Option<String>,
    pub last_error: Option<String>,
    pub failure_kind: Option<String>,
    pub transient_failures: i64,
    pub availability_json: Option<String>,
    pub usage_limit_json: Option<String>,
    pub usage_reset_at: Option<String>,
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
    pub max_concurrency: i64,
    pub enabled: bool,
    pub command: Vec<String>,
    pub model_args: Vec<String>,
    pub cooldown_until: Option<String>,
    pub last_error: Option<String>,
    pub failure_kind: Option<String>,
    pub failure_summary: Option<FailureSummary>,
    pub transient_failures: i64,
    pub availability: Option<AvailabilityConfig>,
    pub usage_limit: Option<UsageLimitConfig>,
    pub usage_window: Option<UsageWindow>,
    pub created_at: String,
    pub updated_at: String,
}

impl AgentView {
    pub fn from_row(row: AgentRow) -> Self {
        let failure_summary = crate::failure::summarize(row.last_error.as_deref());
        Self {
            id: row.id,
            name: row.name,
            kind: row.kind,
            model: row.model,
            roles: parse_string_list(&row.roles),
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
            failure_kind: row.failure_kind,
            failure_summary,
            transient_failures: row.transient_failures,
            availability: crate::usage::parse_availability(row.availability_json.as_deref()),
            usage_limit: crate::usage::parse_usage_limit(row.usage_limit_json.as_deref()),
            usage_window: None,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(dead_code)]
pub struct ProjectAgentRow {
    pub project_id: String,
    pub role: String,
    pub agent_id: String,
    pub agent_name: String,
    pub priority: i64,
    pub enabled: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectAgentEntry {
    pub agent_id: String,
    pub name: String,
    pub kind: String,
    pub model: String,
    pub priority: i64,
    pub enabled: bool,
    pub agent_enabled: bool,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub max_concurrency: i64,
    pub cooldown_until: Option<String>,
    pub last_error: Option<String>,
    pub failure_kind: Option<String>,
    pub failure_summary: Option<FailureSummary>,
    pub transient_failures: i64,
    pub availability: Option<AvailabilityConfig>,
    pub availability_reason: Option<String>,
    pub usage_limit: Option<UsageLimitConfig>,
    pub usage_window: Option<UsageWindow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectRolePool {
    pub role: String,
    pub entries: Vec<ProjectAgentEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectAgentPool {
    pub project_id: String,
    pub roles: Vec<ProjectRolePool>,
}

/// 任务专用 Agent 池：由项目 Agent 池对应角色快照派生，按任务独立维护。
/// 任务只使用执行者与验收者两个角色；规划者始终使用项目池。
#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(dead_code)]
pub struct TaskAgentRow {
    pub task_id: String,
    pub role: String,
    pub agent_id: String,
    pub agent_name: String,
    pub priority: i64,
    pub enabled: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl TaskAgentRow {
    pub fn into_project_row(self) -> ProjectAgentRow {
        ProjectAgentRow {
            project_id: String::new(),
            role: self.role,
            agent_id: self.agent_id,
            agent_name: self.agent_name,
            priority: self.priority,
            enabled: self.enabled,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// 任务 Agent 池视图：roles 与 ProjectAgentPool 相同形态，便于前端复用面板。
#[derive(Debug, Clone, Serialize)]
pub struct TaskAgentPool {
    pub project_id: String,
    pub task_id: String,
    pub roles: Vec<ProjectRolePool>,
}

/// 默认团队是系统级模板：新建项目的项目 Agent 池按它快照初始化。
/// 视图复用 ProjectAgentPool 的形态（project_id 固定为 "default"）。
#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(dead_code)]
pub struct DefaultTeamRow {
    pub role: String,
    pub agent_id: String,
    pub agent_name: String,
    pub priority: i64,
    pub enabled: i64,
    pub created_at: String,
    pub updated_at: String,
}

pub fn default_team_row_to_pool(row: DefaultTeamRow) -> ProjectAgentRow {
    ProjectAgentRow {
        project_id: "default".to_string(),
        role: row.role,
        agent_id: row.agent_id,
        agent_name: row.agent_name,
        priority: row.priority,
        enabled: row.enabled,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalTask {
    pub key: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisProposal {
    pub kind: String,
    pub summary: String,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub tasks: Vec<ProposalTask>,
    #[serde(default)]
    pub plan: Vec<String>,
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default)]
    pub skip_confirmation: bool,
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
    pub replan_reason: Option<String>,
    pub replan_source: Option<String>,
    pub replan_requested_at: Option<String>,
    pub confirm_mode: String,
    #[serde(skip_serializing)]
    pub proposal: Option<String>,
    pub confirmed_at: Option<String>,
    pub completion_mode: String,
    pub merged_at: Option<String>,
    pub merged_commit: Option<String>,
    /// Project-level agents skipped by the next project job dispatch, mirroring
    /// the task-level pair.
    #[serde(skip_serializing)]
    pub avoid_agent_id: Option<String>,
    #[serde(skip_serializing)]
    pub avoid_agent_role: Option<String>,
    /// Agent targeted by a manual retry of the current project phase. The
    /// scheduler lets it bypass its cooldown once; a successful dispatch
    /// clears its backoff and this marker.
    #[serde(skip_serializing)]
    pub retry_agent_id: Option<String>,
    #[serde(skip_serializing)]
    pub retry_role: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TaskRow {
    pub id: String,
    pub project_id: String,
    pub seq: i64,
    pub queue_order: i64,
    pub key: String,
    pub title: String,
    pub description: String,
    pub acceptance: String,
    pub deps: String,
    pub parent_id: Option<String>,
    pub milestone: i64,
    pub human_gate: Option<String>,
    pub forced_agent_id: Option<String>,
    pub forced_agent_role: Option<String>,
    pub executor_agent_id: Option<String>,
    pub verifier_agent_id: Option<String>,
    pub stage: String,
    pub status: Option<String>,
    pub agent_id: Option<String>,
    pub last_agent_id: Option<String>,
    pub avoid_agent_id: Option<String>,
    pub avoid_agent_role: Option<String>,
    pub retry_agent_id: Option<String>,
    pub retry_role: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub artifacts_released_at: Option<String>,
    pub attempts: i64,
    pub rejections: i64,
    pub last_error: Option<String>,
    pub last_failure_kind: Option<String>,
    pub waiting_reason: Option<String>,
    pub result_summary: Option<String>,
    pub verifier_issues: String,
    pub guidance: String,
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
    pub failure_kind: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub cost: Option<f64>,
    pub duration_ms: Option<i64>,
    pub usage_known: i64,
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
    pub job_id: Option<String>,
    pub kind: String,
    pub message: String,
    pub created_at: String,
}

/// List view of an audit event. `message` is bounded to the preview length and
/// `message_length` reports the stored character count so the UI can offer the
/// full document on demand.
#[derive(Debug, Clone, Serialize)]
pub struct EventView {
    pub id: i64,
    pub project_id: Option<String>,
    pub task_id: Option<String>,
    pub job_id: Option<String>,
    pub kind: String,
    pub message: String,
    pub message_length: usize,
    pub message_truncated: bool,
    pub created_at: String,
}

impl EventView {
    pub fn from_row(row: EventRow) -> Self {
        let preview = crate::event_preview::preview(&row.message);
        Self {
            id: row.id,
            project_id: row.project_id,
            task_id: row.task_id,
            job_id: row.job_id,
            kind: row.kind,
            message: preview.message,
            message_length: preview.message_length,
            message_truncated: preview.message_truncated,
            created_at: row.created_at,
        }
    }

    /// Full event view used by the single-event endpoint.
    pub fn full(row: EventRow) -> Self {
        let message_length = row.message.chars().count();
        Self {
            id: row.id,
            project_id: row.project_id,
            task_id: row.task_id,
            job_id: row.job_id,
            kind: row.kind,
            message: row.message,
            message_length,
            message_truncated: false,
            created_at: row.created_at,
        }
    }
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
    pub replan_reason: Option<String>,
    pub replan_source: Option<String>,
    pub confirm_mode: String,
    pub completion_mode: String,
    pub merged_at: Option<String>,
    pub merged_commit: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub id: String,
    pub project_id: String,
    pub seq: i64,
    pub queue_order: i64,
    pub queue_position: Option<i64>,
    pub key: String,
    pub title: String,
    pub description: String,
    pub acceptance: Vec<String>,
    pub deps: Vec<String>,
    pub parent_id: Option<String>,
    pub milestone: bool,
    pub human_gate: Option<String>,
    pub forced_agent_id: Option<String>,
    pub forced_agent_role: Option<String>,
    pub executor_agent_id: Option<String>,
    pub verifier_agent_id: Option<String>,
    pub stage: String,
    pub status: Option<String>,
    pub agent_id: Option<String>,
    pub last_agent_id: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub artifacts_released_at: Option<String>,
    pub attempts: i64,
    pub rejections: i64,
    pub last_error: Option<String>,
    pub last_failure_kind: Option<String>,
    pub waiting_reason: Option<String>,
    pub result_summary: Option<String>,
    pub verifier_issues: Vec<String>,
    pub guidance: String,
    pub created_at: String,
    pub updated_at: String,
}

impl TaskView {
    pub fn from_row(row: TaskRow) -> Self {
        Self {
            id: row.id,
            project_id: row.project_id,
            seq: row.seq,
            queue_order: row.queue_order,
            queue_position: None,
            key: row.key,
            title: row.title,
            description: row.description,
            acceptance: parse_string_list(&row.acceptance),
            deps: parse_string_list(&row.deps),
            parent_id: row.parent_id,
            milestone: row.milestone != 0,
            human_gate: row.human_gate,
            forced_agent_id: row.forced_agent_id,
            forced_agent_role: row.forced_agent_role,
            executor_agent_id: row.executor_agent_id,
            verifier_agent_id: row.verifier_agent_id,
            stage: row.stage,
            status: row.status,
            agent_id: row.agent_id,
            last_agent_id: row.last_agent_id,
            worktree: row.worktree,
            branch: row.branch,
            artifacts_released_at: row.artifacts_released_at,
            attempts: row.attempts,
            rejections: row.rejections,
            last_error: row.last_error,
            last_failure_kind: row.last_failure_kind,
            waiting_reason: row.waiting_reason,
            result_summary: row.result_summary,
            verifier_issues: parse_string_list(&row.verifier_issues),
            guidance: row.guidance,
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
    pub failure_kind: Option<String>,
    pub failure_summary: Option<FailureSummary>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub cost: Option<f64>,
    pub duration_ms: Option<i64>,
    pub usage_known: bool,
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
    pub failure_kind: Option<String>,
    pub failure_summary: Option<FailureSummary>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub cost: Option<f64>,
    pub duration_ms: Option<i64>,
    pub usage_known: bool,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectDetail {
    pub project: ProjectRow,
    pub merged_at: Option<String>,
    pub merged_commit: Option<String>,
    /// 目标分支已经包含项目分支提交；包括 DOMO 记录的合入和人工在外部完成的合入。
    pub merged_into_base: bool,
    pub tasks: Vec<TaskView>,
    pub jobs: Vec<JobSummary>,
    pub messages: Vec<MessageRow>,
    pub events: Vec<EventView>,
    pub agents: Vec<AgentView>,
    pub agent_pool: ProjectAgentPool,
    pub usage: ProjectUsage,
    pub proposal: Option<AnalysisProposal>,
}

#[derive(Debug, Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    pub repo_path: String,
    pub base_branch: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AnalysisMessageRequest {
    pub message: String,
    pub confirm_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AnalysisPolicyRequest {
    pub mode: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct ConfirmAnalysisRequest {}

#[derive(Debug, Deserialize)]
pub struct CreateTaskRequest {
    pub key: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub milestone: bool,
}

#[derive(Debug, Deserialize)]
pub struct TaskGuidanceRequest {
    #[serde(alias = "message", alias = "content", alias = "text")]
    pub guidance: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReplanRequest {
    #[serde(alias = "message", alias = "content", alias = "text")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MilestoneRequest {
    #[serde(alias = "enabled", alias = "value")]
    pub milestone: Option<bool>,
    pub action: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MilestoneAcceptRequest {
    #[serde(alias = "feedback", alias = "comment", alias = "message")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MilestoneRejectRequest {
    #[serde(alias = "feedback", alias = "comment", alias = "message")]
    pub reason: String,
    pub guidance: Option<String>,
    #[serde(
        alias = "option",
        alias = "disposition",
        alias = "decision",
        alias = "mode"
    )]
    pub action: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ArbitrationRequest {
    pub action: String,
    pub agent_id: Option<String>,
    #[serde(alias = "message", alias = "content", alias = "text")]
    pub guidance: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReportRequest {
    pub action: String,
    #[serde(alias = "message", alias = "comment", alias = "text")]
    pub feedback: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAgentRequest {
    pub name: String,
    pub kind: String,
    pub model: Option<String>,
    pub roles: Option<Vec<String>>,
    pub max_concurrency: Option<i64>,
    pub enabled: Option<bool>,
    pub command: Option<Vec<String>>,
    pub model_args: Option<Vec<String>>,
    pub availability: Option<AvailabilityConfig>,
    pub usage_limit: Option<UsageLimitConfig>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAgentRequest {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub model: Option<String>,
    pub roles: Option<Vec<String>>,
    pub max_concurrency: Option<i64>,
    pub enabled: Option<bool>,
    pub command: Option<Vec<String>>,
    pub model_args: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_json")]
    pub availability: Option<Option<AvailabilityConfig>>,
    #[serde(default, deserialize_with = "deserialize_optional_json")]
    pub usage_limit: Option<Option<UsageLimitConfig>>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectAgentUpdateRequest {
    pub enabled: Option<bool>,
    pub direction: Option<String>,
    /// 0 基目标位置：提供时把该 Agent 移动到角色内该位置，并把角色内优先级重排为 0..n-1。
    pub position: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ProjectPauseRequest {
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub mode: Option<String>,
}

impl ProjectPauseRequest {
    pub fn forced(&self) -> bool {
        self.force
            || matches!(
                self.mode.as_deref().map(str::trim),
                Some("force") | Some("forced") | Some("强制")
            )
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct TaskPauseRequest {
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub mode: Option<String>,
}

impl TaskPauseRequest {
    pub fn forced(&self) -> bool {
        self.force
            || matches!(
                self.mode.as_deref().map(str::trim),
                Some("force") | Some("forced") | Some("强制")
            )
    }
}

#[derive(Debug, Deserialize)]
pub struct TaskQueueRequest {
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub position: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct AddProjectAgentRequest {
    pub role: String,
    pub agent_id: String,
}

#[derive(Debug, Deserialize)]
pub struct TaskAgentsRequest {
    #[serde(default, deserialize_with = "deserialize_optional_agent")]
    pub executor_agent_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_agent")]
    pub verifier_agent_id: Option<Option<String>>,
}

/// Missing fields leave the current override unchanged; explicit null or an
/// empty string clears it.
pub fn deserialize_optional_agent<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(Some(value.and_then(|text| {
        let text = text.trim();
        if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        }
    })))
}

/// Distinguishes a missing configuration field from an explicit null so a PATCH
/// can either preserve or clear the persisted configuration.
pub fn deserialize_optional_json<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    let value = Option::<T>::deserialize(deserializer)?;
    Ok(Some(value))
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
