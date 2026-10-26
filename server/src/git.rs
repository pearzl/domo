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
