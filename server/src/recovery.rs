//! M4 三档恢复降级（DESIGN §10）：中断任务恢复依序尝试三档。
//!
//! 1. **native_resume（原生会话恢复）**：agent 声明 `supports_resume` 且该中断 attempt 的
//!    真实 opencode 会话 id 已持久化 → `opencode run --session <同一真实id>` 直接续作
//!    （attempt 不自增、逻辑会话名 `exec_session(task_id, attempt)` 不变）。
//! 2. **handoff（交接摘要重建）**：第一档跳过或失败 → 从事件流 + 任务数据生成结构化
//!    交接书（目标/已完成/剩余/约束/产物路径），以**新会话**（新 session id ≠ 原 id）续作。
//! 3. **manual（人工重建）**：两档都失败 → 任务 `suspended` + 收件箱条目
//!    （payload 同时带两档失败原因）；每轮重启至多重新进一轮三档。
//!
//! 审计契约：每档各落一条 append-only 事件 `task.recovered`，payload 固定携带
//! `tier / outcome / reason / task_id / attempt / session_id`。

use crate::{db, events, models::Task, state::App};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::sync::Arc;

pub const TIER_NATIVE: &str = "native_resume";
pub const TIER_HANDOFF: &str = "handoff";
pub const TIER_MANUAL: &str = "manual";

pub const OUTCOME_SUCCEEDED: &str = "succeeded";
pub const OUTCOME_FAILED: &str = "failed";
pub const OUTCOME_SKIPPED: &str = "skipped";
pub const OUTCOME_SUSPENDED: &str = "suspended";

/// 收件箱条目 reason（第三档人工重建挂起），与 failure_limit 等既有升级项区分。
pub const INBOX_REASON: &str = "recovery_exhausted";

// ---------- 启动标记 ----------

/// 启动期标记（`db::open` 调用）：没有 worker 能跨重启存活。
///
/// - `executing` 且**当前 attempt 没有失败记录** → 进程重启中断（真·中断），置三档待恢复；
///   有失败记录的属于「失败后等重试」，仍走原有 attempt+1 重派语义。
/// - `suspended` 且 `recovery_manual=1` → 上一轮第三档人工挂起，重启后重新进一轮三档
///   （一轮失败即回到 suspended + pending=0，同一 boot 内不会反复重试）。
///
/// 标记本身不派发任何东西：实际派发由调度器 scan 执行，且受项目 `paused` 冻结约束。
pub fn boot_mark(c: &Connection) {
    let interrupted: Vec<(String, i64)> = {
        let mut st = match c.prepare("SELECT id, attempt FROM tasks WHERE status='executing'") {
            Ok(s) => s,
            Err(_) => return,
        };
        st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    };
    for (id, attempt) in interrupted {
        if attempt_has_failure(c, &id, attempt) {
            continue; // 失败重试路径（不进三档，保持 attempt+1 语义）
        }
        tracing::info!(task = %id, attempt, "boot: interrupted execution marked for three-tier recovery");
        // M5 状态归一：中断尝试不再停留在 executing（那会让任务视图把「等待恢复/排队」
        // 误显示为「正在执行」），改为显式 recovering；真正派发恢复时才回到 executing。
        db::recovery_set(c, &id, Some(true), Some(TIER_NATIVE), None);
        let _ = c.execute(
            "UPDATE tasks SET status='recovering' WHERE id=?1 AND status='executing'",
            params![id],
        );
    }
    // 第三档人工重建挂起的任务：本次重启重新进一轮三档（每轮重启至多一轮）
    let _ = c.execute(
        "UPDATE tasks SET recovery_pending=1, recovery_tier=?1 WHERE status='suspended' AND recovery_manual=1",
        params![TIER_NATIVE],
    );
}

/// 该 attempt 是否已记录执行失败（有 → 是失败后的重试，不是中断）。
fn attempt_has_failure(c: &Connection, task_id: &str, attempt: i64) -> bool {
    let mut st = match c.prepare(
        "SELECT payload FROM events WHERE task_id=?1 AND type='task.execution_failed' ORDER BY id DESC LIMIT 10",
    ) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let payloads: Vec<String> = st
        .query_map(params![task_id], |r| r.get::<_, String>(0))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    payloads.iter().any(|p| {
        serde_json::from_str::<Value>(p)
            .ok()
            .and_then(|v| v.get("attempt").and_then(|a| a.as_i64()))
            == Some(attempt)
    })
}

