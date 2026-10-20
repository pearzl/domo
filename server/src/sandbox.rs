//! M4 任务沙盒（并发隔离，DESIGN §13）。
//!
//! 心智模型：项目工作区目录 `~/.domo/workspace/<prj>/`（可用 DOMO_WORKSPACE_ROOT 覆盖）
//! 是 agent 的家；任务沙盒永远在它内部——
//! - 代码任务（git）：`<prj>/wt/<tsk_short>/<repo>/` 为 worktree（分支 `domo/task-<short>`），
//!   源仓库（显式 local / git-url clone）只作为**源**，验收通过后由系统 merge 回；
//! - 非代码任务：`<prj>/tasks/<tsk_short>/` 独立子目录。
//!
//! 隔离边界：执行/验收都在沙盒内；每轮执行结束后对「scope 外的源仓库」做脏文件
//! 基线比对，发现 agent 绕过沙盒直写源仓库 → `sandbox.violation` 事件 + 收件箱。
//!
//! 合入规则：单仓验收通过后把任务分支合回；冲突一律交人（`task.merge_conflict` + 收件箱），
//! 已 abort 的 merge 保留 worktree 供人工处置，**禁止**静默强制覆盖或丢弃 agent 改动。
//!
//! 共享目录（多仓父目录 / 非 git 目录）不使用 worktree：直接在工程目录内串行工作，
//! git 分支与提交由用户管理（方案 B，见 DESIGN §4.2）。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::{
    db, events,
    models::{Project, Task},
    state::App,
};

/// 只在需要读写数据库时短暂持锁：**任何 git / 文件 IO 都不得在锁内执行**。
fn with_db<R>(state: &Arc<App>, f: impl FnOnce(&Connection) -> R) -> R {
    let c = state.db.lock().unwrap();
    f(&c)
}

/// DOMO 管辖的项目沙盒根：<workspace_root>/<project_id>。
pub fn sandbox_root(project_id: &str) -> PathBuf {
    crate::api::workspace_root().join(project_id)
}

fn short_id(id: &str) -> &str {
    id.rsplit('_').next().unwrap_or(id)
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
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn is_git_repo(dir: &Path) -> bool {
    dir.is_dir() && git(dir, &["rev-parse", "--is-inside-work-tree"]).map(|o| o.status.success()).unwrap_or(false)
}

/// 方案 B 自动识别：锚点本身是 git 仓库根 → `single`（任务 worktree 并行）；
/// 其余（多仓父目录、非 git 目录、仓库子目录）→ `plain`（共享目录，串行）。
///
/// 只读 git 元数据，不创建分支、不修改仓库。
pub fn detect_workspace_kind(path: &Path) -> &'static str {
    if !path.is_dir() {
        return "plain";
    }
    let Ok(out) = git(path, &["rev-parse", "--show-toplevel"]) else {
        return "plain";
    };
    if !out.status.success() {
        return "plain";
    }
    let top_raw = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    let here = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let top = top_raw.canonicalize().unwrap_or(top_raw);
    if here == top {
        "single"
    } else {
        "plain"
    }
}

/// 项目工作区内登记的仓库：rel（相对工作区）+ main。
pub fn scan_repos(workspace: &Path) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    if is_git_repo(workspace) {
        out.push((".".into(), true));
        return out;
    }
    if let Ok(entries) = std::fs::read_dir(workspace) {
        let mut subs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        subs.sort();
        for p in subs {
            if p.file_name().map(|n| n.to_string_lossy().starts_with('.')).unwrap_or(false) {
                continue;
            }
            if is_git_repo(&p) {
                let rel = p.strip_prefix(workspace).unwrap_or(&p).display().to_string();
                out.push((rel, out.is_empty()));
            } else if let Ok(inner) = std::fs::read_dir(&p) {
                let mut subs2: Vec<PathBuf> = inner.flatten().map(|e| e.path()).filter(|q| q.is_dir()).collect();
                subs2.sort();
                for q in subs2 {
                    if is_git_repo(&q) {
                        let rel = q.strip_prefix(workspace).unwrap_or(&q).display().to_string();
                        out.push((rel, out.is_empty()));
                    }
                }
            }
        }
    }
    out
}

