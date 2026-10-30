//! Git worktree isolation. All Git mutations performed by DOMO use a fixed
//! identity so a project does not depend on the user's global Git configuration.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use tokio::process::Command;

const GIT_IDENTITY: [&str; 6] = [
    "-c",
    "user.name=DOMO",
    "-c",
    "user.email=domo@localhost",
    "-c",
    "commit.gpgsign=false",
];

pub async fn run_git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await
        .with_context(|| format!("执行 git 失败: {}", args.join(" ")))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {} 失败: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub async fn repo_root(path: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .await
        .context("无法执行 git")?;
    if !output.status.success() {
        return Err(anyhow!("{} 不是 Git 仓库", path.display()));
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        return Err(anyhow!("无法识别 Git 仓库根目录"));
    }
    Ok(PathBuf::from(root))
}

pub async fn current_branch(repo: &Path) -> Result<String> {
    let branch = run_git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let branch = branch.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return Err(anyhow!("无法识别当前分支"));
    }
    Ok(branch)
}

pub async fn validate_base(repo: &Path, base: &str) -> Result<()> {
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
) -> Result<()> {
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
) -> Result<()> {
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
) -> Result<()> {
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

pub async fn remove_worktree(project_ws: &Path, dest: &Path) -> Result<()> {
    let dest_s = dest.to_string_lossy().to_string();
    run_git(project_ws, &["worktree", "remove", "--force", &dest_s]).await?;
    Ok(())
}

pub async fn prune_worktrees(repo: &Path) -> Result<()> {
    run_git(repo, &["worktree", "prune"]).await?;
    Ok(())
}

/// Moves a branch tip into the archive namespace and removes its visible
/// `refs/heads` entry only when the tip still matches `expected`.
pub async fn archive_branch(repo: &Path, branch: &str, expected: &str) -> Result<()> {
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

pub async fn status_porcelain(dir: &Path) -> Result<String> {
    run_git(dir, &["status", "--porcelain"]).await
}

pub async fn commit_all(dir: &Path, message: &str) -> Result<bool> {
    if status_porcelain(dir).await?.trim().is_empty() {
        return Ok(false);
    }
    run_git(dir, &["add", "-A"]).await?;
    let mut args = GIT_IDENTITY.to_vec();
    args.extend(["commit", "-m", message]);
    run_git(dir, &args).await?;
    Ok(true)
}

pub async fn diff_between(dir: &Path, from: &str, to: &str) -> Result<String> {
    run_git(dir, &["diff", "--stat", &format!("{from}..{to}")]).await
}

pub async fn reset_hard(dir: &Path) -> Result<()> {
    run_git(dir, &["reset", "--hard", "HEAD"]).await?;
    run_git(dir, &["clean", "-fd"]).await?;
    Ok(())
}

pub async fn squash_merge(project_ws: &Path, task_branch: &str, message: &str) -> Result<bool> {
    let merge = Command::new("git")
        .arg("-C")
        .arg(project_ws)
        .args(["merge", "--squash", task_branch])
        .output()
        .await
        .context("执行 git merge --squash 失败")?;
    if !merge.status.success() {
        let detail = format!(
            "{}{}",
            String::from_utf8_lossy(&merge.stdout),
            String::from_utf8_lossy(&merge.stderr)
        );
        let _ = reset_hard(project_ws).await;
        return Err(anyhow!("合并失败: {}", detail.trim()));
    }
    if status_porcelain(project_ws).await?.trim().is_empty() {
        return Ok(false);
    }
    commit_all(project_ws, message).await
}

pub async fn has_branch(repo: &Path, branch: &str) -> bool {
    run_git(repo, &["rev-parse", "--verify", branch])
        .await
        .is_ok()
}

pub async fn commit_of(repo: &Path, rev: &str) -> Result<String> {
    let out = run_git(
        repo,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
    .await?;
    Ok(out.trim().to_string())
}

pub async fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .await
        .context("执行 git merge-base --is-ancestor 失败")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(anyhow!(
            "git merge-base --is-ancestor 失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

pub async fn count_merge_commits(repo: &Path, from: &str, to: &str) -> Result<i64> {
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

pub async fn worktree_for_branch(repo: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let out = run_git(repo, &["worktree", "list", "--porcelain"]).await?;
    Ok(parse_worktrees(&out)
        .into_iter()
        .find(|entry| entry.branch.as_deref() == Some(branch))
        .map(|entry| entry.path))
}

/// Rebases the current branch onto `upstream`. A failed rebase is aborted so the
/// branch ref and working tree return to the state before the call.
pub async fn rebase_onto(dir: &Path, upstream: &str) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "rebase.autoStash=false", "rebase", upstream])
        .output()
        .await
        .context("执行 git rebase 失败")?;
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
    Err(anyhow!("{}", rebase_failure_message(&conflicts, &detail)))
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

pub async fn merge_ff_only(worktree: &Path, branch: &str) -> Result<()> {
    run_git(worktree, &["merge", "--ff-only", branch]).await?;
    Ok(())
}

/// Moves `branch` to `commit` only when it still points at `expected`.
pub async fn update_branch_ref(
    repo: &Path,
    branch: &str,
    commit: &str,
    expected: &str,
) -> Result<()> {
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

pub async fn reset_hard_to(dir: &Path, commit: &str) -> Result<()> {
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
}
