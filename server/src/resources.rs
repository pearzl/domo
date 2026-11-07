//! Project-owned knowledge and artifact indexes. Knowledge is immutable records
//! in a separate Git repository; ordinary delivery files are never versioned here.
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::SqlitePool;

use crate::{
    domain::{new_id, now},
    git,
    models::*,
    state::AppState,
};

pub async fn initialize(pool: &SqlitePool) -> crate::db::DbResult<()> {
    sqlx::raw_sql(
        r#"
CREATE TABLE IF NOT EXISTS shared_records (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), task_id TEXT, job_id TEXT,
 memory_no INTEGER NOT NULL, body TEXT NOT NULL, supersedes TEXT NOT NULL DEFAULT '',
 invalidated_by TEXT, invalidated_at TEXT,
 commit_id TEXT, created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_shared_no ON shared_records(project_id, memory_no);
CREATE INDEX IF NOT EXISTS idx_shared_project ON shared_records(project_id, created_at);
CREATE TABLE IF NOT EXISTS job_shared_versions (
 job_id TEXT NOT NULL REFERENCES jobs(id), version TEXT NOT NULL, record_ids TEXT NOT NULL,
 created_at TEXT NOT NULL, PRIMARY KEY(job_id, version)
);
CREATE TABLE IF NOT EXISTS artifacts (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), task_id TEXT, job_id TEXT,
 path TEXT NOT NULL, kind TEXT NOT NULL, format TEXT NOT NULL, verification TEXT NOT NULL,
 commit_id TEXT, contract_id INTEGER, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
 UNIQUE(project_id, path)
);
CREATE TABLE IF NOT EXISTS artifact_versions (
 id INTEGER PRIMARY KEY AUTOINCREMENT, artifact_id TEXT NOT NULL REFERENCES artifacts(id),
 commit_id TEXT NOT NULL, job_id TEXT, recorded_at TEXT NOT NULL,
 UNIQUE(artifact_id, commit_id)
);
CREATE TABLE IF NOT EXISTS merge_requests (
 project_id TEXT PRIMARY KEY REFERENCES projects(id), agent_id TEXT NOT NULL,
 state TEXT NOT NULL, job_id TEXT, error TEXT, base_commit TEXT NOT NULL,
 project_commit TEXT NOT NULL, requested_at TEXT NOT NULL
);
"#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub fn shared_repo(project: &ProjectRow) -> PathBuf {
    Path::new(&project.workspace).join("shared")
}

