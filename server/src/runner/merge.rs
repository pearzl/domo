//! User-selected merge assistance is a project job, not a task or acceptance.
use super::{agent::run_agent_job, common::load_project, lifecycle::finish_job};
use crate::{
    agents::{self, AgentSpec},
    domain::now,
    git,
    models::*,
    paths,
    state::AppState,
};
use anyhow::{anyhow, Result};
use serde_json::json;
use std::path::PathBuf;

pub(super) async fn run(state: &AppState, job: &JobRow) -> Result<()> {
    let project = load_project(&state.db, &job.project_id)
        .await?
        .ok_or_else(|| anyhow!("项目不存在"))?;
    let agent = crate::db::load_agent(&state.db, job.agent_id.as_deref().unwrap_or_default())
        .await?
        .ok_or_else(|| anyhow!("Agent 不存在"))?;
    let (base, original): (String, String) = sqlx::query_as("SELECT base_commit, project_commit FROM merge_requests WHERE project_id = ? AND job_id = ?")
        .bind(&project.id).bind(&job.id).fetch_one(&state.db).await?;
    let job_dir = paths::job_dir(&PathBuf::from(&project.workspace), &job.id);
    tokio::fs::create_dir_all(&job_dir).await?;
    let cwd = job_dir.join("resolution");
    let branch = format!("domo-merge/{}", job.id);
    let repo = PathBuf::from(&project.repo_path);
    {
        let _guard = state.git_lock.lock().await;
        git::create_task_worktree(&repo, &original, &cwd, &branch).await?;
    }
    let initial = git::run_git(
        &cwd,
        &[
            "-c",
            "user.name=DOMO",
            "-c",
            "user.email=domo@localhost",
            "rebase",
            &base,
        ],
    )
    .await;
    let prompt = crate::prompts::merge_prompt(
        &project.name,
        &project.base_branch,
        &original,
        &base,
        &cwd.to_string_lossy(),
        &job_dir.join("result.json").to_string_lossy(),
        &format!("{initial:?}"),
    );
    tokio::fs::write(job_dir.join("prompt.md"), &prompt).await?;
    tokio::fs::write(job_dir.join("context.json"), json!({"job_id": job.id, "role": "merge_conflict", "base_commit": base, "project_commit": original, "cwd": cwd}).to_string()).await?;
    sqlx::query("UPDATE jobs SET prompt = ?, started_at = ? WHERE id = ?")
        .bind(&prompt)
        .bind(now())
        .bind(&job.id)
        .execute(&state.db)
        .await?;
    state.register_job(job_dir.clone());
    let progress = agents::JobProgress::new();
    let spec = AgentSpec {
        job_id: job.id.clone(),
        role: job.role.clone(),
        cwd: cwd.clone(),
        job_dir: job_dir.clone(),
        prompt: prompt.clone(),
        original_prompt: prompt,
        session_id: agents::session_for_job(&agent, &job.id),
        continuation: false,
        progress: progress.clone(),
    };
    let mut run = run_agent_job(
        state,
        job,
        &job_dir,
        &agent,
        &spec,
        &progress,
        state.cfg.job_timeout,
    )
    .await?;
    let usage = agents::finish_job(agents::FinishContext {
        agent: &agent,
        job_id: &job.id,
        session_id: run.session_id.as_deref().or(spec.session_id.as_deref()),
        process_failed: run.process_failure(),
        shutting_down: state.is_shutting_down(),
    })
    .await;
    run.usage = agents::apply_usage(run.usage, usage);
    state.unregister_job(&job_dir);
    let raw = tokio::fs::read_to_string(job_dir.join("result.json"))
        .await
        .ok();
    let mut failure = if run.process_failure() {
        Some(format!("Agent 进程失败：{}", run.stderr))
    } else {
        raw.as_deref()
            .and_then(crate::domain::extract_json)
            .and_then(|value| {
                if value["kind"] == "done" {
                    None
                } else {
                    Some(
                        value["reason"]
                            .as_str()
                            .unwrap_or("无有效冲突处理结果")
                            .to_string(),
                    )
                }
            })
    };
    if raw
        .as_deref()
        .and_then(crate::domain::extract_json)
        .is_none()
    {
        failure = Some("没有有效 result.json".into());
    }
    if failure.is_none() {
        let _publication_guard = state.schedule_lock.lock().await;
        let current: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(&state.db)
            .await?;
        if current != JOB_STATUS_RUNNING {
            return Ok(());
        }
        // The agent may leave its resolved files staged, but DOMO owns publication.
        if cwd.join(".git").exists() {
            let _ = git::run_git(&cwd, &["add", "-A"]).await;
            let gitdir = git::run_git(&cwd, &["rev-parse", "--absolute-git-dir"]).await?;
            if PathBuf::from(gitdir.trim()).join("rebase-merge").exists()
                || PathBuf::from(gitdir.trim()).join("rebase-apply").exists()
            {
                if let Err(error) = git::run_git(
                    &cwd,
                    &[
                        "-c",
                        "user.name=DOMO",
                        "-c",
                        "user.email=domo@localhost",
                        "-c",
                        "core.editor=true",
                        "rebase",
                        "--continue",
                    ],
                )
                .await
                {
                    failure = Some(format!("rebase 尚未完成：{error:#}"));
                }
            }
        }
        if failure.is_none() {
            let prepared = git::commit_of(&cwd, "HEAD").await?;
            if let Err(error) =
                crate::api::publish_assisted(state, &project, &base, &original, &prepared).await
            {
                failure = Some(format!("合入失败：{error:?}"));
            }
        }
    }
    if run.usage.is_none() {
        run.usage = raw
            .as_deref()
            .and_then(crate::usage::UsageRecord::from_result);
    }
    let status = if failure.is_none() {
        JOB_STATUS_SUCCEEDED
    } else {
        JOB_STATUS_FAILED
    };
    if !finish_job(state, job, status, raw, &run, failure.as_deref(), None).await? {
        return Ok(());
    }
    sqlx::query(
        "UPDATE merge_requests SET state = ?, error = ? WHERE project_id = ? AND job_id = ? AND state = 'running'",
    )
    .bind(if failure.is_none() {
        "succeeded"
    } else {
        "failed"
    })
    .bind(failure)
    .bind(&project.id)
    .bind(&job.id)
    .execute(&state.db)
    .await?;
    if git::status_porcelain(&cwd).await?.trim().is_empty() {
        let tip = git::commit_of(&cwd, "HEAD").await?;
        let _guard = state.git_lock.lock().await;
        git::run_git(&cwd, &["checkout", "--detach", &tip]).await?;
        git::archive_branch(&repo, &branch, &tip).await?;
    }
    // Keep the resolution checkout and its commits as audit evidence, including
    // failed attempts. They are included in project backup and archival review.
    Ok(())
}