// ---------- 审计事件契约 ----------

/// 落一条 `task.recovered` 审计事件（append-only）。
///
/// payload 固定字段：`tier` / `outcome` / `reason` / `task_id` / `attempt` / `session_id`，
/// `extra` 用于补充档位专属信息（如交接书全文与落盘路径、两档失败原因）。
pub fn recovered(
    state: &Arc<App>,
    c: &Connection,
    t: &Task,
    tier: &str,
    outcome: &str,
    reason: &str,
    session_id: Option<&str>,
    extra: Value,
) -> Value {
    let mut payload = json!({
        "tier": tier,
        "outcome": outcome,
        "reason": reason,
        "task_id": t.id,
        "attempt": t.attempt,
        "session_id": session_id.map(String::from),
    });
    if let (Some(o), Some(v)) = (extra.as_object(), payload.as_object_mut()) {
        for (k, val) in o {
            v.entry(k.clone()).or_insert_with(|| val.clone());
        }
    }
    tracing::info!(
        task = %t.id, tier, outcome, attempt = t.attempt,
        "task recovered: {tier}/{outcome} — {reason}"
    );
    events::push_event(state, c, &t.project_id, Some(&t.id), "task.recovered", payload.clone());
    payload
}

/// 该任务该 attempt 最近一次 `task.recovered`（指定档位）的 reason——第三档挂起时
/// 用于把第一/第二档失败原因一并写进收件箱 payload。
pub fn last_tier_reason(c: &Connection, task_id: &str, attempt: i64, tier: &str) -> Option<String> {
    let mut st = c
        .prepare("SELECT payload FROM events WHERE task_id=?1 AND type='task.recovered' ORDER BY id DESC")
        .ok()?;
    let rows: Vec<String> = st
        .query_map(params![task_id], |r| r.get::<_, String>(0))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    for raw in rows {
        let v: Value = serde_json::from_str(&raw).ok()?;
        if v.get("tier").and_then(|x| x.as_str()) == Some(tier)
            && v.get("attempt").and_then(|x| x.as_i64()) == Some(attempt)
        {
            return v.get("reason").and_then(|x| x.as_str()).map(String::from);
        }
    }
    None
}