async fn ensure_repo(project: &ProjectRow) -> Result<()> {
    let repo = shared_repo(project);
    if !repo.join(".git").is_dir() {
        tokio::fs::create_dir_all(repo.join("records")).await?;
        git::run_git(&repo, &["init", "-q", "-b", "main"]).await?;
        git::run_git(
            &repo,
            &[
                "-c",
                "user.name=DOMO",
                "-c",
                "user.email=domo@localhost",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "domo: project knowledge",
            ],
        )
        .await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedInput {
    pub text: String,
    #[serde(default)]
    pub supersedes: String,
}

/// Persist intent before Git. Re-entry writes the same immutable file, commits
/// any outstanding records and only then publishes their commit references.
async fn settle_shared(state: &AppState, project: &ProjectRow) -> Result<()> {
    ensure_repo(project).await?;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, body FROM shared_records WHERE project_id = ? AND commit_id IS NULL ORDER BY created_at, id",
    ).bind(&project.id).fetch_all(&state.db).await?;
    if rows.is_empty() {
        return Ok(());
    }
    let repo = shared_repo(project);
    for (id, _) in &rows {
        let record = shared_record(&state.db, &project.id, id).await?;
        let path = repo.join("records").join(format!("{id}.json"));
        let bytes = serde_json::to_vec_pretty(&record)?;
        if path.exists() && tokio::fs::read(&path).await? != bytes {
            bail!("共享记录文件冲突：{id}，保留现场，不覆盖已有文件");
        }
        tokio::fs::write(path, bytes).await?;
    }
    git::commit_all(&repo, "domo: publish project records").await?;
    let commit = git::commit_of(&repo, "HEAD").await?;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    for (id, _) in rows {
        sqlx::query("UPDATE shared_records SET commit_id = ? WHERE id = ? AND commit_id IS NULL")
            .bind(&commit)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO events(project_id, kind, message, payload, created_at) VALUES(?, 'shared_record_committed', ?, ?, ?)")
            .bind(&project.id).bind("项目共享信息已提交")
            .bind(json!({"version": 1, "record_id": id, "commit": commit}).to_string()).bind(now()).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn shared_record(pool: &SqlitePool, project_id: &str, id: &str) -> Result<Value> {
    type Row = (
        String,
        Option<String>,
        Option<String>,
        i64,
        String,
        String,
        Option<String>,
        String,
    );
    let (id, task, job, memory_no, body, supersedes, invalidated_by, at): Row = sqlx::query_as(
        "SELECT id, task_id, job_id, memory_no, body, supersedes, invalidated_by, created_at FROM shared_records WHERE project_id = ? AND id = ?",
    ).bind(project_id).bind(id).fetch_one(pool).await?;
    Ok(json!({
        "id": id,
        "no": format!("M{memory_no}"),
        "text": body,
        "task_id": task,
        "job_id": job,
        "supersedes": supersedes,
        "invalidated_by": invalidated_by,
        "created_at": at,
    }))
}

pub async fn publish(
    state: &AppState,
    project: &ProjectRow,
    task_id: Option<&str>,
    job_id: Option<&str>,
    records: &[SharedInput],
) -> Result<()> {
    if records.len() > 5 {
        bail!("一次最多提交 5 条项目记忆");
    }
    let _guard = state.git_lock.lock().await;
    settle_shared(state, project).await?;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let mut seen = std::collections::HashSet::new();
    for record in records {
        let text = record.text.trim();
        if text.is_empty() {
            bail!("项目记忆正文不能为空");
        }
        if text.chars().count() > 120 {
            bail!("项目记忆正文不能超过 120 字");
        }
        if !seen.insert(text.to_string()) {
            continue;
        }
        let target = if record.supersedes.trim().is_empty() {
            None
        } else {
            let no = record.supersedes.trim();
            let number: i64 = no
                .strip_prefix('M')
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| anyhow!("supersedes 必须是 M 编号，例如 M12"))?;
            let target_id: Option<String> = sqlx::query_scalar(
                "SELECT id FROM shared_records WHERE project_id = ? AND memory_no = ? AND commit_id IS NOT NULL",
            )
            .bind(&project.id)
            .bind(number)
            .fetch_optional(&mut *tx)
            .await?;
            Some((
                no.to_string(),
                target_id.ok_or_else(|| anyhow!("supersedes 指向的项目记忆不存在：{no}"))?,
            ))
        };
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM events WHERE project_id = ? AND job_id IS ? AND kind = 'shared_submission' AND message = ?",
        )
        .bind(&project.id)
        .bind(job_id)
        .bind(text)
        .fetch_one(&mut *tx)
        .await?;
        if exists != 0 {
            continue;
        }
        let memory_no: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(memory_no), 0) + 1 FROM shared_records WHERE project_id = ?",
        )
        .bind(&project.id)
        .fetch_one(&mut *tx)
        .await?;
        let id = new_id("info");
        sqlx::query(
            "INSERT INTO shared_records(id, project_id, task_id, job_id, memory_no, body, supersedes, created_at) VALUES(?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&project.id)
        .bind(task_id)
        .bind(job_id)
        .bind(memory_no)
        .bind(text)
        .bind(target.as_ref().map(|(no, _)| no.as_str()).unwrap_or(""))
        .bind(now())
        .execute(&mut *tx)
        .await?;
        if let Some((_, target_id)) = &target {
            sqlx::query(
                "UPDATE shared_records SET invalidated_by = ?, invalidated_at = ? WHERE id = ? AND project_id = ? AND invalidated_by IS NULL",
            )
            .bind(&id)
            .bind(now())
            .bind(target_id)
            .bind(&project.id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("INSERT INTO events(project_id, task_id, job_id, kind, message, payload, created_at) VALUES(?, ?, ?, 'shared_submission', ?, ?, ?)")
            .bind(&project.id).bind(task_id).bind(job_id).bind(text)
            .bind(json!({"version": 1, "record_id": id, "memory_no": format!("M{memory_no}"), "supersedes": target.as_ref().map(|(no, _)| no)}).to_string())
            .bind(now()).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    settle_shared(state, project).await
}

pub async fn shared_snapshot(
    state: &AppState,
    project: &ProjectRow,
    job: Option<&JobRow>,
    _task: Option<&TaskRow>,
) -> Result<Value> {
    let _guard = state.git_lock.lock().await;
    settle_shared(state, project).await?;
    let repo = shared_repo(project);
    let version = git::commit_of(&repo, "HEAD").await?;
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM shared_records WHERE project_id = ? AND commit_id IS NOT NULL AND invalidated_by IS NULL ORDER BY memory_no",
    )
    .bind(&project.id)
    .fetch_all(&state.db)
    .await?;
    let mut records = Vec::new();
    for id in &ids {
        records.push(shared_record(&state.db, &project.id, id).await?);
    }
    let superseded_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM shared_records WHERE project_id = ? AND invalidated_by IS NOT NULL ORDER BY memory_no",
    )
    .bind(&project.id)
    .fetch_all(&state.db)
    .await?;
    let record_index = job.map(|_| {
        records
            .iter()
            .map(|record| {
                json!({
                    "no": record["no"],
                    "text": record["text"],
                    "task_id": record["task_id"],
                    "created_at": record["created_at"],
                    "supersedes": record["supersedes"],
                })
            })
            .collect::<Vec<_>>()
    });
    if let Some(job) = job {
        sqlx::query("INSERT OR IGNORE INTO job_shared_versions(job_id, version, record_ids, created_at) VALUES(?, ?, ?, ?)")
            .bind(&job.id).bind(&version).bind(json!(ids).to_string()).bind(now()).execute(&state.db).await?;
    }
    let mut snapshot = json!({"version": version, "repository": repo, "records": records, "superseded_ids": superseded_ids,
        "read_endpoint": format!("/api/projects/{}/shared", project.id)});
    if let Some(index) = record_index {
        snapshot["record_index"] = json!(index);
    }
    Ok(snapshot)
}

pub fn safe_relative(path: &str) -> Result<PathBuf> {
    let value = Path::new(path);
    if path.is_empty()
        || path.contains('\\')
        || path.contains(':')
        || value
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || value.components().any(|c| c.as_os_str() == ".git")
    {
        bail!("路径必须是工作区内不含 .git、..、盘符的相对路径");
    }
    Ok(value.to_path_buf())
}

pub async fn contained(root: &Path, path: &str) -> Result<PathBuf> {
    let relative = safe_relative(path)?;
    let base = tokio::fs::canonicalize(root).await?;
    let candidate = root.join(relative);
    let mut existing = candidate.as_path();
    while !existing.exists() {
        existing = existing.parent().ok_or_else(|| anyhow!("路径不存在"))?;
    }
    if !tokio::fs::canonicalize(existing).await?.starts_with(&base) {
        bail!("路径通过符号链接逃逸工作区");
    }
    Ok(candidate)
}

pub async fn recover(state: &AppState) -> Result<()> {
    let projects = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects")
        .fetch_all(&state.db)
        .await?;
    for project in projects {
        let _guard = state.git_lock.lock().await;
        settle_shared(state, &project)
            .await
            .with_context(|| format!("恢复共享仓库 {}", project.id))?;
    }
    Ok(())
}

pub fn delivery_root(project: &ProjectRow) -> PathBuf {
    if project.is_git() {
        crate::paths::project_repo(Path::new(&project.workspace))
    } else {
        PathBuf::from(&project.repo_path)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ArtifactInput {
    pub path: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub verification: String,
}

async fn validate_artifact(root: &Path, input: &ArtifactInput) -> Result<()> {
    let path = contained(root, &input.path).await?;
    let metadata = tokio::fs::symlink_metadata(&path)
        .await
        .with_context(|| format!("登记的成果或参考输入不存在：{}", input.path))?;
    if !metadata.is_file() && !metadata.is_dir() {
        bail!("登记对象必须是普通文件或目录：{}", input.path);
    }
    Ok(())
}

/// Validate declarations before committing delivery files so the same job can
/// correct invalid metadata. An absent list never means "index all files".
async fn declared_artifacts(raw: &str, cwd: &Path) -> Result<Vec<ArtifactInput>> {
    let result = crate::domain::extract_json(raw).ok_or_else(|| anyhow!("结果不是 JSON 对象"))?;
    let records: Vec<ArtifactInput> = match result.get("artifacts") {
        Some(value) => {
            if value
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item.get("kind").is_some()))
            {
                bail!("artifacts 不再接受 kind；成果只有一种类型");
            }
            serde_json::from_value(value.clone()).context("artifacts 必须是产物声明数组")?
        }
        None => Vec::new(),
    };
    if records.len() > 4 {
        bail!("artifacts 最多 4 条");
    }
    let mut paths = std::collections::HashSet::new();
    for record in &records {
        if !paths.insert(&record.path) {
            bail!("artifacts 中的路径重复：{}", record.path);
        }
        validate_artifact(cwd, record).await?;
    }
    Ok(records)
}

pub async fn validate_delivery_artifacts(raw: &str, cwd: &Path, is_git: bool) -> Result<()> {
    let records = declared_artifacts(raw, cwd).await?;
    if is_git {
        for record in records {
            let tracked = git::run_git(
                cwd,
                &["--literal-pathspecs", "ls-files", "--", &record.path],
            )
            .await?;
            if tracked.trim().is_empty() {
                bail!("产物未被 Git 跟踪：{}", record.path);
            }
        }
    }
    Ok(())
}

struct ArtifactSource<'a> {
    task_id: Option<&'a str>,
    job_id: Option<&'a str>,
    contract_id: Option<i64>,
    root: &'a Path,
    commit: Option<&'a str>,
}