/// 任务沙盒准备结果。
pub struct Sandbox {
    /// agent 的 cwd（永远在项目工作区内）。
    pub cwd: PathBuf,
    /// 代码任务的 worktree 列表（rel 仓库路径 → worktree 路径 + 分支）。
    pub worktrees: Vec<(String, PathBuf, String)>,
    /// 项目源仓库根（显式 local / clone 目录）。
    pub source_root: PathBuf,
}

fn task_scope(task: &Task, repos: &[(String, bool)]) -> Vec<String> {
    let default_scope = || -> Vec<String> {
        repos
            .iter()
            .find(|(_, main)| *main)
            .map(|(rel, _)| vec![rel.clone()])
            .unwrap_or_default()
    };
    if task.scope_repos.is_empty() {
        return default_scope();
    }
    let filtered: Vec<String> = task
        .scope_repos
        .iter()
        .filter(|r| repos.iter().any(|(rel, _)| rel == *r))
        .cloned()
        .collect();
    if filtered.is_empty() {
        default_scope()
    } else {
        filtered
    }
}

/// 准备（幂等）任务沙盒并返回 agent 的 cwd。
///
/// 只做必要的短锁读取；创建 worktree 等 git 操作全部在锁外执行。
pub fn prepare(state: &Arc<App>, project: &Project, task: &Task) -> Result<Sandbox, String> {
    let (source_root, repos) = with_db(state, |c| {
        (
            db::project_workdir(c, project),
            db::project_repos(c, &project.id)
                .iter()
                .map(|r| (r.rel.clone(), r.main))
                .collect::<Vec<_>>(),
        )
    });
    let source_root =
        source_root.ok_or_else(|| "project has no workspace (set a workspace or use the default one)".to_string())?;
    let root = sandbox_root(&project.id);
    let ts = short_id(&task.id);
    // 方案 B：单仓工程走 worktree；共享目录（multi/plain/legacy）直接在工程目录工作
    // （项目/任务串行；不用 worktree，不创建/切换项目分支，也不做跨仓合入编排）。
    let direct_mode = project
        .engineering_id
        .as_deref()
        .and_then(|eid| with_db(state, |c| db::get_engineering(c, eid)))
        .map(|e| e.workspace_kind != "single")
        .unwrap_or(false);
    if direct_mode {
        with_db(state, |c| {
            events::push_event(state, c, &project.id, Some(&task.id), "task.sandbox_ready",
                json!({"cwd": source_root.display().to_string(), "mode": "direct_workspace",
                       "hint": "共享目录：直接在工程目录工作（项目/任务串行，git 分支由用户管理）"}));
        });
        return Ok(Sandbox { cwd: source_root.clone(), worktrees: vec![], source_root });
    }

    if repos.is_empty() {
        // 工作区内没有登记到 git 仓库：
        // - 默认工作区（DOMO 管辖的空目录）→ 非编程任务，用独立子目录隔离；
        // - 显式指定的非 git 目录（用户自己的代码/资料目录）→ 直接在其中工作
        //   （无版本控制即无 merge 语义；UI 已提示建议 git init 以获得隔离）。
        let explicit_non_default = project
            .workspace
            .as_ref()
            .map(|w| w.get("default").and_then(|d| d.as_bool()).unwrap_or(false) == false)
            .unwrap_or(false);
        if explicit_non_default {
            with_db(state, |c| {
                events::push_event(state, c, &project.id, Some(&task.id), "task.sandbox_ready",
                    json!({"cwd": source_root.display().to_string(), "mode": "direct_non_git",
                           "hint": "显式指定的非 git 工作区：任务直接在其中工作（无隔离）；建议 git init 以获得 worktree 隔离"}));
            });
            return Ok(Sandbox { cwd: source_root.clone(), worktrees: vec![], source_root });
        }
        let dir = root.join("tasks").join(ts);
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create task sandbox {}: {e}", dir.display()))?;
        return Ok(Sandbox { cwd: dir, worktrees: vec![], source_root });
    }

    let scope = task_scope(task, &repos);
    let mut worktrees: Vec<(String, PathBuf, String)> = Vec::new();
    for rel in &scope {
        let src = if rel == "." { source_root.clone() } else { source_root.join(rel) };
        let repo_name = if rel == "." {
            src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "repo".into())
        } else {
            rel.replace('/', "_")
        };
        let wt = root.join("wt").join(ts).join(&repo_name);
        let branch = format!("domo/task-{ts}");
        if !wt.exists() {
            if let Some(parent) = wt.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            // 分支可能已存在（上一轮任务保留）→ 复用
            let branch_exists = git(&src, &["rev-parse", "--verify", &branch])
                .map(|o| o.status.success())
                .unwrap_or(false);
            let out = if branch_exists {
                git(&src, &["worktree", "add", wt.to_str().unwrap_or(""), &branch])
            } else {
                git(&src, &["worktree", "add", wt.to_str().unwrap_or(""), "-b", &branch, "HEAD"])
            }?;
            if !out.status.success() {
                return Err(format!(
                    "git worktree add failed for {rel}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            with_db(state, |c| {
                events::push_event(state, c, &project.id, Some(&task.id), "task.sandbox_created",
                    json!({"repo": rel, "worktree": wt.display().to_string(), "branch": branch}));
            });
        }
        worktrees.push((rel.clone(), wt, branch));
    }
    // cwd = 主仓库（若在 scope）否则 scope 第一个的 worktree
    let cwd = worktrees
        .iter()
        .find(|(rel, _, _)| repos.iter().any(|(r, main)| r == rel && *main))
        .or_else(|| worktrees.first())
        .map(|(_, wt, _)| wt.clone())
        .ok_or_else(|| "no worktree prepared (empty repo scope)".to_string())?;

    // 违规基线：scope 外源仓库的脏文件集合（快速比对用）
    let baseline = violation_snapshot(&source_root, &repos, &scope);
    with_db(state, |c| {
        events::push_event(state, c, &project.id, Some(&task.id), "task.sandbox_ready",
            json!({"cwd": cwd.display().to_string(), "scope": scope, "baseline": baseline}));
    });

    Ok(Sandbox { cwd, worktrees, source_root })
}

