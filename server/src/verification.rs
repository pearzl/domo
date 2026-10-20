//! 验收机械检查（DESIGN §9）：只做两类——文件存在性、命令退出码。
//!
//! - 检查项由任务保存（`db::get_task_checks`），每项形如
//!   `{"type":"file_exists","path":"a.txt"}`、`{"type":"file_not_exists","path":"b"}`、
//!   `{"type":"command","cmd":"cargo test","expect_exit":0}`。
//! - 全部在**任务工作区**内执行；路径必须是相对路径且不得包含 `..`。
//! - 结果只作为证据（`acceptance_checks_completed` 事件 + evidence 表），
//!   最终结论仍由验收者给出；本模块不做通过/退回判定。
//! - 命令不设白名单，但有 120s 时限与输出截断；执行环境与 agent 相同。

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    db, events,
    models::{Project, Task},
    sandbox,
    state::App,
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const OUTPUT_LIMIT: usize = 2000;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Check {
    FileExists { path: String },
    FileNotExists { path: String },
    Command {
        cmd: String,
        #[serde(default)]
        expect_exit: i64,
    },
}

/// 执行任务的全部机械检查并落证据；无检查项时返回空向量。
pub async fn run(state: &Arc<App>, project: &Project, task: &Task) -> Vec<Value> {
    let raws = {
        let c = state.db.lock().unwrap();
        db::get_task_checks(&c, &task.id)
    };
    if raws.is_empty() {
        return vec![];
    }
    let cwd = match sandbox::prepare(state, project, task) {
        Ok(sb) => sb.cwd,
        Err(e) => {
            return raws
                .iter()
                .map(|r| {
                    json!({
                        "type": r.get("type").cloned().unwrap_or(json!("unknown")),
                        "passed": false,
                        "detail": format!("任务工作区不可用，无法执行检查：{e}"),
                    })
                })
                .collect()
        }
    };

    let mut results = Vec::new();
    for raw in raws {
        let result = match serde_json::from_value::<Check>(raw.clone()) {
            Ok(check) => run_one(&cwd, &check).await,
            Err(e) => json!({
                "type": raw.get("type").cloned().unwrap_or(json!("unknown")),
                "passed": false,
                "detail": format!("检查项格式错误：{e}"),
            }),
        };
        results.push(result);
    }

    {
        let c = state.db.lock().unwrap();
        db::insert_acceptance_evidence(&c, &task.id, task.attempt, &json!(results));
        events::push_event(
            state,
            &c,
            &task.project_id,
            Some(&task.id),
            "acceptance.checks_completed",
            json!({"results": results}),
        );
    }
    results
}

async fn run_one(cwd: &Path, check: &Check) -> Value {
    match check {
        Check::FileExists { path } => match resolve(cwd, path) {
            None => blocked(check),
            Some(p) => {
                let ok = p.is_file() || p.exists();
                json!({"type": "file_exists", "path": path, "passed": ok,
                       "detail": if ok { "文件存在".into() } else { format!("文件不存在：{}", p.display()) }})
            }
        },
        Check::FileNotExists { path } => match resolve(cwd, path) {
            None => blocked(check),
            Some(p) => {
                let gone = !p.exists();
                json!({"type": "file_not_exists", "path": path, "passed": gone,
                       "detail": if gone { "文件不存在（符合预期）".into() } else { format!("文件仍然存在：{}", p.display()) }})
            }
        },
        Check::Command { cmd, expect_exit } => {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c")
                .arg(cmd)
                .current_dir(cwd)
                .kill_on_drop(true)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            match tokio::time::timeout(COMMAND_TIMEOUT, c.output()).await {
                Err(_) => json!({"type": "command", "cmd": cmd, "passed": false,
                                 "detail": format!("命令超时（{}s）", COMMAND_TIMEOUT.as_secs())}),
                Ok(Err(e)) => json!({"type": "command", "cmd": cmd, "passed": false,
                                     "detail": format!("命令无法执行：{e}")}),
                Ok(Ok(out)) => {
                    let code = out.status.code().unwrap_or(-1);
                    let mut detail = String::new();
                    if !out.stdout.is_empty() {
                        detail.push_str(&tail(&String::from_utf8_lossy(&out.stdout), OUTPUT_LIMIT));
                    }
                    if !out.stderr.is_empty() {
                        if !detail.is_empty() {
                            detail.push('\n');
                        }
                        detail.push_str(&tail(&String::from_utf8_lossy(&out.stderr), OUTPUT_LIMIT));
                    }
                    json!({"type": "command", "cmd": cmd, "expect_exit": expect_exit,
                           "exit_code": code, "passed": code as i64 == *expect_exit,
                           "detail": detail})
                }
            }
        }
    }
}

fn blocked(check: &Check) -> Value {
    json!({"type": check_type(check), "passed": false,
           "detail": "路径必须是工作区内的相对路径（不允许绝对路径或 ..）"})
}

fn check_type(c: &Check) -> &'static str {
    match c {
        Check::FileExists { .. } => "file_exists",
        Check::FileNotExists { .. } => "file_not_exists",
        Check::Command { .. } => "command",
    }
}

/// 只允许工作区内的相对路径：绝对路径、`..` 一律拒绝。
fn resolve(cwd: &Path, p: &str) -> Option<PathBuf> {
    let pb = Path::new(p);
    if pb.is_absolute() {
        return None;
    }
    if pb.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    Some(cwd.join(pb))
}

fn tail(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().rev().take(max).collect::<Vec<_>>().into_iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("domo-check-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn file_checks_and_command() {
        let d = tmpdir();
        std::fs::write(d.join("a.txt"), "hi").unwrap();
        assert!(run_one(&d, &Check::FileExists { path: "a.txt".into() }).await["passed"].as_bool().unwrap());
        assert!(!run_one(&d, &Check::FileExists { path: "missing".into() }).await["passed"].as_bool().unwrap());
        assert!(run_one(&d, &Check::FileNotExists { path: "missing".into() }).await["passed"].as_bool().unwrap());
        let ok = run_one(&d, &Check::Command { cmd: "exit 0".into(), expect_exit: 0 }).await;
        assert!(ok["passed"].as_bool().unwrap());
        let bad = run_one(&d, &Check::Command { cmd: "exit 3".into(), expect_exit: 0 }).await;
        assert!(!bad["passed"].as_bool().unwrap());
        assert_eq!(bad["exit_code"], 3);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rejects_paths_outside_workspace() {
        let d = Path::new("/tmp/ws");
        assert!(resolve(d, "../etc/passwd").is_none());
        assert!(resolve(d, "/etc/passwd").is_none());
        assert_eq!(resolve(d, "sub/a.txt").unwrap(), d.join("sub/a.txt"));
    }
}
