//! Git worktree isolation. All Git mutations performed by DOMO use a fixed
//! identity so a project does not depend on the user's global Git configuration.

use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::platform;

/// Failures raised by Git commands and worktree bookkeeping. Messages keep the
/// wording callers already rely on, including the underlying I/O detail.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("执行 git 失败: {args}: {cause}")]
    CommandIo { args: String, cause: std::io::Error },
    #[error("git {args} 失败: {stderr}")]
    CommandFailed { args: String, stderr: String },
    #[error("无法执行 git: {cause}")]
    RepositoryProbeIo { cause: std::io::Error },
    #[error("{path} 不是 Git 仓库")]
    NotRepository { path: String },
    #[error("无法识别 Git 仓库根目录")]
    RepositoryRootUnknown,
    #[error("无法识别当前分支")]
    BranchUnknown,
    #[error("执行 git merge --squash 失败: {cause}")]
    SquashProbeIo { cause: std::io::Error },
    #[error("合并失败: {detail}")]
    SquashFailed { detail: String },
    #[error("合并目标工作区有未提交改动，保留现场: {path}")]
    DirtyMergeTarget { path: String },
    #[error("执行 git merge {branch} 失败: {cause}")]
    MergeProbeIo {
        branch: String,
        cause: std::io::Error,
    },
    #[error("合并 {branch} 失败且没有文件冲突: {detail}")]
    MergeWithoutConflicts { branch: String, detail: String },
    #[error("执行 git merge-base --is-ancestor 失败: {cause}")]
    AncestorProbeIo { cause: std::io::Error },
    #[error("git merge-base --is-ancestor 失败: {stderr}")]
    AncestorFailed { stderr: String },
    #[error("执行 git rebase 失败: {cause}")]
    RebaseProbeIo { cause: std::io::Error },
    #[error("{message}")]
    RebaseFailed { message: String },
    #[error(transparent)]
    Platform(#[from] platform::PlatformError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type GitResult<T> = std::result::Result<T, GitError>;

const GIT_IDENTITY: [&str; 6] = [
    "-c",
    "user.name=DOMO",
    "-c",
    "user.email=domo@localhost",
    "-c",
    "commit.gpgsign=false",
];

pub async fn run_git(dir: &Path, args: &[&str]) -> GitResult<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        // Checkouts routinely hold trees deeper than the legacy Windows path
        // limit, so every command accepts them.
        .args(["-c", "core.longpaths=true"])
        .args(args)
        .output()
        .await
        .map_err(|cause| GitError::CommandIo {
            args: args.join(" "),
            cause,
        })?;
    if !output.status.success() {
        return Err(GitError::CommandFailed {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub async fn repo_root(path: &Path) -> GitResult<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .await
        .map_err(|cause| GitError::RepositoryProbeIo { cause })?;
    if !output.status.success() {
        return Err(GitError::NotRepository {
            path: path.display().to_string(),
        });
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        return Err(GitError::RepositoryRootUnknown);
    }
    Ok(PathBuf::from(root))
}

pub async fn current_branch(repo: &Path) -> GitResult<String> {
    let branch = run_git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let branch = branch.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return Err(GitError::BranchUnknown);
    }
    Ok(branch)
}

pub async fn validate_base(repo: &Path, base: &str) -> GitResult<()> {
    run_git(
        repo,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
    )
    .await?;
    Ok(())
}

pub async fn create_project_worktree(
    repo: &Path,
    base: &str,
    dest: &Path,
    branch: &str,
) -> GitResult<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let dest_s = dest.to_string_lossy().to_string();
    run_git(repo, &["worktree", "add", "-b", branch, &dest_s, base]).await?;
    Ok(())
}

pub async fn create_task_worktree(
    project_ws: &Path,
    project_branch: &str,
    dest: &Path,
    branch: &str,
) -> GitResult<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let dest_s = dest.to_string_lossy().to_string();
    let exists = run_git(
        project_ws,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    )
    .await
    .is_ok();
    if exists {
        run_git(project_ws, &["worktree", "add", &dest_s, branch]).await?;
    } else {
        run_git(
            project_ws,
            &["worktree", "add", "-b", branch, &dest_s, project_branch],
        )
        .await?;
    }
    Ok(())
}

