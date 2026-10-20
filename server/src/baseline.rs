//! 项目基线（DESIGN §4、ARCH §15 第 1 步）。
//!
//! - 单仓：确认项目时记录工程仓库当前 `{"branch"}`；detached HEAD 记录 `{"commit"}`。
//! - 多仓：确认项目时逐仓确保 `domo/iter-<项目short>` 存在并 checkout；
//!   全部成功才返回 `{"repos": {rel: branch}}`。
//! - **先检查、后执行**：所有仓库通过预检后才动第一个仓库；执行中意外失败会回滚
//!   本次已切换的仓库（并删除本次新建的分支），不留部分切换。
//! - 任一失败都返回仓库级原因，供 API 以 409 列出失败仓库。
//! - 本模块只做 git 与文件系统操作，**不碰数据库、不持锁**；落库与事件由调用方负责。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::models::RepoRef;

/// 单个仓库的失败原因（API 原样返回，前端可直接展示）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RepoFailure {
    pub repo: String,
    pub error: String,
}

/// 基线准备失败：`error` 是总述，`repos` 列出具体仓库。
#[derive(Debug, Clone, Serialize)]
pub struct BaselineFailure {
    pub error: String,
    pub repos: Vec<RepoFailure>,
}

impl BaselineFailure {
    fn new(error: impl Into<String>, repos: Vec<RepoFailure>) -> Self {
        BaselineFailure {
            error: error.into(),
            repos,
        }
    }
}

/// 项目迭代分支名：`domo/iter-<项目 id 后缀>`。
pub fn iteration_branch(project_id: &str) -> String {
    let short = project_id.rsplit('_').next().unwrap_or(project_id);
    format!("domo/iter-{short}")
}