/// 已产生的交接轮次数（决定交接会话名 `{task}_a{attempt}_h{n}` 的 n）。
pub fn handoff_count(c: &Connection, task_id: &str, attempt: i64) -> i64 {
    let mut st = match c.prepare("SELECT payload FROM events WHERE task_id=?1 AND type='task.recovered' ORDER BY id") {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let rows: Vec<String> = st
        .query_map(params![task_id], |r| r.get::<_, String>(0))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    rows.iter()
        .filter(|raw| {
            serde_json::from_str::<Value>(raw)
                .ok()
                .map(|v| {
                    v.get("tier").and_then(|x| x.as_str()) == Some(TIER_HANDOFF)
                        && v.get("attempt").and_then(|x| x.as_i64()) == Some(attempt)
                })
                .unwrap_or(false)
        })
        .count() as i64
}

// ---------- 第二档：结构化交接书 ----------

/// 交接书五节（DESIGN §10.2）：目标 / 已完成 / 剩余 / 约束 / 产物路径。
/// 同时保存渲染好的 Markdown 与落盘路径，随 `task.recovered` 事件持久化。
#[derive(Debug, Clone)]
pub struct HandoffDoc {
    pub goal: String,
    pub done: String,
    pub remaining: String,
    pub constraints: String,
    pub artifacts: String,
    pub markdown: String,
    pub path: String,
}

impl HandoffDoc {
    /// 事件 payload 用：五节齐全（中文键名与验收口径一一对应）+ 落盘信息。
    pub fn to_value(&self) -> Value {
        json!({
            "目标": self.goal,
            "已完成": self.done,
            "剩余": self.remaining,
            "约束": self.constraints,
            "产物路径": self.artifacts,
            "markdown": self.markdown,
            "path": self.path,
        })
    }
}

fn cut_line(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

/// 从事件流 + 任务数据生成交接书并落盘（`<data>/projects/<pid>/handoffs/`）。
pub fn build_handoff(c: &Connection, t: &Task, handoff_no: i64) -> HandoffDoc {
    let project = db::get_project(c, &t.project_id);
    let workspace = project
        .as_ref()
        .and_then(|p| db::project_workdir(c, p))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(未设置工作区)".into());
    let criteria = t
        .acceptance_criteria
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "(未指定)".into());
    let guidance = db::latest_guidance(c, &t.id);

    let logs = db::task_logs(c, &t.id);
    let mut log_tail: Vec<String> = logs
        .iter()
        .rev()
        .take(8)
        .map(|l| cut_line(&format!("- [{}] {}", l.ts, l.line), 300))
        .collect();
    log_tail.reverse();
    let started = c
        .query_row(
            "SELECT ts FROM events WHERE task_id=?1 AND type='agent.session_started' ORDER BY id DESC LIMIT 1",
            params![t.id],
            |r| r.get::<_, i64>(0),
        )
        .ok();
    let done = if logs.is_empty() {
        "（本轮暂无执行日志；上一轮会话在启动后即中断）".to_string()
    } else {
        format!(
            "上一轮执行会话已启动{}（attempt {}），累计 {} 条日志；最近输出：\n{}",
            started.map(|ts| format!("（时间戳 {ts}）")).unwrap_or_default(),
            t.attempt,
            logs.len(),
            log_tail.join("\n")
        )
    };
    let artifacts = if t.artifacts.is_empty() {
        format!("{workspace}（任务级 artifacts 尚无记录，产物默认落在工作区内）")
    } else {
        format!("{}\n任务 artifacts：{}", workspace, t.artifacts.join("、"))
    };

    let goal = format!(
        "任务《{}》：{}\n验收标准：{}",
        t.title,
        if t.description.trim().is_empty() { "(无补充描述)" } else { t.description.trim() },
        criteria
    );
    let remaining = format!(
        "在不重复已完成部分的前提下，继续完成任务描述中尚未完成的工作，直到满足验收标准：{criteria}；完成后按提示输出 TASK_DONE 与完成说明。"
    );
    let constraints = format!(
        "工作目录 = 项目工作区（{}），只改动与本任务相关的文件；{}\n原任务约束不变：验收标准「{criteria}」；不要向用户提问，直接动手完成。",
        workspace,
        guidance
            .map(|g| format!("必须遵守最新人工指导：{g}"))
            .unwrap_or_else(|| "(当前无人工指导)".into()),
    );

    let markdown = format!(
        "# 任务交接书（handoff #{handoff_no}）\n\n\
         > 上一轮执行会话因进程重启中断，本轮以**新会话**续作。已完成部分不要重做。\n\n\
         ## 目标\n\n{goal}\n\n## 已完成\n\n{done}\n\n## 剩余\n\n{remaining}\n\n\
         ## 约束\n\n{constraints}\n\n## 产物路径\n\n{artifacts}\n"
    );

    let dir = db::data_dir().join("projects").join(&t.project_id).join("handoffs");
    let path = dir.join(format!("{}_a{}_h{}.md", t.id, t.attempt, handoff_no));
    let path_str = path.display().to_string();
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &markdown)) {
        tracing::warn!(task = %t.id, error = %e, path = %path_str, "handoff doc could not be written to disk (event payload still carries it)");
    }

    HandoffDoc { goal, done, remaining, constraints, artifacts, markdown, path: path_str }
}

// ---------- 第三档：人工重建（挂起 + 收件箱） ----------