pub async fn create_verify_worktree(
    project_ws: &Path,
    task_branch: &str,
    dest: &Path,
) -> GitResult<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let dest_s = dest.to_string_lossy().to_string();
    run_git(
        project_ws,
        &["worktree", "add", "--detach", &dest_s, task_branch],
    )
    .await?;
    Ok(())
}

/// Removes a worktree checkout together with its metadata. Git refuses to delete
/// checkout contents that exceed the legacy Windows path limit, so a failed
/// removal deletes the directory directly and prunes the stale metadata.
pub async fn remove_worktree(project_ws: &Path, dest: &Path) -> GitResult<()> {
    let dest_s = dest.to_string_lossy().to_string();
    let result = run_git(project_ws, &["worktree", "remove", "--force", &dest_s]).await;
    let Err(error) = result else {
        return Ok(());
    };
    tracing::debug!(
        path = %dest.display(),
        error = %format!("{error:#}"),
        "git 无法删除工作树，改为直接删除目录"
    );
    match platform::remove_dir_all(dest).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    prune_worktrees(project_ws).await
}

pub async fn prune_worktrees(repo: &Path) -> GitResult<()> {
    run_git(repo, &["worktree", "prune"]).await?;
    Ok(())
}

/// Moves a branch tip into the archive namespace and removes its visible
/// `refs/heads` entry only when the tip still matches `expected`.
pub async fn archive_branch(repo: &Path, branch: &str, expected: &str) -> GitResult<()> {
    let archive = archive_ref(branch);
    run_git(
        repo,
        &[
            "update-ref",
            "-m",
            "domo: archive merged work",
            &archive,
            expected,
        ],
    )
    .await?;
    run_git(
        repo,
        &[
            "update-ref",
            "-d",
            &format!("refs/heads/{branch}"),
            expected,
        ],
    )
    .await?;
    Ok(())
}

pub fn archive_ref(branch: &str) -> String {
    format!("refs/domo-archive/heads/{branch}")
}

/// True when `dir` is the root of a Git worktree. Plain directories and
/// subdirectories of another checkout are rejected so leftover directories are
/// never mistaken for a task workspace.
pub async fn is_worktree(dir: &Path) -> bool {
    let Ok(root) = repo_root(dir).await else {
        return false;
    };
    match (
        tokio::fs::canonicalize(dir).await,
        tokio::fs::canonicalize(&root).await,
    ) {
        (Ok(dir), Ok(root)) => dir == root,
        _ => false,
    }
}

pub async fn status_porcelain(dir: &Path) -> GitResult<String> {
    run_git(dir, &["status", "--porcelain"]).await
}

pub async fn commit_all(dir: &Path, message: &str) -> GitResult<bool> {
    if status_porcelain(dir).await?.trim().is_empty() {
        return Ok(false);
    }
    run_git(dir, &["add", "-A"]).await?;
    let mut args = GIT_IDENTITY.to_vec();
    args.extend(["commit", "-m", message]);
    run_git(dir, &args).await?;
    Ok(true)
}