/// scope 外仓库的脏文件基线（不包含 scope 内仓库，它们的脏改动属于任务自身）。
fn violation_snapshot(source_root: &Path, repos: &[(String, bool)], scope: &[String]) -> Value {
    let mut snap = serde_json::Map::new();
    for (rel, _) in repos {
        if scope.iter().any(|s| s == rel) {
            continue;
        }
        let dir = if rel == "." { source_root.to_path_buf() } else { source_root.join(rel) };
        if let Ok(text) = git_ok(&dir, &["status", "--porcelain"]) {
            let files: Vec<String> = text.lines().map(|l| l.to_string()).collect();
            snap.insert(rel.clone(), json!(files));
        }
    }
    Value::Object(snap)
}

/// 每轮执行后检测 scope 外源仓库是否出现**新增**脏文件（agent 绕过沙盒直写）。
pub fn detect_violations(state: &Arc<App>, project: &Project, task: &Task) -> Vec<String> {
    let (source_root, repos) = with_db(state, |c| {
        (
            db::project_workdir(c, project),
            db::project_repos(c, &project.id)
                .iter()
                .map(|r| (r.rel.clone(), r.main))
                .collect::<Vec<_>>(),
        )
    });
    let Some(source_root) = source_root else { return vec![] };
    if repos.is_empty() {
        return vec![];
    }
    let scope = task_scope(task, &repos);
    let now = violation_snapshot(&source_root, &repos, &scope);
    // 基线来自最近一次 sandbox_ready 事件
    let baseline = with_db(state, |c| {
        db::list_events(c, None, Some(&task.id))
            .into_iter()
            .rev()
            .find(|e| e.kind == "task.sandbox_ready")
            .and_then(|e| e.payload.get("baseline").cloned())
            .unwrap_or(json!({}))
    });
    let mut violations: Vec<String> = Vec::new();
    if let Some(map) = now.as_object() {
        for (rel, files) in map {
            let before: Vec<String> = baseline
                .get(rel)
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let after: Vec<String> = files.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            let added: Vec<String> = after.into_iter().filter(|f| !before.contains(f)).collect();
            if !added.is_empty() {
                violations.push(format!("{rel}: {}", added.iter().take(5).cloned().collect::<Vec<_>>().join("; ")));
            }
        }
    }
    if !violations.is_empty() {
        with_db(state, |c| {
            events::push_event(state, c, &project.id, Some(&task.id), "sandbox.violation",
                json!({"violations": violations, "hint": "agent 直接修改了 scope 外的源仓库（应只在 worktree 内工作）"}));
        });
    }
    violations
}