async fn save_artifact_batch(
    state: &AppState,
    project: &ProjectRow,
    source: &ArtifactSource<'_>,
    inputs: &[ArtifactInput],
) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    // Check the complete batch before publishing any metadata or version index.
    for input in inputs {
        if let Some(commit) = source.commit {
            let kind = git::run_git(
                source.root,
                &["cat-file", "-t", &format!("{commit}:{}", input.path)],
            )
            .await?;
            if !matches!(kind.trim(), "blob" | "tree") {
                bail!("产物不是提交中的文件或目录：{}", input.path);
            }
        }
    }
    let timestamp = now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    for input in inputs {
        let artifact_id: String = sqlx::query_scalar("INSERT INTO artifacts(id, project_id, task_id, job_id, path, kind, format, verification, commit_id, contract_id, created_at, updated_at) \
            VALUES(?, ?, ?, ?, ?, '', ?, ?, ?, ?, ?, ?) ON CONFLICT(project_id, path) DO UPDATE SET \
            kind = '', format = excluded.format, verification = excluded.verification, task_id = excluded.task_id, job_id = excluded.job_id, \
            commit_id = excluded.commit_id, contract_id = excluded.contract_id, updated_at = excluded.updated_at RETURNING id")
            .bind(new_id("art")).bind(&project.id).bind(source.task_id).bind(source.job_id).bind(&input.path).bind(&input.format).bind(&input.verification)
            .bind(source.commit).bind(source.contract_id).bind(&timestamp).bind(&timestamp).fetch_one(&mut *tx).await?;
        if let Some(commit) = source.commit {
            sqlx::query("INSERT OR IGNORE INTO artifact_versions(artifact_id, commit_id, job_id, recorded_at) VALUES(?, ?, ?, ?)")
                .bind(&artifact_id).bind(commit).bind(source.job_id).bind(&timestamp).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn artifact_snapshot(state: &AppState, project: &ProjectRow) -> Result<Value> {
    type Row = (
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        String,
    );
    let rows: Vec<Row> = sqlx::query_as("SELECT id, path, format, verification, task_id, job_id, commit_id, contract_id, updated_at FROM artifacts WHERE project_id = ? ORDER BY path")
        .bind(&project.id).fetch_all(&state.db).await?;
    let items = rows.into_iter().map(|(id, path, format, verification, task_id, job_id, commit, contract, at)| json!({
        "id": id, "path": path, "format": format, "verification": verification,
        "task_id": task_id, "job_id": job_id, "commit": commit, "contract_id": contract, "updated_at": at
    })).collect::<Vec<_>>();
    Ok(json!({"root": delivery_root(project), "items": items}))
}

pub async fn settle_artifacts(
    state: &AppState,
    project: &ProjectRow,
    job: &JobRow,
    cwd: &Path,
    raw: &str,
) -> Result<()> {
    if job.role != JOB_ROLE_EXECUTOR {
        return Ok(());
    }
    let inputs = declared_artifacts(raw, cwd).await?;
    let commit = if project.is_git() {
        Some(git::commit_of(cwd, "HEAD").await?)
    } else {
        None
    };
    save_artifact_batch(
        state,
        project,
        &ArtifactSource {
            task_id: job.task_id.as_deref(),
            job_id: Some(&job.id),
            contract_id: job.contract_id,
            root: cwd,
            commit: commit.as_deref(),
        },
        &inputs,
    )
    .await
}

pub async fn block_workspace(
    state: &AppState,
    project: &ProjectRow,
    job: &JobRow,
    reason: &str,
) -> Result<()> {
    let transition = crate::transitions::Transitions::lock(state).await;
    if transition.project_result(project).await?.is_none() {
        return Ok(());
    }
    if let Some(task_id) = &job.task_id {
        let task =
            sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ? AND project_id = ?")
                .bind(task_id)
                .bind(&project.id)
                .fetch_optional(&state.db)
                .await?;
        if !task.is_some_and(|task| {
            task.stage != TASK_STAGE_COMPLETED
                && task.status.as_deref() != Some(STATUS_CANCELED)
                && task.contract_id == job.contract_id
        }) {
            return Ok(());
        }
    }
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("UPDATE jobs SET status = 'failed', failure_kind = 'task', error = ?, finished_at = ? WHERE id = ? AND status = 'running'")
        .bind(reason).bind(now()).bind(&job.id).execute(&mut *tx).await?;
    if let Some(task) = &job.task_id {
        sqlx::query("UPDATE tasks SET status = CASE WHEN status IN ('paused', 'pausing', 'canceled') THEN status ELSE 'blocked' END, last_failure_kind = 'workspace', last_error = ?, updated_at = ? WHERE id = ?")
            .bind(reason).bind(now()).bind(task).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE projects SET status = CASE WHEN status IN ('paused', 'pausing', 'archived') THEN status ELSE 'blocked' END, blocked_reason = ?, blocked_kind = 'workspace', updated_at = ? WHERE id = ?")
        .bind(reason).bind(now()).bind(&project.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn artifact_declarations_are_explicit_and_validate_the_complete_list() {
        let root = std::env::temp_dir().join(format!("domo-resources-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(root.join("outputs"))
            .await
            .unwrap();
        tokio::fs::write(root.join("outputs/report.md"), "report")
            .await
            .unwrap();
        for raw in [r#"{"commit_files":[]}"#, r#"{"artifacts":[]}"#] {
            assert!(declared_artifacts(raw, &root).await.unwrap().is_empty());
        }
        let valid = r#"{"artifacts":[{"path":"outputs/report.md"},{"path":"outputs"}]}"#;
        assert_eq!(declared_artifacts(valid, &root).await.unwrap().len(), 2);
        for value in [
            json!(null),
            json!({"path":"outputs/report.md"}),
            json!([{"path":"missing.md"}]),
            json!([{"path":"../outside.md"}]),
            json!([{"path":".git/config"}]),
            json!([{"path":"outputs/report.md"}, {"path":"outputs/report.md"}]),
            json!([{"path":"outputs/report.md", "kind":"delivery"}]),
            json!([{"path":"outputs/report.md"}, {"path":"outputs"}, {"path":"outputs/report.md"}, {"path":"outputs"}, {"path":"outputs"}]),
        ] {
            let raw = json!({"artifacts":value}).to_string();
            assert!(declared_artifacts(&raw, &root).await.is_err(), "{raw}");
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn git_artifact_declarations_require_files_in_the_delivery_snapshot() {
        let root = std::env::temp_dir().join(format!("domo-resources-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(root.join("outputs"))
            .await
            .unwrap();
        git::run_git(&root, &["init", "-q", "-b", "main"])
            .await
            .unwrap();
        tokio::fs::write(root.join("outputs/seed.md"), "seed")
            .await
            .unwrap();
        git::commit_all(&root, "fixture").await.unwrap();
        tokio::fs::write(root.join("outputs/report.md"), "report")
            .await
            .unwrap();
        let raw = json!({"artifacts":[{"path":"outputs/report.md"}]}).to_string();
        assert!(validate_delivery_artifacts(&raw, &root, true)
            .await
            .is_err());
        git::commit_all(&root, "deliver report").await.unwrap();
        validate_delivery_artifacts(&raw, &root, true)
            .await
            .unwrap();
        let raw = json!({"artifacts":[{"path":"outputs/missing.md"}]}).to_string();
        assert!(validate_delivery_artifacts(&raw, &root, true)
            .await
            .is_err());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artifact_declarations_reject_symlink_escape() {
        let root = std::env::temp_dir().join(format!("domo-resources-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(root.join("workspace"))
            .await
            .unwrap();
        tokio::fs::write(root.join("outside.md"), "outside")
            .await
            .unwrap();
        std::os::unix::fs::symlink(&root, root.join("workspace/escape")).unwrap();
        let raw = r#"{"artifacts":[{"path":"escape/outside.md","kind":"input"}]}"#;
        assert!(declared_artifacts(raw, &root.join("workspace"))
            .await
            .is_err());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