/// 仓库目录：rel="." 表示工作区本身是仓库。
pub fn repo_dir(root: &Path, rel: &str) -> PathBuf {
    if rel == "." {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {args:?} failed to spawn: {e}"))
}

fn git_ok(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(dir, args)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn branch_exists(dir: &Path, branch: &str) -> bool {
    git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .map(|o| o.status.success())
    .unwrap_or(false)
}

fn current_branch(dir: &Path) -> Option<String> {
    git_ok(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .filter(|s| !s.is_empty())
}

fn probe_writable(dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!("directory not found: {}", dir.display()));
    }
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let path = dir.join(format!(
        ".domo-baseline-probe-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(f) => {
            drop(f);
            let _ = std::fs::remove_file(&path);
            Ok(())
        }
        Err(e) => Err(format!(
            "repository is not writable ({}): {e}",
            dir.display()
        )),
    }
}

/// 目标分支是否已被**另一个** worktree 占用（git 会拒绝 checkout）。
fn branch_checked_out_elsewhere(dir: &Path, branch: &str) -> Result<bool, String> {
    let out = git_ok(dir, &["worktree", "list", "--porcelain"])?;
    let want = format!("branch refs/heads/{branch}");
    let here = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let mut worktree: Option<PathBuf> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            worktree = Some(PathBuf::from(p));
        } else if line == want {
            let path = worktree.clone().unwrap_or_default();
            let path = path.canonicalize().unwrap_or(path);
            if path != here {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[derive(Debug, Clone)]
enum Original {
    Branch(String),
    Commit(String),
}

#[derive(Debug, Clone)]
struct Plan {
    rel: String,
    dir: PathBuf,
    branch: String,
    original: Original,
    branch_exists: bool,
}

fn precheck(root: &Path, repo: &RepoRef, branch: &str) -> Result<Plan, String> {
    let dir = repo_dir(root, &repo.rel);
    if !dir.is_dir() {
        return Err(format!("repository directory not found: {}", dir.display()));
    }
    let inside = git_ok(&dir, &["rev-parse", "--is-inside-work-tree"])?;
    if inside != "true" {
        return Err("not a git work tree".into());
    }
    let head = git_ok(&dir, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if head.is_empty() {
        return Err("repository has no commit (HEAD is unborn)".into());
    }
    let git_dir = git_ok(&dir, &["rev-parse", "--git-dir"])?;
    let git_dir = PathBuf::from(git_dir);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        dir.join(git_dir)
    };
    probe_writable(&git_dir)?;
    probe_writable(&dir)?;

    let original = match current_branch(&dir) {
        Some(b) => Original::Branch(b),
        None => Original::Commit(head),
    };
    let exists = branch_exists(&dir, branch);
    let already_here = matches!(&original, Original::Branch(b) if b == branch);
    if exists && !already_here && branch_checked_out_elsewhere(&dir, branch)? {
        return Err(format!(
            "branch {branch} is already checked out in another worktree"
        ));
    }
    Ok(Plan {
        rel: repo.rel.clone(),
        dir,
        branch: branch.to_string(),
        original,
        branch_exists: exists,
    })
}

fn checkout(plan: &Plan) -> Result<(), String> {
    let out = if plan.branch_exists {
        git(&plan.dir, &["checkout", &plan.branch])?
    } else {
        git(&plan.dir, &["checkout", "-b", &plan.branch, "HEAD"])?
    };
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git checkout {} failed: {}",
            plan.branch,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn rollback(plans: &[Plan], switched: &[usize], failures: &mut Vec<RepoFailure>) {
    for idx in switched.iter().rev() {
        let plan = &plans[*idx];
        let restored = match &plan.original {
            Original::Branch(b) => git(&plan.dir, &["checkout", b]),
            Original::Commit(c) => git(&plan.dir, &["checkout", "--detach", c]),
        };
        if !restored.map(|o| o.status.success()).unwrap_or(false) {
            failures.push(RepoFailure {
                repo: plan.rel.clone(),
                error: "rollback failed: repository may remain on the new branch".into(),
            });
            continue;
        }
        // 分支是本次新建的：切回原位置后再删除，避免留下空迭代分支。
        if !plan.branch_exists {
            let _ = git(&plan.dir, &["branch", "-D", &plan.branch]);
        }
    }
}

/// 多仓项目基线：先检查全部仓库，再逐仓创建/切换迭代分支。
pub fn apply_multi(
    root: &Path,
    repos: &[RepoRef],
    project_id: &str,
) -> Result<Value, BaselineFailure> {
    let branch = iteration_branch(project_id);
    let mut plans: Vec<Plan> = Vec::new();
    let mut failures: Vec<RepoFailure> = Vec::new();
    for repo in repos {
        match precheck(root, repo, &branch) {
            Ok(p) => plans.push(p),
            Err(e) => failures.push(RepoFailure {
                repo: repo.rel.clone(),
                error: e,
            }),
        }
    }
    if !failures.is_empty() {
        return Err(BaselineFailure::new(
            format!(
                "项目基线检查失败：{} 个仓库不可切换（未切换任何仓库）",
                failures.len()
            ),
            failures,
        ));
    }

    let mut switched: Vec<usize> = Vec::new();
    for (idx, plan) in plans.iter().enumerate() {
        if matches!(&plan.original, Original::Branch(b) if b == &plan.branch) {
            continue; // 已在该项目分支上，幂等
        }
        if let Err(e) = checkout(plan) {
            let mut failures = vec![RepoFailure {
                repo: plan.rel.clone(),
                error: e,
            }];
            rollback(&plans, &switched, &mut failures);
            return Err(BaselineFailure::new(
                "项目分支切换失败，已回滚已切换的仓库",
                failures,
            ));
        }
        switched.push(idx);
    }

    let mut map = Map::new();
    for plan in &plans {
        map.insert(plan.rel.clone(), Value::String(branch.clone()));
    }
    Ok(json!({ "repos": map }))
}

/// 单仓项目基线：记录当前分支；detached HEAD 记录当前提交。
///
/// `rel` 为仓库相对工作区的路径（"." = 工作区本身是仓库）。
pub fn single_baseline(root: &Path, rel: &str) -> Result<Value, String> {
    let dir = repo_dir(root, rel);
    let inside = git_ok(&dir, &["rev-parse", "--is-inside-work-tree"])?;
    if inside != "true" {
        return Err(format!("not a git work tree: {}", dir.display()));
    }
    let commit = git_ok(&dir, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if commit.is_empty() {
        return Err("repository has no commit (HEAD is unborn)".into());
    }
    Ok(match current_branch(&dir) {
        Some(branch) => json!({ "branch": branch }),
        None => json!({ "commit": commit }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "domo-baseline-{tag}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            make_writable(&self.0);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn make_writable(p: &Path) {
        if let Ok(md) = std::fs::symlink_metadata(p) {
            let mut perm = md.permissions();
            if perm.readonly() {
                set_mode(&mut perm, 0o700, true);
                let _ = std::fs::set_permissions(p, perm);
            }
        }
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    make_writable(&e.path());
                }
            }
        }
    }

    fn git_test(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git_test(dir, &["init", "-q"]);
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        git_test(dir, &["add", "-A"]);
        git_test(
            dir,
            &[
                "-c",
                "user.name=domo",
                "-c",
                "user.email=domo@example.invalid",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
    }

    fn repos(names: &[&str]) -> Vec<RepoRef> {
        names
            .iter()
            .enumerate()
            .map(|(i, rel)| RepoRef {
                rel: (*rel).into(),
                main: i == 0,
            })
            .collect()
    }

    #[test]
    fn iteration_branch_uses_project_short_id() {
        assert_eq!(iteration_branch("prj_abc123"), "domo/iter-abc123");
        assert_eq!(iteration_branch("legacy"), "domo/iter-legacy");
    }

    #[test]
    fn multi_baseline_creates_and_checks_out_each_repo() {
        let tmp = Tmp::new("apply");
        let root = tmp.path();
        init_repo(&root.join("api"));
        init_repo(&root.join("web"));

        let project = "prj_test0001";
        let baseline = apply_multi(root, &repos(&["api", "web"]), project).unwrap();

        let branch = iteration_branch(project);
        assert_eq!(baseline["repos"]["api"], branch);
        assert_eq!(baseline["repos"]["web"], branch);
        for rel in ["api", "web"] {
            assert_eq!(
                current_branch(&root.join(rel)).as_deref(),
                Some(branch.as_str())
            );
            assert!(branch_exists(&root.join(rel), &branch));
        }

        // 幂等：重复确认不会报错，也不会重建分支。
        apply_multi(root, &repos(&["api", "web"]), project).unwrap();
    }

    #[test]
    fn multi_baseline_is_all_or_nothing_when_a_repo_is_read_only() {
        let tmp = Tmp::new("readonly");
        let root = tmp.path();
        init_repo(&root.join("api"));
        init_repo(&root.join("web"));
        let api_before = current_branch(&root.join("api"));
        let web_before = current_branch(&root.join("web"));

        make_read_only(&root.join("web"));
        let err = apply_multi(root, &repos(&["api", "web"]), "prj_test0002").unwrap_err();

        assert!(
            err.repos.iter().any(|r| r.repo == "web"),
            "failed repos: {:?}",
            err.repos
        );
        // 预检阶段就失败：两个仓库都还在原分支，也没有创建迭代分支。
        assert_eq!(current_branch(&root.join("api")), api_before);
        assert_eq!(current_branch(&root.join("web")), web_before);
        assert!(!branch_exists(
            &root.join("api"),
            &iteration_branch("prj_test0002")
        ));
    }

    #[test]
    fn multi_baseline_rejects_branch_checked_out_in_another_worktree() {
        let tmp = Tmp::new("conflict");
        let root = tmp.path();
        init_repo(&root.join("api"));
        init_repo(&root.join("web"));
        let branch = iteration_branch("prj_test0003");
        // web 的迭代分支已被另一个 worktree 占用
        git_test(&root.join("web"), &["branch", &branch, "HEAD"]);
        let other = root.join("other-worktree");
        git_test(
            &root.join("web"),
            &["worktree", "add", other.to_str().unwrap(), &branch],
        );
        let api_before = current_branch(&root.join("api"));

        let err = apply_multi(root, &repos(&["api", "web"]), "prj_test0003").unwrap_err();

        assert!(err.repos.iter().any(|r| r.repo == "web"));
        assert_eq!(current_branch(&root.join("api")), api_before);
        assert!(!branch_exists(&root.join("api"), &branch));
    }

    #[test]
    fn single_baseline_records_branch_or_detached_commit() {
        let tmp = Tmp::new("single");
        let root = tmp.path();
        init_repo(root);

        let baseline = single_baseline(root, ".").unwrap();
        let branch = current_branch(root).unwrap();
        assert_eq!(baseline["branch"], branch);

        git_test(root, &["checkout", "--detach", "HEAD"]);
        let commit = git_ok(root, &["rev-parse", "HEAD"]).unwrap();
        let baseline = single_baseline(root, ".").unwrap();
        assert_eq!(baseline["commit"], commit);
        assert!(baseline.get("branch").is_none());
    }

    #[cfg(unix)]
    fn set_mode(perm: &mut std::fs::Permissions, bits: u32, add: bool) {
        use std::os::unix::fs::PermissionsExt;
        let mode = perm.mode();
        perm.set_mode(if add { mode | bits } else { mode & !bits });
    }

    #[cfg(not(unix))]
    fn set_mode(perm: &mut std::fs::Permissions, _bits: u32, add: bool) {
        perm.set_readonly(!add);
    }

    fn make_read_only(p: &Path) {
        if let Ok(md) = std::fs::symlink_metadata(p) {
            let mut perm = md.permissions();
            set_mode(&mut perm, 0o222, false);
            let _ = std::fs::set_permissions(p, perm);
        }
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    make_read_only(&e.path());
                }
            }
        }
    }
}