/// Validate exact repo-relative file paths; no globs, directories or symlink
/// traversal. Deleted paths and both sides of a rename are valid declarations.
pub async fn validate_commit_files(dir: &Path, files: &[String]) -> GitResult<()> {
    let root = tokio::fs::canonicalize(dir).await?;
    for file in files {
        if !crate::domain::valid_commit_path(file) {
            return Err(GitError::SquashFailed {
                detail: format!("commit_files 包含不安全路径: {file}"),
            });
        }
        let path = root.join(file);
        let mut parent = path.parent();
        while let Some(p) = parent {
            if p == root {
                break;
            }
            if tokio::fs::symlink_metadata(p)
                .await
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                return Err(GitError::SquashFailed {
                    detail: format!("commit_files 路径穿过符号链接: {file}"),
                });
            }
            parent = p.parent();
        }
        if tokio::fs::symlink_metadata(&path)
            .await
            .is_ok_and(|m| !m.is_file())
        {
            return Err(GitError::SquashFailed {
                detail: format!("commit_files 只接受普通文件或删除路径: {file}"),
            });
        }
        let tracked = run_git(
            dir,
            &[
                "--literal-pathspecs",
                "ls-files",
                "--error-unmatch",
                "--",
                file,
            ],
        )
        .await
        .is_ok();
        if !tracked && !path.is_file() {
            return Err(GitError::SquashFailed {
                detail: format!("commit_files 文件不存在且未被跟踪: {file}"),
            });
        }
        if !tracked
            && run_git(dir, &["check-ignore", "--quiet", "--", file])
                .await
                .is_ok()
        {
            return Err(GitError::SquashFailed {
                detail: format!("commit_files 包含被忽略的文件: {file}"),
            });
        }
    }
    // A resolved merge cannot be committed with an omitted conflicted path.
    let conflicts = run_git(dir, &["diff", "--name-only", "-z", "--diff-filter=U"]).await?;
    if conflicts
        .split('\0')
        .filter(|p| !p.is_empty())
        .any(|p| !files.iter().any(|file| file == p))
    {
        return Err(GitError::SquashFailed {
            detail: "commit_files 必须包含全部冲突路径".into(),
        });
    }
    let staged = run_git(
        dir,
        &["diff", "--cached", "--name-only", "--no-renames", "-z"],
    )
    .await?;
    if staged
        .split('\0')
        .filter(|p| !p.is_empty())
        .any(|p| !files.iter().any(|file| file == p))
    {
        return Err(GitError::SquashFailed {
            detail: "暂存区包含 commit_files 之外的路径，请保留现场并核对清单".into(),
        });
    }
    Ok(())
}

pub async fn commit_files(dir: &Path, message: &str, files: &[String]) -> GitResult<bool> {
    validate_commit_files(dir, files).await?;
    if !files.is_empty() {
        let mut args = vec!["--literal-pathspecs", "add", "-A", "--"];
        args.extend(files.iter().map(String::as_str));
        run_git(dir, &args).await?;
    }
    // Resolving a conflict by keeping HEAD can leave an unchanged tree, but
    // its merge still needs a commit to conclude MERGE_HEAD.
    let merging = run_git(dir, &["rev-parse", "--verify", "MERGE_HEAD"])
        .await
        .is_ok();
    if run_git(dir, &["diff", "--cached", "--name-only"])
        .await?
        .trim()
        .is_empty()
        && !merging
    {
        return Ok(false);
    }
    let mut args = GIT_IDENTITY.to_vec();
    args.extend(["commit", "-m", message]);
    run_git(dir, &args).await?;
    Ok(true)
}

pub async fn diff_between(dir: &Path, from: &str, to: &str) -> GitResult<String> {
    run_git(dir, &["diff", "--stat", &format!("{from}..{to}")]).await
}

pub async fn reset_hard(dir: &Path) -> GitResult<()> {
    run_git(dir, &["reset", "--hard", "HEAD"]).await?;
    run_git(dir, &["clean", "-fd"]).await?;
    Ok(())
}

pub async fn squash_merge(project_ws: &Path, task_branch: &str, message: &str) -> GitResult<bool> {
    if !status_porcelain(project_ws).await?.trim().is_empty() {
        return Err(GitError::DirtyMergeTarget {
            path: project_ws.display().to_string(),
        });
    }
    let merge = Command::new("git")
        .arg("-C")
        .arg(project_ws)
        .args(["merge", "--squash", task_branch])
        .output()
        .await
        .map_err(|cause| GitError::SquashProbeIo { cause })?;
    if !merge.status.success() {
        let detail = format!(
            "{}{}",
            String::from_utf8_lossy(&merge.stdout),
            String::from_utf8_lossy(&merge.stderr)
        );
        let _ = reset_hard(project_ws).await;
        return Err(GitError::SquashFailed {
            detail: detail.trim().to_string(),
        });
    }
    if status_porcelain(project_ws).await?.trim().is_empty() {
        return Ok(false);
    }
    commit_all(project_ws, message).await
}