/// 验收通过后的合并结果。
pub enum MergeResult {
    /// 无 worktree（非代码任务）或全部合并成功
    Ok(Vec<String>),
    /// 某仓库冲突（已 abort 本次 merge，worktree 保留）
    Conflict { repo: String, detail: String },
}

/// 把所有 worktree 分支 merge 回源仓库（--no-ff），并清理 worktree 与分支。
pub fn merge_all(state: &Arc<App>, project: &Project, task: &Task, remove: bool) -> MergeResult {
    let Some(sb) = prepare(state, project, task).ok() else {
        return MergeResult::Ok(vec![]);
    };
    let mut merged: Vec<String> = Vec::new();
    for (rel, wt, branch) in &sb.worktrees {
        let src = if rel == "." { sb.source_root.clone() } else { sb.source_root.join(rel) };
        // worktree 内是否有未提交改动 → 先自动提交，保证 merge 语义完整
        if git_ok(wt, &["status", "--porcelain"]).map(|s| !s.trim().is_empty()).unwrap_or(false) {
            let _ = git(wt, &["add", "-A"]);
            let _ = git(wt, &["commit", "-m", &format!("domo: task {} ({}) ", short_id(&task.id), task.title)]);
        }
        let out = match git(&src, &["merge", "--no-ff", "-m",
            &format!("domo: merge task {} ({})", short_id(&task.id), task.title), branch]) {
            Ok(o) => o,
            Err(e) => return MergeResult::Conflict { repo: rel.clone(), detail: e },
        };
        if !out.status.success() {
            let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let _ = git(&src, &["merge", "--abort"]);
            with_db(state, |c| {
                events::push_event(state, c, &project.id, Some(&task.id), "task.merge_conflict",
                    json!({"repo": rel, "branch": branch, "detail": detail}));
            });
            return MergeResult::Conflict { repo: rel.clone(), detail };
        }
        with_db(state, |c| {
            events::push_event(state, c, &project.id, Some(&task.id), "task.merged",
                json!({"repo": rel, "branch": branch}));
        });
        merged.push(rel.clone());
        if remove {
            // 工作区销毁前先停宿主：宿主以 cwd 为键，避免路径复用后拿到旧实例
            crate::host::stop(wt);
            let _ = git(&src, &["worktree", "remove", "--force", wt.to_str().unwrap_or("")]);
            let _ = git(&src, &["branch", "-D", branch]);
        }
    }
    MergeResult::Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "domo-sandbox-{tag}-{}-{}",
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
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git_init(dir: &Path) {
        let out = Command::new("git").arg("-C").arg(dir).args(["init", "-q"]).output().unwrap();
        assert!(out.status.success());
    }

    #[test]
    fn workspace_kind_is_detected_from_anchor() {
        let tmp = Tmp::new("kind");
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(detect_workspace_kind(&plain), "plain");

        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_init(&repo);
        assert_eq!(detect_workspace_kind(&repo), "single");
        // 仓库子目录不是锚点根 → 共享目录
        let sub = repo.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(detect_workspace_kind(&sub), "plain");

        // 多仓父目录 → 共享目录
        assert_eq!(detect_workspace_kind(tmp.path()), "plain");
    }
}
