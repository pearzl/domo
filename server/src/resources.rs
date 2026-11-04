//! Project-owned knowledge and artifact indexes. Knowledge is immutable records
//! in a separate Git repository; ordinary delivery files are never versioned here.
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::{
    domain::{new_id, now},
    git,
    models::*,
    state::AppState,
};

pub async fn initialize(pool: &SqlitePool) -> crate::db::DbResult<()> {
    sqlx::raw_sql(r#"
CREATE TABLE IF NOT EXISTS shared_records (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), task_id TEXT, job_id TEXT,
 topic TEXT NOT NULL, body TEXT NOT NULL, scope TEXT NOT NULL, evidence TEXT NOT NULL,
 supersedes TEXT NOT NULL, conflicts TEXT NOT NULL, reason TEXT NOT NULL,
 commit_id TEXT, created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_shared_project ON shared_records(project_id, created_at);
CREATE TABLE IF NOT EXISTS job_shared_versions (
 job_id TEXT NOT NULL REFERENCES jobs(id), version TEXT NOT NULL, record_ids TEXT NOT NULL,
 created_at TEXT NOT NULL, PRIMARY KEY(job_id, version)
);
CREATE TABLE IF NOT EXISTS artifacts (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), task_id TEXT, job_id TEXT,
 path TEXT NOT NULL, kind TEXT NOT NULL, format TEXT NOT NULL, verification TEXT NOT NULL,
 fingerprint TEXT, commit_id TEXT, contract_id INTEGER, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
 UNIQUE(project_id, path)
);
CREATE TABLE IF NOT EXISTS workspace_checks (
 project_id TEXT PRIMARY KEY REFERENCES projects(id), interrupted INTEGER NOT NULL DEFAULT 0,
 reason TEXT, acknowledged_at TEXT, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS artifact_versions (
 id INTEGER PRIMARY KEY AUTOINCREMENT, artifact_id TEXT NOT NULL REFERENCES artifacts(id),
 commit_id TEXT NOT NULL, fingerprint TEXT, job_id TEXT, recorded_at TEXT NOT NULL,
 UNIQUE(artifact_id, commit_id)
);
CREATE TABLE IF NOT EXISTS file_permissions (
 project_id TEXT NOT NULL REFERENCES projects(id), task_id TEXT NOT NULL DEFAULT '', path TEXT NOT NULL,
 reason TEXT NOT NULL, PRIMARY KEY(project_id, task_id, path)
);
CREATE TABLE IF NOT EXISTS merge_requests (
 project_id TEXT PRIMARY KEY REFERENCES projects(id), agent_id TEXT NOT NULL,
 state TEXT NOT NULL, job_id TEXT, error TEXT, base_commit TEXT NOT NULL,
 project_commit TEXT NOT NULL, requested_at TEXT NOT NULL
);
"#).execute(pool).await?;
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
    pub topic: String,
    pub body: String,
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub supersedes: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub reason: String,
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
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    );
    let (id, task, job, topic, body, scope, evidence, supersedes, conflicts, reason, at): Row = sqlx::query_as(
        "SELECT id, task_id, job_id, topic, body, scope, evidence, supersedes, conflicts, reason, created_at FROM shared_records WHERE project_id = ? AND id = ?",
    ).bind(project_id).bind(id).fetch_one(pool).await?;
    Ok(
        json!({"id": id, "task_id": task, "job_id": job, "topic": topic, "body": body,
        "scope": serde_json::from_str::<Value>(&scope)?, "evidence": serde_json::from_str::<Value>(&evidence)?,
        "supersedes": serde_json::from_str::<Value>(&supersedes)?, "conflicts": serde_json::from_str::<Value>(&conflicts)?,
        "reason": reason, "created_at": at}),
    )
}