/// Starts merging `branch` into the worktree's current branch without creating
/// a commit. When the merge conflicts, the conflict markers and unmerged index
/// entries stay in the worktree so an executor can resolve them. A clean merge
/// or an unrelated failure is aborted so the worktree stays unchanged.
pub async fn merge_conflict_files(worktree: &Path, branch: &str) -> GitResult<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["merge", "--no-commit", "--no-ff", branch])
        .output()
        .await
        .map_err(|cause| GitError::MergeProbeIo {
            branch: branch.to_string(),
            cause,
        })?;
    if output.status.success() {
        abort_merge(worktree).await;
        return Ok(Vec::new());
    }
    let conflicts = run_git(worktree, &["diff", "--name-only", "--diff-filter=U"])
        .await
        .unwrap_or_default();
    let files: Vec<String> = conflicts
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    if files.is_empty() {
        let detail = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        abort_merge(worktree).await;
        return Err(GitError::MergeWithoutConflicts {
            branch: branch.to_string(),
            detail: detail.trim().to_string(),
        });
    }
    Ok(files)
}

async fn abort_merge(dir: &Path) {
    let _ = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["merge", "--abort"])
        .output()
        .await;
}

pub async fn has_branch(repo: &Path, branch: &str) -> bool {
    run_git(repo, &["rev-parse", "--verify", branch])
        .await
        .is_ok()
}

pub async fn commit_of(repo: &Path, rev: &str) -> GitResult<String> {
    let out = run_git(
        repo,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
    .await?;
    Ok(out.trim().to_string())
}

pub async fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> GitResult<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .await
        .map_err(|cause| GitError::AncestorProbeIo { cause })?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(GitError::AncestorFailed {
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        }),
    }
}

pub async fn count_merge_commits(repo: &Path, from: &str, to: &str) -> GitResult<i64> {
    let out = run_git(
        repo,
        &["rev-list", "--merges", "--count", &format!("{from}..{to}")],
    )
    .await?;
    Ok(out.trim().parse::<i64>().unwrap_or(0))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<String>,
}

pub fn parse_worktrees(text: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    for line in text.lines() {
        if line.is_empty() {
            if let Some(path) = path.take() {
                entries.push(WorktreeEntry {
                    path,
                    branch: branch.take(),
                });
            }
            continue;
        }
        if let Some(raw) = line.strip_prefix("worktree ") {
            if let Some(previous) = path.take() {
                entries.push(WorktreeEntry {
                    path: previous,
                    branch: branch.take(),
                });
            }
            path = Some(PathBuf::from(raw));
        } else if let Some(raw) = line.strip_prefix("branch refs/heads/") {
            branch = Some(raw.to_string());
        }
    }
    if let Some(path) = path.take() {
        entries.push(WorktreeEntry { path, branch });
    }
    entries
}

pub async fn worktree_for_branch(repo: &Path, branch: &str) -> GitResult<Option<PathBuf>> {
    let out = run_git(repo, &["worktree", "list", "--porcelain"]).await?;
    Ok(parse_worktrees(&out)
        .into_iter()
        .find(|entry| entry.branch.as_deref() == Some(branch))
        .map(|entry| entry.path))
}

/// Rebases the current branch onto `upstream`. A failed rebase is aborted so the
/// branch ref and working tree return to the state before the call.
pub async fn rebase_onto(dir: &Path, upstream: &str) -> GitResult<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "rebase.autoStash=false", "rebase", upstream])
        .output()
        .await
        .map_err(|cause| GitError::RebaseProbeIo { cause })?;
    if output.status.success() {
        return Ok(());
    }
    let conflicts = run_git(dir, &["diff", "--name-only", "--diff-filter=U"])
        .await
        .unwrap_or_default();
    let detail = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rebase", "--abort"])
        .output()
        .await;
    Err(GitError::RebaseFailed {
        message: rebase_failure_message(&conflicts, &detail),
    })
}