/// 两档均失败 → 任务 `suspended` + `task.recovered`(manual/suspended) + 收件箱条目。
/// 收件箱按 (task, reason=recovery_exhausted) 去重：同一任务不重复堆叠，
/// 重复挂起只更新原条目的两档失败原因。
pub fn suspend_manual(state: &Arc<App>, c: &Connection, t: &Task) {
    let tier1 = last_tier_reason(c, &t.id, t.attempt, TIER_NATIVE)
        .unwrap_or_else(|| "第一档未执行（无记录）".into());
    let tier2 = last_tier_reason(c, &t.id, t.attempt, TIER_HANDOFF)
        .unwrap_or_else(|| "第二档未执行（无记录）".into());
    db::recovery_set(c, &t.id, Some(false), None, Some(true));
    let t = events::status_changed(state, c, t, "suspended", INBOX_REASON);

    let reason = format!(
        "三档恢复均未成功：第一档原生会话恢复（native_resume）——{tier1}；第二档交接摘要重建（handoff）——{tier2}"
    );
    recovered(
        state,
        c,
        &t,
        TIER_MANUAL,
        OUTCOME_SUSPENDED,
        &reason,
        None,
        json!({ "tier1_reason": tier1, "tier2_reason": tier2 }),
    );

    let detail = format!(
        "任务「{}」中断恢复两档均失败，已挂起等待人工重建（第一档失败：{}；第二档失败：{}）。无依赖分支不受影响；下次重启会自动再进一轮三档恢复。",
        t.title, tier1, tier2
    );
    let executor = crate::scheduler::agent_summary(c, &t.executor_agent_id);
    let payload = json!({
        "task_id": t.id,
        "reason": INBOX_REASON,
        "phase": "execute",
        "detail": detail,
        "tier1": {"tier": TIER_NATIVE, "outcome": OUTCOME_FAILED, "reason": tier1},
        "tier2": {"tier": TIER_HANDOFF, "outcome": OUTCOME_FAILED, "reason": tier2},
        "tier1_reason": tier1,
        "tier2_reason": tier2,
        "last_error": tier2,
        "attempt": t.attempt,
        "executor": executor,
    });
    events::push_event(
        state,
        c,
        &t.project_id,
        Some(&t.id),
        "escalation.raised",
        json!({
            "task_id": t.id, "reason": INBOX_REASON, "phase": "execute",
            "detail": detail, "last_error": tier2,
            "tier1_reason": tier1, "tier2_reason": tier2,
            "attempt": t.attempt,
        }),
    );

    // 收件箱去重：已有 open 条目 → 只更新原因，不新增。
    let existing = db::list_inbox(c).into_iter().find(|i| {
        i.kind == "escalation"
            && i.status == "open"
            && i.payload.get("task_id").and_then(|v| v.as_str()) == Some(t.id.as_str())
            && i.payload.get("reason").and_then(|v| v.as_str()) == Some(INBOX_REASON)
    });
    match existing {
        Some(item) => {
            let _ = c.execute("UPDATE inbox SET payload=?2 WHERE id=?1", params![item.id, payload.to_string()]);
            if let Some(fresh) = db::get_inbox(c, &item.id) {
                events::send_inbox(state, &fresh);
            }
        }
        None => {
            events::new_inbox_item(state, c, &t.project_id, "escalation", payload);
        }
    }
}

/// 恢复重入（任务脱离挂起、系统自动重试一轮三档）时结档旧的 `recovery_exhausted` 条目：
/// 避免出现「条目说已挂起、任务实际在跑」的陈旧状态；若本轮再次失败，会重新登记新条目。
pub fn mark_recovery_retry(state: &Arc<App>, c: &Connection, t: &Task) {
    let resolution = json!({
        "resolution": "auto_retry",
        "note": "系统已自动重试一轮三档恢复，任务脱离挂起；若再次失败将重新登记条目",
    });
    let items: Vec<String> = db::list_inbox(c)
        .into_iter()
        .filter(|i| {
            i.kind == "escalation"
                && i.status == "open"
                && i.payload.get("task_id").and_then(|v| v.as_str()) == Some(t.id.as_str())
                && i.payload.get("reason").and_then(|v| v.as_str()) == Some(INBOX_REASON)
        })
        .map(|i| i.id)
        .collect();
    for id in items {
        let _ = c.execute(
            "UPDATE inbox SET status='resolved', resolution=?2 WHERE id=?1",
            params![id, resolution.to_string()],
        );
        events::push_event(
            state,
            c,
            &t.project_id,
            None,
            "escalation.resolved",
            json!({"inbox_id": id, "resolution": resolution}),
        );
        if let Some(item) = db::get_inbox(c, &id) {
            events::send_inbox(state, &item);
        }
    }
}

