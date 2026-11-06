//! Lifecycle commands share one scheduling critical section. Authority comes
//! from rows loaded inside this scope, never from an earlier HTTP/job snapshot.
//! Git and agent work keep their existing resource protocols; no lock spans an
//! agent invocation.

use tokio::sync::MutexGuard;

use crate::{models::*, state::AppState};

#[derive(Clone, Copy)]
pub enum ProjectAction {
    Pause,
    Resume,
    Analysis,
    Archive,
    ManageTask,
    AddTask,
    Report,
    Merge,
}

#[derive(Clone, Copy)]
pub enum TaskAction {
    Pause,
    Resume,
    Reorder,
    Guide,
    Milestone,
    Arbitrate,
}

#[derive(Debug, thiserror::Error)]
pub enum TransitionError {
    #[error("项目不存在")]
    ProjectMissing,
    #[error("任务不存在")]
    TaskMissing,
    #[error("{0}")]
    Rejected(&'static str),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

type Result<T> = std::result::Result<T, TransitionError>;

pub struct Transitions<'a> {
    state: &'a AppState,
    _guard: MutexGuard<'a, ()>,
}

impl<'a> Transitions<'a> {
    pub async fn lock(state: &'a AppState) -> Self {
        Self {
            state,
            _guard: state.schedule_lock.lock().await,
        }
    }

    async fn load_project(&self, id: &str) -> Result<ProjectRow> {
        sqlx::query_as("SELECT * FROM projects WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.state.db)
            .await?
            .ok_or(TransitionError::ProjectMissing)
    }

    pub async fn project(&self, id: &str, action: ProjectAction) -> Result<ProjectRow> {
        let project = self.load_project(id).await?;
        check_project(&project, action)?;
        Ok(project)
    }

    pub async fn task(
        &self,
        project_id: &str,
        id: &str,
        action: TaskAction,
    ) -> Result<(ProjectRow, TaskRow)> {
        let project = self.project(project_id, ProjectAction::ManageTask).await?;
        let task: TaskRow = sqlx::query_as("SELECT * FROM tasks WHERE id = ? AND project_id = ?")
            .bind(id)
            .bind(&project.id)
            .fetch_optional(&self.state.db)
            .await?
            .ok_or(TransitionError::TaskMissing)?;
        check_task(&task, action)?;
        Ok((project, task))
    }

    /// In-flight work may drain while a replan is being discussed, but cannot
    /// mutate an archived/completed project or a different confirmed contract.
    pub async fn project_result(&self, expected: &ProjectRow) -> Result<Option<ProjectRow>> {
        let current = self.load_project(&expected.id).await?;
        Ok((current.status.as_deref() != Some(STATUS_ARCHIVED)
            && current.stage != STAGE_COMPLETED
            && current.contract_id == expected.contract_id
            && (current.stage == expected.stage || current.replan_active != 0))
            .then_some(current))
    }

    pub async fn task_result(
        &self,
        project: &ProjectRow,
        expected: &TaskRow,
    ) -> Result<Option<TaskRow>> {
        if self.project_result(project).await?.is_none() {
            return Ok(None);
        }
        let current: Option<TaskRow> =
            sqlx::query_as("SELECT * FROM tasks WHERE id = ? AND project_id = ?")
                .bind(&expected.id)
                .bind(&project.id)
                .fetch_optional(&self.state.db)
                .await?;
        Ok(current.filter(|task| {
            task.status.as_deref() != Some(STATUS_CANCELED)
                && task.stage != TASK_STAGE_COMPLETED
                && task.stage == expected.stage
                && task.contract_id == expected.contract_id
        }))
    }
}

fn check_project(project: &ProjectRow, action: ProjectAction) -> Result<()> {
    if matches!(action, ProjectAction::Archive) {
        return Ok(());
    }
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(TransitionError::Rejected("项目已归档"));
    }
    if matches!(action, ProjectAction::AddTask) {
        return Ok(());
    }
    if matches!(action, ProjectAction::Merge) {
        return if project.stage == STAGE_COMPLETED {
            Ok(())
        } else {
            Err(TransitionError::Rejected(
                "项目尚未完成，只有完成阶段的项目可以合入",
            ))
        };
    }
    if project.stage == STAGE_COMPLETED {
        return Err(TransitionError::Rejected("项目已完成"));
    }
    match action {
        ProjectAction::Resume
            if !matches!(
                project.status.as_deref(),
                Some(STATUS_PAUSING | STATUS_PAUSED)
            ) =>
        {
            Err(TransitionError::Rejected("项目当前没有暂停"))
        }
        ProjectAction::Analysis if project.stage != STAGE_ANALYSIS => {
            Err(TransitionError::Rejected("当前阶段不允许项目分析操作"))
        }
        ProjectAction::Analysis
            if matches!(
                project.status.as_deref(),
                Some(STATUS_PAUSING | STATUS_PAUSED)
            ) =>
        {
            Err(TransitionError::Rejected("项目已暂停，请先恢复"))
        }
        ProjectAction::Report if project.stage != STAGE_SUMMARIZING => {
            Err(TransitionError::Rejected("项目不在总结阶段"))
        }
        _ => Ok(()),
    }
}

fn check_task(task: &TaskRow, action: TaskAction) -> Result<()> {
    if task.status.as_deref() == Some(STATUS_CANCELED) {
        return Err(TransitionError::Rejected("任务已取消"));
    }
    if task.stage == TASK_STAGE_COMPLETED {
        return Err(TransitionError::Rejected("任务已完成"));
    }
    match action {
        TaskAction::Resume
            if !matches!(task.status.as_deref(), Some(STATUS_PAUSING | STATUS_PAUSED)) =>
        {
            Err(TransitionError::Rejected("任务当前没有暂停"))
        }
        TaskAction::Reorder
            if !matches!(
                task.stage.as_str(),
                TASK_STAGE_PENDING | TASK_STAGE_EXECUTING
            ) || task.attempts > 0
                || task.rejections > 0
                || !matches!(
                    task.status.as_deref(),
                    None | Some(STATUS_QUEUED | STATUS_PAUSED)
                ) =>
        {
            Err(TransitionError::Rejected(
                "只有尚未开始的任务可以调整排队顺序",
            ))
        }
        TaskAction::Arbitrate if task.status.as_deref() != Some(STATUS_BLOCKED) => {
            Err(TransitionError::Rejected("只有阻塞中的任务可以裁决"))
        }
        _ => Ok(()),
    }
}