fn rebase_failure_message(conflicts: &str, detail: &str) -> String {
    let files: Vec<&str> = conflicts
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let detail = truncate_chars(detail.trim(), 800);
    let head = if files.is_empty() {
        "rebase 失败，已中止".to_string()
    } else {
        format!("rebase 冲突，已中止；冲突文件：{}", files.join(", "))
    };
    if detail.is_empty() {
        head
    } else {
        format!("{head}。{detail}")
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    let mut truncated: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        truncated.push('…');
    }
    truncated
}

pub async fn merge_ff_only(worktree: &Path, branch: &str) -> GitResult<()> {
    run_git(worktree, &["merge", "--ff-only", branch]).await?;
    Ok(())
}

/// Moves `branch` to `commit` only when it still points at `expected`.
pub async fn update_branch_ref(
    repo: &Path,
    branch: &str,
    commit: &str,
    expected: &str,
) -> GitResult<()> {
    run_git(
        repo,
        &[
            "update-ref",
            &format!("refs/heads/{branch}"),
            commit,
            expected,
        ],
    )
    .await?;
    Ok(())
}

pub async fn reset_hard_to(dir: &Path, commit: &str) -> GitResult<()> {
    run_git(dir, &["reset", "--hard", commit]).await?;
    Ok(())
}