/// 恢复成功后关闭该任务的第三档人工挂起收件箱条目（避免成功后残留 open 条目）。
pub fn close_recovery_inbox(state: &Arc<App>, c: &Connection, t: &Task) {
    let resolution = json!({
        "resolution": "auto_recovered",
        "note": "三档恢复自动续作成功，任务已脱离挂起",
    });
    let items: Vec<String> = db::list_inbox(c)
        .into_iter()
        .filter(|i| {
            i.kind == "escalation"
                && i.status == "open"
                && i.payload.get("task_id").and_then(|v| v.as_str()) == Some(t.id.as_str())
                && i.payload.get("reason").and_then(|v| v.as_str()) == Some(INBOX_REASON)
        })
        .map(|i| i.id)
        .collect();
    for id in items {
        let _ = c.execute(
            "UPDATE inbox SET status='resolved', resolution=?2 WHERE id=?1",
            params![id, resolution.to_string()],
        );
        events::push_event(
            state,
            c,
            &t.project_id,
            None,
            "escalation.resolved",
            json!({"inbox_id": id, "resolution": resolution}),
        );
        if let Some(item) = db::get_inbox(c, &id) {
            events::send_inbox(state, &item);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(c: &Connection) {
        c.execute(
            "INSERT INTO projects (id,name,brief,status,plan_content,confirmed_at,created_at) VALUES ('p1','P','', 'running',NULL,NULL,1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO tasks (id,project_id,title,description,acceptance_required,negotiation_status,status,attempt,artifacts,created_at)
             VALUES ('t1','p1','标题','描述正文',1,'agreed','executing',1,'[\"a.txt\"]',1)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn handoff_doc_has_five_sections_and_is_persisted() {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate_for_tests(&c);
        seed(&c);
        let t = crate::db::get_task(&c, "t1").unwrap();
        c.execute("INSERT INTO logs (task_id, ts, line) VALUES ('t1', 2, '写文件 progress')", []).unwrap();

        let doc = build_handoff(&c, &t, 1);
        let v = doc.to_value();
        for key in ["目标", "已完成", "剩余", "约束", "产物路径"] {
            let s = v[key].as_str().unwrap_or("");
            assert!(!s.trim().is_empty(), "section {key} must be non-empty");
            assert!(doc.markdown.contains(&format!("## {key}")), "markdown missing section {key}");
        }
        assert!(v["目标"].as_str().unwrap().contains("标题"));
        assert!(v["已完成"].as_str().unwrap().contains("写文件 progress"));
        // 落盘可复查
        assert!(std::path::Path::new(&doc.path).exists(), "handoff file {}", doc.path);
        let on_disk = std::fs::read_to_string(&doc.path).unwrap();
        assert!(on_disk.contains("## 产物路径"));
        let _ = std::fs::remove_file(&doc.path);
    }

    #[test]
    fn recovery_event_contract_fields() {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate_for_tests(&c);
        seed(&c);
        let t = crate::db::get_task(&c, "t1").unwrap();
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let state = Arc::new(crate::state::App {
            db: std::sync::Mutex::new(Connection::open_in_memory().unwrap()),
            tx,
            handles: Default::default(),
            handle_seq: Default::default(),
            wake: Default::default(),
            sessions: Default::default(),
            circuits: Default::default(),
        });
        // 事件写入独立连接（state.db 与这里不同）：直接用本地连接断言 payload 结构
        let payload = recovered(
            &state,
            &c,
            &t,
            TIER_NATIVE,
            OUTCOME_SKIPPED,
            "agent does not declare supports_resume",
            None,
            json!({}),
        );
        for k in ["tier", "outcome", "reason", "task_id", "attempt", "session_id"] {
            assert!(payload.get(k).is_some(), "missing field {k}");
        }
        assert_eq!(payload["tier"], "native_resume");
        assert_eq!(payload["outcome"], "skipped");
        assert_eq!(payload["task_id"], "t1");
        assert_eq!(payload["attempt"], 1);
        assert!(payload["session_id"].is_null());
        let evs = crate::db::list_events(&c, None, Some("t1"));
        assert!(evs.iter().any(|e| e.kind == "task.recovered"));
    }

    #[test]
    fn boot_marks_interrupted_but_not_failed_attempts() {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate_for_tests(&c);
        seed(&c);
        // t2: 中断（无失败记录）→ 标记待恢复
        c.execute("INSERT INTO tasks (id,project_id,title,status,attempt,created_at) VALUES ('t2','p1','x','executing',2,2)", []).unwrap();
        // t3: 失败后重启（当前 attempt 有失败记录）→ 不标记，走失败重试
        c.execute("INSERT INTO tasks (id,project_id,title,status,attempt,created_at) VALUES ('t3','p1','y','executing',3,3)", []).unwrap();
        c.execute(
            "INSERT INTO events (ts,project_id,task_id,\"type\",payload) VALUES (5,'p1','t3','task.execution_failed','{\"error\":\"boom\",\"attempt\":3}')",
            [],
        )
        .unwrap();
        // t4: 第三档人工挂起 → 重启重新进一轮
        c.execute("INSERT INTO tasks (id,project_id,title,status,attempt,recovery_manual,created_at) VALUES ('t4','p1','z','suspended',1,1,4)", []).unwrap();
        // t5: 常规失败挂起 → 不进三档
        c.execute("INSERT INTO tasks (id,project_id,title,status,attempt,created_at) VALUES ('t5','p1','w','suspended',3,5)", []).unwrap();

        boot_mark(&c);

        let r = crate::db::recovery_state(&c, "t1");
        assert!(r.pending && r.tier == "native_resume", "{:?}", r);
        let r2 = crate::db::recovery_state(&c, "t2");
        assert!(r2.pending && r2.tier == "native_resume");
        let r3 = crate::db::recovery_state(&c, "t3");
        assert!(!r3.pending, "failed attempt must stay on the fresh-retry path");
        let r4 = crate::db::recovery_state(&c, "t4");
        assert!(r4.pending && r4.manual, "{:?}", r4);
        let r5 = crate::db::recovery_state(&c, "t5");
        assert!(!r5.pending, "failure-limit suspension must not enter recovery");
    }

    #[test]
    fn session_map_roundtrip_across_reopen() {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate_for_tests(&c);
        seed(&c);
        crate::db::session_put(&c, "t1_a1", "ses_real_1", Some("t1"), Some("execute"), Some(1));
        assert_eq!(crate::db::session_get(&c, "t1_a1").as_deref(), Some("ses_real_1"));
        assert_eq!(crate::db::exec_resume_session(&c, "t1", 1).as_deref(), Some("ses_real_1"));
        // handoff 轮写入同一 (task, attempt) → 取最新
        crate::db::session_put(&c, "t1_a1_h1", "ses_handoff_1", Some("t1"), Some("execute"), Some(1));
        assert_eq!(crate::db::exec_resume_session(&c, "t1", 1).as_deref(), Some("ses_handoff_1"));
        assert!(crate::db::exec_resume_session(&c, "t1", 9).is_none());
    }

    #[test]
    fn suspend_manual_dedups_inbox_and_records_both_reasons() {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate_for_tests(&c);
        seed(&c);
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let state = Arc::new(crate::state::App {
            db: std::sync::Mutex::new(Connection::open_in_memory().unwrap()),
            tx,
            handles: Default::default(),
            handle_seq: Default::default(),
            wake: Default::default(),
            sessions: Default::default(),
            circuits: Default::default(),
        });
        let t = crate::db::get_task(&c, "t1").unwrap();
        // 第一档跳过 + 第二档失败的记录先落
        recovered(&state, &c, &t, TIER_NATIVE, OUTCOME_SKIPPED, "supports_resume=false", None, json!({}));
        recovered(&state, &c, &t, TIER_HANDOFF, OUTCOME_FAILED, "workspace missing", None, json!({}));
        let t = crate::db::get_task(&c, "t1").unwrap();
        suspend_manual(&state, &c, &t);

        let t2 = crate::db::get_task(&c, "t1").unwrap();
        assert_eq!(t2.status, "suspended");
        let rec = crate::db::recovery_state(&c, "t1");
        assert!(rec.manual && !rec.pending, "{:?}", rec);

        let evs = crate::db::list_events(&c, None, Some("t1"));
        let manual = evs.iter().find(|e| e.kind == "task.recovered" && e.payload["tier"] == "manual").unwrap();
        assert_eq!(manual.payload["outcome"], "suspended");
        assert!(manual.payload["reason"].as_str().unwrap().contains("supports_resume=false"));
        assert!(manual.payload["reason"].as_str().unwrap().contains("workspace missing"));

        let items: Vec<_> = crate::db::list_inbox(&c)
            .into_iter()
            .filter(|i| i.payload["reason"] == INBOX_REASON)
            .collect();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].payload["tier1_reason"], "supports_resume=false");
        assert_eq!(items[0].payload["tier2_reason"], "workspace missing");

        // 再次挂起：不新增条目，只更新原因
        let t = crate::db::get_task(&c, "t1").unwrap();
        suspend_manual(&state, &c, &t);
        let items2: Vec<_> = crate::db::list_inbox(&c)
            .into_iter()
            .filter(|i| i.payload["reason"] == INBOX_REASON)
            .collect();
        assert_eq!(items2.len(), 1, "same task must not stack inbox items");
    }
}