pub async fn publish(
    state: &AppState,
    project: &ProjectRow,
    task_id: Option<&str>,
    job_id: Option<&str>,
    records: &[SharedInput],
) -> Result<()> {
    if records.len() > 50 {
        bail!("一次最多提交 50 条共享记录");
    }
    let _guard = state.git_lock.lock().await;
    settle_shared(state, project).await?;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    for record in records {
        if record.topic.trim().is_empty()
            || record.body.trim().is_empty()
            || record.body.len() > 100_000
        {
            bail!("共享记录必须有主题和正文，正文不超过 100000 字节");
        }
        if record.evidence.is_empty() {
            bail!("共享记录必须引用证据或产物路径");
        }
        if (!record.supersedes.is_empty() || !record.conflicts.is_empty())
            && record.reason.trim().is_empty()
        {
            bail!("修正结论或登记分歧必须说明依据、替代关系和受影响任务");
        }
        for id in record.supersedes.iter().chain(&record.conflicts) {
            let found: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM shared_records WHERE project_id = ? AND id = ? AND commit_id IS NOT NULL")
                .bind(&project.id).bind(id).fetch_one(&mut *tx).await?;
            if found == 0 {
                bail!("共享信息引用不存在或尚未提交：{id}");
            }
        }
        // A repeated result from the same job must not duplicate its records.
        let encoded = serde_json::to_string(record)?;
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE project_id = ? AND job_id IS ? AND kind = 'shared_submission' AND message = ?")
            .bind(&project.id).bind(job_id).bind(&encoded).fetch_one(&mut *tx).await?;
        if exists != 0 {
            continue;
        }
        let id = new_id("info");
        sqlx::query("INSERT INTO shared_records(id, project_id, task_id, job_id, topic, body, scope, evidence, supersedes, conflicts, reason, created_at) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(&id).bind(&project.id).bind(task_id).bind(job_id).bind(&record.topic).bind(&record.body)
            .bind(serde_json::to_string(&record.scope)?).bind(serde_json::to_string(&record.evidence)?)
            .bind(serde_json::to_string(&record.supersedes)?).bind(serde_json::to_string(&record.conflicts)?)
            .bind(&record.reason).bind(now()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO events(project_id, task_id, job_id, kind, message, payload, created_at) VALUES(?, ?, ?, 'shared_submission', ?, ?, ?)")
            .bind(&project.id).bind(task_id).bind(job_id).bind(encoded)
            .bind(json!({"version": 1, "record_id": id, "scope": record.scope, "conflicts": record.conflicts}).to_string())
            .bind(now()).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    settle_shared(state, project).await
}

pub async fn shared_snapshot(
    state: &AppState,
    project: &ProjectRow,
    job: Option<&JobRow>,
    task: Option<&TaskRow>,
) -> Result<Value> {
    let _guard = state.git_lock.lock().await;
    settle_shared(state, project).await?;
    let repo = shared_repo(project);
    let version = git::commit_of(&repo, "HEAD").await?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM shared_records WHERE project_id = ? AND commit_id IS NOT NULL ORDER BY created_at, id")
        .bind(&project.id).fetch_all(&state.db).await?;
    let mut all = Vec::new();
    for id in ids {
        all.push(shared_record(&state.db, &project.id, &id).await?);
    }
    let superseded: std::collections::HashSet<String> = all
        .iter()
        .flat_map(|r| {
            r["supersedes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let related: Vec<Value> = all
        .into_iter()
        .filter(|r| {
            if job.is_none() {
                return true;
            }
            if superseded.contains(r["id"].as_str().unwrap_or_default()) {
                return false;
            }
            task.is_none()
                || r["scope"].as_array().is_none_or(|scope| {
                    scope.is_empty()
                        || scope.iter().any(|s| {
                            s == "*"
                                || task.is_some_and(|t| {
                                    s == &t.key
                                        || parse_string_list(&t.deps).iter().any(|dep| s == dep)
                                })
                        })
                })
        })
        .collect();
    if let Some(job) = job {
        sqlx::query("INSERT OR IGNORE INTO job_shared_versions(job_id, version, record_ids, created_at) VALUES(?, ?, ?, ?)")
            .bind(&job.id).bind(&version).bind(json!(related.iter().map(|r| &r["id"]).collect::<Vec<_>>()).to_string()).bind(now()).execute(&state.db).await?;
    }
    Ok(
        json!({"version": version, "repository": repo, "records": related, "superseded_ids": superseded,
        "read_endpoint": format!("/api/projects/{}/shared", project.id)}),
    )
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

fn fingerprint_sync(path: &Path) -> Result<String> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        bail!("登记路径不能是符号链接：{}", path.display());
    }
    let mut hash = Sha256::new();
    if meta.is_dir() {
        let mut children = std::fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            if child.file_name() == ".git" {
                continue;
            }
            hash.update(child.file_name().to_string_lossy().as_bytes());
            hash.update(fingerprint_sync(&child.path())?.as_bytes());
        }
    } else {
        let mut file = std::fs::File::open(path)?;
        let mut buffer = [0_u8; 65536];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

async fn fingerprint(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let path = path.to_path_buf();
    Ok(Some(
        tokio::task::spawn_blocking(move || fingerprint_sync(&path)).await??,
    ))
}

fn files_sync(root: &Path) -> Result<Vec<String>> {
    let mut pending = vec![root.to_path_buf()];
    let mut result = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_name() == ".git" {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                result.push(
                    entry
                        .path()
                        .strip_prefix(root)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
                if result.len() > 100_000 {
                    bail!("工作区超过 100000 个文件，请选择更具体的交付目录");
                }
            }
        }
    }
    result.sort();
    Ok(result)
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ArtifactInput {
    pub path: String,
    pub kind: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub verification: String,
}

pub async fn register_artifact(
    state: &AppState,
    project: &ProjectRow,
    task_id: Option<&str>,
    job_id: Option<&str>,
    input: &ArtifactInput,
) -> Result<()> {
    if !matches!(input.kind.as_str(), "input" | "intermediate" | "delivery") {
        bail!("产物 kind 必须为 input、intermediate 或 delivery");
    }
    let path = contained(&delivery_root(project), &input.path).await?;
    let checksum = fingerprint(&path).await?;
    let commit = if project.is_git() {
        let root = delivery_root(project);
        let commit = git::commit_of(&root, &project.branch).await?;
        let kind = git::run_git(
            &root,
            &["cat-file", "-t", &format!("{commit}:{}", input.path)],
        )
        .await?;
        if !matches!(kind.trim(), "blob" | "tree") {
            bail!("产物不是提交中的文件或目录");
        }
        Some(commit)
    } else {
        None
    };
    let timestamp = now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let artifact_id: String = sqlx::query_scalar("INSERT INTO artifacts(id, project_id, task_id, job_id, path, kind, format, verification, fingerprint, commit_id, contract_id, created_at, updated_at) \
        VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(project_id, path) DO UPDATE SET \
        kind = excluded.kind, format = excluded.format, verification = excluded.verification, updated_at = excluded.updated_at RETURNING id")
        .bind(new_id("art")).bind(&project.id).bind(task_id).bind(job_id).bind(&input.path).bind(&input.kind).bind(&input.format).bind(&input.verification)
        .bind(&checksum).bind(&commit).bind(project.contract_id).bind(&timestamp).bind(&timestamp).fetch_one(&mut *tx).await?;
    if let Some(commit) = commit {
        sqlx::query("INSERT OR IGNORE INTO artifact_versions(artifact_id, commit_id, fingerprint, job_id, recorded_at) VALUES(?, ?, ?, ?, ?)")
            .bind(&artifact_id).bind(commit).bind(checksum).bind(job_id).bind(&timestamp).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn initialize_inputs(state: &AppState, project: &ProjectRow) -> Result<()> {
    if project.is_git() {
        return Ok(());
    }
    let root = delivery_root(project);
    let files = tokio::task::spawn_blocking(move || files_sync(&root)).await??;
    for path in files {
        register_artifact(
            state,
            project,
            None,
            None,
            &ArtifactInput {
                path,
                kind: "input".into(),
                format: String::new(),
                verification: "保留原始输入".into(),
            },
        )
        .await?;
    }
    Ok(())
}

pub async fn artifact_snapshot(state: &AppState, project: &ProjectRow) -> Result<Value> {
    type Row = (
        String,
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
    let rows: Vec<Row> = sqlx::query_as("SELECT id, path, kind, format, verification, task_id, job_id, commit_id, contract_id, updated_at FROM artifacts WHERE project_id = ? ORDER BY kind, path")
        .bind(&project.id).fetch_all(&state.db).await?;
    let items = rows.into_iter().map(|(id, path, kind, format, verification, task_id, job_id, commit, contract, at)| json!({
        "id": id, "path": path, "kind": kind, "format": format, "verification": verification,
        "task_id": task_id, "job_id": job_id, "commit": commit, "contract_id": contract, "updated_at": at
    })).collect::<Vec<_>>();
    let check: Option<(i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT interrupted, reason, acknowledged_at FROM workspace_checks WHERE project_id = ?",
    )
    .bind(&project.id)
    .fetch_optional(&state.db)
    .await?;
    let permissions: Vec<(String, String, String)> =
        sqlx::query_as("SELECT task_id, path, reason FROM file_permissions WHERE project_id = ?")
            .bind(&project.id)
            .fetch_all(&state.db)
            .await?;
    Ok(
        json!({"root": delivery_root(project), "items": items, "check": check.map(|(interrupted, reason, at)| json!({"interrupted": interrupted != 0, "reason": reason, "acknowledged_at": at})),
        "permissions": permissions.into_iter().map(|(task, path, reason)| json!({"task_id": task, "path": path, "reason": reason})).collect::<Vec<_>>()}),
    )
}

/// Detect registered external changes and incomplete attempts before any new
/// process. Detection is not a sandbox and never rolls back the user's files.
pub async fn workspace_preflight(
    state: &AppState,
    project: &ProjectRow,
    job: &JobRow,
) -> Result<()> {
    if project.is_git() {
        return Ok(());
    }
    let interrupted: Option<(i64, Option<String>)> =
        sqlx::query_as("SELECT interrupted, reason FROM workspace_checks WHERE project_id = ?")
            .bind(&project.id)
            .fetch_optional(&state.db)
            .await?;
    if interrupted.is_some_and(|(flag, _)| flag != 0) {
        bail!("普通工作区可能有未完成修改，请先检查现场并通过工作区检查入口确认，不会自动回滚");
    }
    let records: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT path, fingerprint FROM artifacts WHERE project_id = ?")
            .bind(&project.id)
            .fetch_all(&state.db)
            .await?;
    let mut changes = Vec::new();
    for (path, expected) in records {
        let full = contained(&delivery_root(project), &path).await?;
        if fingerprint(&full).await? != expected {
            changes.push(path);
        }
    }
    if !changes.is_empty() {
        interrupt(
            state,
            project,
            &format!("已登记输入或产物发生外部变化：{}", changes.join("、")),
        )
        .await?;
        bail!("普通工作区外部变化需要检查：{}", changes.join("、"));
    }
    if matches!(job.role.as_str(), JOB_ROLE_EXECUTOR | JOB_ROLE_DIRECT) {
        sqlx::query("INSERT INTO workspace_checks(project_id, interrupted, reason, updated_at) VALUES(?, 1, ?, ?) \
            ON CONFLICT(project_id) DO UPDATE SET interrupted = 1, reason = excluded.reason, updated_at = excluded.updated_at")
            .bind(&project.id).bind(format!("作业 {} 写入中，若中断请检查文件现场", job.id)).bind(now()).execute(&state.db).await?;
    }
    Ok(())
}

pub async fn interrupt(state: &AppState, project: &ProjectRow, reason: &str) -> Result<()> {
    if project.is_git() {
        return Ok(());
    }
    sqlx::query("INSERT INTO workspace_checks(project_id, interrupted, reason, updated_at) VALUES(?, 1, ?, ?) \
        ON CONFLICT(project_id) DO UPDATE SET interrupted = 1, reason = excluded.reason, updated_at = excluded.updated_at")
        .bind(&project.id).bind(reason).bind(now()).execute(&state.db).await?;
    crate::events::emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "workspace_inspection_required",
        reason,
    )
    .await;
    Ok(())
}

pub async fn workspace_settle(
    state: &AppState,
    project: &ProjectRow,
    job: &JobRow,
    cwd: &Path,
    baseline: &Value,
) -> Result<()> {
    if !matches!(job.role.as_str(), JOB_ROLE_EXECUTOR | JOB_ROLE_DIRECT) {
        return Ok(());
    }
    let rows: Vec<(String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT path, fingerprint, task_id FROM artifacts WHERE project_id = ?")
            .bind(&project.id)
            .fetch_all(&state.db)
            .await?;
    if !project.is_git() {
        for (path, expected, owner) in &rows {
            let actual = fingerprint(&contained(cwd, path).await?).await?;
            if actual != *expected
                && (owner.is_none() || owner.as_deref() != job.task_id.as_deref())
            {
                let allowed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM file_permissions WHERE project_id = ? AND task_id = ? AND path = ?")
                    .bind(&project.id).bind(job.task_id.as_deref().unwrap_or("")).bind(path).fetch_one(&state.db).await?;
                if allowed == 0 {
                    interrupt(
                        state,
                        project,
                        &format!("作业未经声明覆盖历史输入或产物：{path}；已保留现场"),
                    )
                    .await?;
                    bail!("普通工作区未授权覆盖：{path}，请人工检查现场");
                }
            }
        }
    }
    let files = if project.is_git() {
        git::run_git(
            cwd,
            &[
                "diff",
                "--name-only",
                "--diff-filter=ACMRT",
                baseline
                    .as_str()
                    .ok_or_else(|| anyhow!("Git 作业缺少初始提交版本"))?,
                "HEAD",
            ],
        )
        .await?
        .lines()
        .map(str::to_string)
        .collect()
    } else {
        let root = cwd.to_path_buf();
        tokio::task::spawn_blocking(move || files_sync(&root)).await??
    };
    for path in files {
        if project.is_git() && !cwd.join(&path).is_file() {
            continue;
        }
        let exists = rows.iter().any(|(registered, _, _)| registered == &path);
        if !exists {
            // New files are indexed automatically; explicit declarations can
            // refine their class, format and verification in the artifact API.
            let checksum = fingerprint(&contained(cwd, &path).await?).await?;
            let commit = if project.is_git() {
                Some(git::commit_of(cwd, "HEAD").await?)
            } else {
                None
            };
            sqlx::query("INSERT OR IGNORE INTO artifacts(id, project_id, task_id, job_id, path, kind, format, verification, fingerprint, commit_id, contract_id, created_at, updated_at) VALUES(?, ?, ?, ?, ?, 'intermediate', '', '', ?, ?, ?, ?, ?)")
                .bind(new_id("art")).bind(&project.id).bind(&job.task_id).bind(&job.id).bind(&path).bind(checksum).bind(commit).bind(job.contract_id).bind(now()).bind(now()).execute(&state.db).await?;
        }
        if project.is_git() {
            let commit = git::commit_of(cwd, "HEAD").await?;
            sqlx::query("INSERT OR IGNORE INTO artifact_versions(artifact_id, commit_id, job_id, recorded_at) SELECT id, ?, ?, ? FROM artifacts WHERE project_id = ? AND path = ?")
                .bind(&commit).bind(&job.id).bind(now()).bind(&project.id).bind(&path).execute(&state.db).await?;
            sqlx::query("UPDATE artifacts SET commit_id = ?, job_id = ?, contract_id = ?, updated_at = ? WHERE project_id = ? AND path = ?")
                .bind(&commit).bind(&job.id).bind(job.contract_id).bind(now()).bind(&project.id).bind(&path).execute(&state.db).await?;
        }
    }
    if !project.is_git() {
        for (path, _, _) in &rows {
            let checksum = fingerprint(&contained(cwd, path).await?).await?;
            sqlx::query("UPDATE artifacts SET fingerprint = ?, updated_at = ? WHERE project_id = ? AND path = ?")
                .bind(checksum).bind(now()).bind(&project.id).bind(path).execute(&state.db).await?;
        }
        sqlx::query("UPDATE workspace_checks SET interrupted = 0, reason = NULL, updated_at = ? WHERE project_id = ?")
            .bind(now()).bind(&project.id).execute(&state.db).await?;
    }
    Ok(())
}

pub async fn acknowledge(state: &AppState, project: &ProjectRow, reason: &str) -> Result<()> {
    if reason.trim().is_empty() {
        bail!("必须记录文件现场检查结论及后续处理方式");
    }
    if project.status.as_deref() != Some(STATUS_PAUSED)
        && project.status.as_deref() != Some(STATUS_BLOCKED)
    {
        bail!("请先暂停项目或等待阻塞后检查文件现场");
    }
    let running: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE project_id = ? AND status = 'running'")
            .bind(&project.id)
            .fetch_one(&state.db)
            .await?;
    if running > 0 {
        bail!("仍有作业在途，请等待其结算后检查");
    }
    let rows: Vec<String> = sqlx::query_scalar("SELECT path FROM artifacts WHERE project_id = ?")
        .bind(&project.id)
        .fetch_all(&state.db)
        .await?;
    for path in rows {
        let checksum = fingerprint(&contained(&delivery_root(project), &path).await?).await?;
        sqlx::query("UPDATE artifacts SET fingerprint = ?, updated_at = ? WHERE project_id = ? AND path = ?")
            .bind(checksum).bind(now()).bind(&project.id).bind(path).execute(&state.db).await?;
    }
    sqlx::query("UPDATE workspace_checks SET interrupted = 0, reason = ?, acknowledged_at = ?, updated_at = ? WHERE project_id = ?")
        .bind(reason).bind(now()).bind(now()).bind(&project.id).execute(&state.db).await?;
    crate::events::emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "workspace_inspected",
        reason,
    )
    .await;
    Ok(())
}

pub async fn block_workspace(
    state: &AppState,
    project: &ProjectRow,
    job: &JobRow,
    reason: &str,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
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