pub async fn abort_rebase(dir: &Path) {
    let _ = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rebase", "--abort"])
        .output()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_worktrees_reads_branches_and_detached_entries() {
        let text = "\
worktree /repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /tmp/task one
HEAD 2222222222222222222222222222222222222222
detached
";
        let entries = parse_worktrees(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, PathBuf::from("/repo"));
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(entries[1].path, PathBuf::from("/tmp/task one"));
        assert_eq!(entries[1].branch, None);
    }

    #[tokio::test]
    async fn declared_files_commit_only_exact_add_edit_delete_and_rename_paths() {
        let repo = std::env::temp_dir().join(format!("domo-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]).await.unwrap();
        for name in ["edit.txt", "delete.txt", "old.txt", "omitted.txt"] {
            std::fs::write(repo.join(name), "base").unwrap();
        }
        std::fs::write(repo.join(".gitignore"), "build/\n").unwrap();
        commit_all(&repo, "initial").await.unwrap();
        std::fs::write(repo.join("edit.txt"), "edited").unwrap();
        std::fs::remove_file(repo.join("delete.txt")).unwrap();
        std::fs::rename(repo.join("old.txt"), repo.join("new.txt")).unwrap();
        let special_name = if cfg!(windows) {
            "space and unicode 文.txt"
        } else {
            "space and\nnewline.txt"
        };
        std::fs::write(repo.join(special_name), "added").unwrap();
        std::fs::write(repo.join("omitted.txt"), "keep outside commit").unwrap();
        std::fs::create_dir(repo.join("build")).unwrap();
        std::fs::write(repo.join("build/log"), "ignored").unwrap();
        let files =
            ["edit.txt", "delete.txt", "old.txt", "new.txt", special_name].map(str::to_string);
        assert!(commit_files(&repo, "delivery", &files).await.unwrap());
        assert_eq!(
            run_git(&repo, &["show", "HEAD:edit.txt"]).await.unwrap(),
            "edited"
        );
        assert!(run_git(&repo, &["show", "HEAD:delete.txt"]).await.is_err());
        assert!(run_git(&repo, &["show", "HEAD:old.txt"]).await.is_err());
        assert_eq!(
            run_git(&repo, &["show", "HEAD:new.txt"]).await.unwrap(),
            "base"
        );
        assert_eq!(
            run_git(&repo, &["show", "HEAD:omitted.txt"]).await.unwrap(),
            "base"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("omitted.txt")).unwrap(),
            "keep outside commit"
        );
        assert!(run_git(&repo, &["show", "HEAD:build/log"]).await.is_err());
        assert!(!commit_files(&repo, "empty", &[]).await.unwrap());
        let _ = std::fs::remove_dir_all(repo);
    }

    #[tokio::test]
    async fn declared_files_reject_unsafe_ignored_and_undeclared_index_paths() {
        let repo = std::env::temp_dir().join(format!("domo-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(repo.join("dir")).unwrap();
        run_git(&repo, &["init", "-b", "main"]).await.unwrap();
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(repo.join("outside.txt"), "base").unwrap();
        commit_all(&repo, "initial").await.unwrap();
        std::fs::write(repo.join("ignored.txt"), "ignored").unwrap();
        for path in [
            "",
            ".",
            "..",
            "../outside",
            "/absolute",
            "C:/file",
            "a\\b",
            ".git/config",
            "a//b",
            "*.txt",
            "dir",
            "missing.txt",
            "ignored.txt",
        ] {
            assert!(
                validate_commit_files(&repo, &[path.into()]).await.is_err(),
                "{path}"
            );
        }
        std::fs::write(repo.join("outside.txt"), "staged").unwrap();
        run_git(&repo, &["add", "outside.txt"]).await.unwrap();
        assert!(commit_files(&repo, "must not commit", &[]).await.is_err());
        assert_eq!(
            run_git(&repo, &["show", "HEAD:outside.txt"]).await.unwrap(),
            "base"
        );
        assert_eq!(
            run_git(&repo, &["show", ":outside.txt"]).await.unwrap(),
            "staged"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(repo.join("outside.txt"), repo.join("link")).unwrap();
            std::os::unix::fs::symlink(repo.join("dir"), repo.join("linkdir")).unwrap();
            for path in ["link", "linkdir/file"] {
                assert!(
                    validate_commit_files(&repo, &[path.into(), "outside.txt".into()])
                        .await
                        .is_err()
                );
            }
        }
        let _ = std::fs::remove_dir_all(repo);
    }

    #[tokio::test]
    async fn is_worktree_rejects_plain_and_nested_directories() {
        let root = std::env::temp_dir().join(format!("domo-git-{}", uuid::Uuid::new_v4()));
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("nested")).unwrap();

        assert!(!is_worktree(&repo).await);

        run_git(&repo, &["init", "-b", "main"]).await.unwrap();
        assert!(is_worktree(&repo).await);
        assert!(!is_worktree(&repo.join("nested")).await);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rebase_failure_message_lists_conflict_files() {
        let message = rebase_failure_message("src/a.rs\nsrc/b.rs\n", "CONFLICT (content)");
        assert!(message.contains("src/a.rs, src/b.rs"), "{message}");
        assert!(message.contains("CONFLICT"), "{message}");
    }

    #[test]
    fn rebase_failure_message_truncates_long_detail() {
        let detail = "x".repeat(2000);
        let message = rebase_failure_message("", &detail);
        assert!(message.chars().count() < 900, "{}", message.chars().count());
        assert!(message.ends_with('…'));
    }

    #[tokio::test]
    async fn archive_branch_moves_the_ref_and_keeps_the_commit() {
        let repo = std::env::temp_dir().join(format!("domo-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]).await.unwrap();
        std::fs::write(repo.join("file.txt"), "value").unwrap();
        run_git(&repo, &["add", "."]).await.unwrap();
        run_git(
            &repo,
            &[
                "-c",
                "user.name=DOMO",
                "-c",
                "user.email=domo@localhost",
                "commit",
                "-m",
                "initial",
            ],
        )
        .await
        .unwrap();
        run_git(&repo, &["branch", "domo-task/demo"]).await.unwrap();
        let expected = commit_of(&repo, "domo-task/demo").await.unwrap();

        archive_branch(&repo, "domo-task/demo", &expected)
            .await
            .unwrap();

        assert!(!has_branch(&repo, "domo-task/demo").await);
        assert_eq!(
            commit_of(&repo, "refs/domo-archive/heads/domo-task/demo")
                .await
                .unwrap(),
            expected
        );
        assert_eq!(
            archive_ref("domo-task/demo"),
            "refs/domo-archive/heads/domo-task/demo"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn squash_merge_preserves_dirty_target_and_source_refs() {
        let repo = std::env::temp_dir().join(format!("domo-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]).await.unwrap();
        std::fs::write(repo.join("shared.txt"), "base\n").unwrap();
        commit_all(&repo, "initial").await.unwrap();
        run_git(&repo, &["checkout", "-b", "child"]).await.unwrap();
        std::fs::write(repo.join("shared.txt"), "child\n").unwrap();
        commit_all(&repo, "child change").await.unwrap();
        let child = commit_of(&repo, "child").await.unwrap();
        run_git(&repo, &["checkout", "main"]).await.unwrap();
        let parent = commit_of(&repo, "main").await.unwrap();
        std::fs::write(repo.join("shared.txt"), "parent unsaved\n").unwrap();
        std::fs::write(repo.join("untracked.txt"), "keep").unwrap();
        assert!(matches!(
            squash_merge(&repo, "child", "merge").await,
            Err(GitError::DirtyMergeTarget { .. })
        ));
        assert_eq!(commit_of(&repo, "main").await.unwrap(), parent);
        assert_eq!(commit_of(&repo, "child").await.unwrap(), child);
        assert_eq!(
            std::fs::read_to_string(repo.join("shared.txt")).unwrap(),
            "parent unsaved\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("untracked.txt")).unwrap(),
            "keep"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn merge_conflict_files_leave_conflicts_in_the_worktree() {
        let repo = std::env::temp_dir().join(format!("domo-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]).await.unwrap();
        std::fs::write(repo.join("shared.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "."]).await.unwrap();
        run_git(
            &repo,
            &[
                "-c",
                "user.name=DOMO",
                "-c",
                "user.email=domo@localhost",
                "commit",
                "-m",
                "initial",
            ],
        )
        .await
        .unwrap();
        run_git(&repo, &["branch", "domo-task/demo"]).await.unwrap();

        std::fs::write(repo.join("shared.txt"), "main\n").unwrap();
        run_git(&repo, &["add", "."]).await.unwrap();
        run_git(
            &repo,
            &[
                "-c",
                "user.name=DOMO",
                "-c",
                "user.email=domo@localhost",
                "commit",
                "-m",
                "main change",
            ],
        )
        .await
        .unwrap();

        run_git(&repo, &["checkout", "domo-task/demo"])
            .await
            .unwrap();
        std::fs::write(repo.join("shared.txt"), "task\n").unwrap();
        run_git(&repo, &["add", "."]).await.unwrap();
        run_git(
            &repo,
            &[
                "-c",
                "user.name=DOMO",
                "-c",
                "user.email=domo@localhost",
                "commit",
                "-m",
                "task change",
            ],
        )
        .await
        .unwrap();

        let conflicts = merge_conflict_files(&repo, "main").await.unwrap();
        assert_eq!(conflicts, vec!["shared.txt".to_string()]);
        let unmerged = run_git(&repo, &["diff", "--name-only", "--diff-filter=U"])
            .await
            .unwrap();
        assert_eq!(unmerged.trim(), "shared.txt");
        let content = std::fs::read_to_string(repo.join("shared.txt")).unwrap();
        assert!(content.contains("<<<<<<<"), "{content}");
        assert!(validate_commit_files(&repo, &[]).await.is_err());
        let head_content = run_git(&repo, &["show", "HEAD:shared.txt"]).await.unwrap();
        std::fs::write(repo.join("shared.txt"), head_content).unwrap();
        assert!(commit_files(&repo, "resolved", &["shared.txt".into()])
            .await
            .unwrap());
        assert!(run_git(&repo, &["diff", "--name-only", "--diff-filter=U"])
            .await
            .unwrap()
            .is_empty());

        let _ = std::fs::remove_dir_all(&repo);
    }
}
