//! M5 项目知识库与经验沉淀：把人工指导（guidance 事件）沉淀为可复用的项目规范。
//!
//! - **条目模型**：`knowledge` 表随 SQLite 持久化（正文 / 适用范围 / 来源事件引用 / 时间），
//!   经 REST 增删改查（见 `api.rs`），全部写入 `knowledge.*` 审计事件。
//! - **提示词注入**：planner / executor / acceptor 组装提示词时按项目取可见条目
//!   （本项目 + 全局），渲染为**高优先级规范段**放在提示词开头——人工指导由此升级为团队规范。
//! - **来源可追溯**：每条条目经 `source_event_id` 回链到产生它的人工指导事件。
//! - **跨项目复用**：条目可提升为 `global`，供其他项目在提示词中引用。
//!
//! 已组装的提示词统一经 [`assemble`] 记录为 `prompt.assembled` 审计事件（保头截断），
//! 使「实际发给 agent 的提示词」可被审计与冒烟断言。

use crate::{db, events, models::KnowledgeEntry, state::App};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::Arc;

/// 注入提示词的规范段开头标记（冒烟 / 前端据此检索注入段）。
pub const SECTION_MARK: &str = "【项目规范（高优先级，必须遵守）】";

/// `prompt.assembled` 审计事件中保留的提示词最大字节数。
/// 规范段固定放在提示词**开头**，截断只保留头部 → 注入段永不被截掉。
const PROMPT_AUDIT_MAX: usize = 8_192;

/// 渲染若干条目为一段「高优先级规范」文本；空列表返回 `None`（不注入）。
pub fn render_section(entries: &[KnowledgeEntry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let mut s = String::from(SECTION_MARK);
    s.push_str("以下规范由人工指导沉淀而来，优先级高于任务描述与计划，必须逐条遵守：\n");
    for (i, k) in entries.iter().enumerate() {
        let scope = if k.scope == "global" { "全局规范" } else { "本项目规范" };
        let applies = k
            .applies_to
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(|a| format!("，适用范围：{a}"))
            .unwrap_or_default();
        s.push_str(&format!(
            "{i}) [{scope}{applies}] {body}\n　（来源事件 #{ev}，登记于 {ts}）\n",
            i = i + 1,
            scope = scope,
            applies = applies,
            body = k.body,
            ev = k.source_event_id,
            ts = crate::report::fmt_ts(k.created_at),
        ));
    }
    Some(s)
}

/// 某项目可见的规范段（本项目条目 + 全局条目）。
/// 供需要「只读预览」的调用方使用（`assemble` 内部直接用 `render_section`）。
#[allow(dead_code)] // 预留：UI 预览「本项目当前生效的规范段」
pub fn section(c: &Connection, project_id: &str) -> Option<String> {
    render_section(&db::list_knowledge(c, project_id))
}

/// 一步完成「取项目规范段 → 组装提示词 → 记录审计」。
///
/// 调用方传入组装闭包（收到规范段 `Option<&str>`，无条目时为 None），返回最终提示词；
/// 提示词随即以 `prompt.assembled` 事件落审计（role/phase/正文头部/knowledge_ids）。
/// 注意：内部自行加锁，调用方不得持有 `state.db` 锁。
pub fn assemble<F>(
    state: &Arc<App>,
    project_id: &str,
    task_id: Option<&str>,
    role: &str,
    phase: &str,
    build: F,
) -> String
where
    F: FnOnce(Option<&str>) -> String,
{
    let (sec, ids) = {
        let c = state.db.lock().unwrap();
        let entries = db::list_knowledge(&c, project_id);
        let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
        (render_section(&entries), ids)
    };
    let prompt = build(sec.as_deref());
    {
        let c = state.db.lock().unwrap();
        record_prompt(state, &c, project_id, task_id, role, phase, &prompt, &ids);
    }
    prompt
}

/// 记录一次已组装的提示词（审计事件 `prompt.assembled`，保头截断）。
pub fn record_prompt(
    state: &Arc<App>,
    c: &Connection,
    project_id: &str,
    task_id: Option<&str>,
    role: &str,
    phase: &str,
    prompt: &str,
    knowledge_ids: &[String],
) {
    let (text, truncated) = head_bytes(prompt, PROMPT_AUDIT_MAX);
    events::push_event(
        state,
        c,
        project_id,
        task_id,
        "prompt.assembled",
        json!({
            "role": role,
            "phase": phase,
            "prompt": text,
            "truncated": truncated,
            "knowledge_ids": knowledge_ids,
            "knowledge_section": prompt.starts_with(SECTION_MARK),
        }),
    );
}

/// 保留头部 `max` 字节（落在字符边界上），超长时追加截断标记。
/// 与提示词审计的语义一致：规范段在头部，永远完整保留。
fn head_bytes(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    (format!("{}……[prompt head truncated]", &s[..cut]), true)
}

/// 条目审计事件的统一 payload（全字段，含来源事件引用）。
pub fn entry_payload(k: &KnowledgeEntry) -> Value {
    serde_json::to_value(k).unwrap_or_else(|_| json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, scope: &str, body: &str, applies: Option<&str>) -> KnowledgeEntry {
        KnowledgeEntry {
            id: id.into(),
            project_id: "prj1".into(),
            scope: scope.into(),
            body: body.into(),
            applies_to: applies.map(String::from),
            source_event_id: 42,
            created_at: 1_700_000_000_000,
            updated_at: 1_700_000_000_000,
        }
    }

    #[test]
    fn render_section_marks_and_bodies() {
        let s = render_section(&[entry("k1", "project", "所有 PR 必须带测试", Some("全部任务"))]).unwrap();
        assert!(s.starts_with(SECTION_MARK));
        assert!(s.contains("所有 PR 必须带测试"));
        assert!(s.contains("适用范围：全部任务"));
        assert!(s.contains("本项目规范"));
        assert!(s.contains("来源事件 #42"));
        // 全局条目标注可辨识
        let g = render_section(&[entry("k2", "global", "禁止直接 push main", None)]).unwrap();
        assert!(g.contains("全局规范"));
        assert!(g.contains("禁止直接 push main"));
    }

    #[test]
    fn render_section_empty_is_none() {
        assert!(render_section(&[]).is_none());
    }

    #[test]
    fn head_bytes_keeps_head_and_marks_truncation() {
        let (t, tr) = head_bytes("short", 100);
        assert_eq!(t, "short");
        assert!(!tr);
        // 多字节字符不越界：中文超长时保头 + 截断标记
        let long = "规".repeat(1000);
        let (t, tr) = head_bytes(&long, 100);
        assert!(tr);
        assert!(t.starts_with("规"));
        assert!(t.ends_with("[prompt head truncated]"));
        assert!(t.len() <= 100 + 40);
    }
}
