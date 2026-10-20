//! 真实适配器：驱动 OpenCode CLI（`opencode run`），stdout 逐行流入任务日志。
//!
//! - 结论协议：计划审查与验收回复必须有独占一行的 `VERDICT: pass` / `VERDICT: reject`；
//!   解析只认最后一处真实结论，排除提示词回声；同一行同时出现 pass/reject 视为格式错误。
//! - 无结论 / 无法解析 → 按基建失败处理（换人重试），不记为"退回"。
//! - `timeout_ms` 是**空闲阈值**（持续无输出才判定卡死），不是总运行时长上限。
//! - 每次调用的提示词与完整输出都会归档（见 `archive_agent_output`）。

use super::{AdapterCapabilities, ExecMode, ExecOutcome};
use crate::{db, events, models::{Agent, Project, Task}, state::App};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, BufReader};

/// M2 adapter: drives the real OpenCode CLI (`opencode run`) headlessly.
/// stdout of the CLI is streamed line-by-line into the task log table
/// (same channel as the mock adapter); exit 0 == success.
pub struct OpenCodeAdapter;

impl AdapterCapabilities for OpenCodeAdapter {
    fn name(&self) -> &'static str { "opencode" }
    fn supports_resume(&self) -> bool { true } // CLI --session implies resumable sessions
    fn supports_spawn(&self) -> bool { true }
    fn supports_send(&self) -> bool { true }
    fn supports_observe(&self) -> bool { true }
}

pub const DEFAULT_TIMEOUT_MS: u64 = 3_600_000;
pub const PLAN_TIMEOUT_MS: u64 = 3_600_000;
pub const DECIDE_TIMEOUT_MS: u64 = 600_000;

/// Every infrastructure failure of an acceptor run (spawn error, rate limit,
/// timeout, auth error...) is wrapped with this prefix so the scheduler can
/// tell it apart from a genuine `VERDICT: reject` produced by the acceptor.
pub const INFRA_ACCEPT_ERROR_PREFIX: &str = "acceptance run failed: ";

pub fn timeout_of(agent: &Agent) -> u64 {
    agent
        .config
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_TIMEOUT_MS)
}

pub fn model_of(agent: &Agent) -> Option<String> {
    agent
        .config
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

// ---------- session ids (agent.session_started/ended events carry these) ----------

pub fn exec_session(task_id: &str, attempt: i64) -> String {
    format!("{task_id}_a{attempt}")
}

/// M4 三档恢复·第二档：交接重建会话名（同一 attempt 的第 n 次交接）。
/// 全新逻辑会话 → 不带历史真实 id → CLI 新开会话，真实 id 必然 ≠ 原会话 id。
pub fn handoff_session(task_id: &str, attempt: i64, n: i64) -> String {
    format!("{task_id}_a{attempt}_h{n}")
}

/// M4 计划阶段会话：执行者制定任务计划。
pub fn task_plan_session(task_id: &str, attempt: i64) -> String {
    format!("tsk_{task_id}_plan_a{attempt}")
}

/// M4 计划阶段会话：验收者审查任务计划。
pub fn plan_review_session(task_id: &str, attempt: i64) -> String {
    format!("tsk_{task_id}_planrev_a{attempt}")
}

pub fn accept_session(task_id: &str, attempt: i64) -> String {
    format!("{task_id}_a{attempt}_accept")
}

/// Planner session id is fixed per project and reused across planning runs.
pub fn plan_session(project_id: &str) -> String {
    format!("planner-{project_id}")
}

/// Decision session id, one per decision request.
pub fn decide_session(decision_id: &str) -> String {
    format!("decide-{decision_id}")
}

/// M3 报告会话 id，每项目固定复用（报告可重试，保留上下文）。
pub fn report_session(project_id: &str) -> String {
    format!("report-{project_id}")
}

/// M3 协商会话 id（每任务固定复用，跨轮次保留上下文）：executor 提案 / acceptor 审核。
pub fn negotiate_exec_session(task_id: &str) -> String {
    format!("{task_id}_negotiate_exec")
}

pub fn negotiate_acc_session(task_id: &str) -> String {
    format!("{task_id}_negotiate_acc")
}

// ---------- prompts ----------

/// M5 知识库：把项目规范段放到提示词**开头**（高优先级，人工指导升级为团队规范）。
/// 规范段在头部，也保证后续任何「保头截断」都不会截掉它。
fn push_knowledge_section(s: &mut String, knowledge: Option<&str>) {
    if let Some(k) = knowledge.filter(|k| !k.trim().is_empty()) {
        s.push_str(k);
        s.push_str("\n\n");
    }
}

/// M4 计划阶段：执行者制定任务计划（属规划性质 → 携带项目目标）。
/// 审查打回时携带上一版计划与验收者意见，要求修订。
pub fn task_plan_prompt(
    project: Option<&Project>,
    task: &Task,
    guidance: Option<&str>,
    prior_plan: Option<&str>,
    review_feedback: Option<&str>,
    knowledge: Option<&str>,
) -> String {
    let mut s = String::new();
    push_knowledge_section(&mut s, knowledge);
    s.push_str("你是项目的执行者 agent（executor）。任务已分配给你，但**先不要动手**：请先制定任务执行计划，交由验收者审查通过后再执行。\n\n");
    if let Some(p) = project {
        s.push_str(&format!("项目名称：{}\n项目目标：{}\n\n", p.name, p.brief));
    }
    s.push_str(&format!("任务标题：{}\n任务描述：\n{}\n\n", task.title, task.description));
    s.push_str(&format!(
        "验收标准（计划必须覆盖其全部要求）：{}\n",
        task.acceptance_criteria.as_deref().unwrap_or("(未指定)")
    ));
    if let Some(g) = guidance.filter(|g| !g.trim().is_empty()) {
        s.push_str(&format!("\n最新人工指导（必须遵守）：{g}\n"));
    }
    if let Some(prev) = prior_plan.filter(|p| !p.trim().is_empty()) {
        s.push_str(&format!("\n上一版计划（已被验收者打回）：\n{prev}\n"));
    }
    if let Some(fb) = review_feedback.filter(|f| !f.trim().is_empty()) {
        s.push_str(&format!("\n验收者的打回意见（必须针对性修订）：{fb}\n"));
    }
    s.push_str("\n输出要求：直接输出计划正文（Markdown），不要输出 JSON。计划需包含：\n");
    s.push_str("1) 目标拆解与步骤顺序；2) 每步将创建/修改的文件或产物（相对当前工作目录）；3) 如何验证每步正确（命令/检查方式）；4) 风险与回退方案。\n");
    s.push_str("保持精炼（200 行以内）。输出计划即可，本轮不要执行任何实际改动。\n");
    s
}

/// M4 计划阶段：验收者审查执行者的任务计划。
pub fn plan_review_prompt(project: &Project, task: &Task, plan: &str, knowledge: Option<&str>) -> String {
    let mut s = String::new();
    push_knowledge_section(&mut s, knowledge);
    s.push_str("你是项目的验收者 agent（acceptor）。执行者提交了任务计划，请审查该计划是否足以达成任务目标与验收标准。\n\n");
    s.push_str(&format!("项目名称：{}\n项目目标：{}\n\n", project.name, project.brief));
    s.push_str(&format!("任务标题：{}\n任务描述：\n{}\n\n", task.title, task.description));
    s.push_str(&format!(
        "验收标准：{}\n\n执行者的任务计划：\n{plan}\n\n",
        task.acceptance_criteria.as_deref().unwrap_or("(未指定)")
    ));
    s.push_str("审查要点：步骤是否覆盖验收标准全部要求；产物/文件是否明确；验证方式是否可独立复核；是否有明显风险遗漏或范围偏差。\n");
    s.push_str("输出要求：回复中必须包含明确的审查结论行：`VERDICT: pass` 或 `VERDICT: reject`（大小写不限）。\n");
    s.push_str("若结论为 reject，请在结论行的下一行写明需要修订的具体意见（执行者将据此重写计划）。\n");
    s
}

pub fn exec_prompt(project: Option<&Project>, task: &Task, guidance: Option<&str>, knowledge: Option<&str>) -> String {
    let mut s = String::new();
    push_knowledge_section(&mut s, knowledge);
    s.push_str("你是项目的执行者 agent（executor）。请直接完成下面这个任务：直接动手做，不要向用户提问或请求确认。\n\n");
    if let Some(p) = project {
        s.push_str(&format!("项目名称：{}\n\n", p.name));
    }
    s.push_str(&format!("任务标题：{}\n任务描述：\n{}\n\n", task.title, task.description));
    s.push_str(&format!(
        "验收标准：{}\n",
        task.acceptance_criteria.as_deref().unwrap_or("(未指定)")
    ));
    if let Some(plan) = task.execution_plan.as_deref().filter(|p| !p.trim().is_empty()) {
        s.push_str(&format!("\n你已制定并经验收者审查通过的任务计划（按其执行）：\n{plan}\n"));
    }
    if let Some(g) = guidance.filter(|g| !g.trim().is_empty()) {
        s.push_str(&format!("\n最新人工指导（必须遵守）：{g}\n"));
    }
    s.push_str("\n当前工作目录就是项目工作区，你可以直接读写其中的文件。\n");
    s.push_str("完成任务的全部工作后，在你的最终回复末尾单独输出一行 TASK_DONE，并附上简要的完成说明（做了什么、产出了什么）。\n");
    s.push_str(replan_hint());
    s
}

/// M4：告知 agent 可用 REPLAN 块提出规划修改（由决策者裁决）。
fn replan_hint() -> &'static str {
    "\n如果你在执行中发现现有规划需要调整（新增/合并/删除/修改任务或依赖），\n     请在回复末尾**单独一行**输出：REPLAN: {\"reason\":\"简短理由\",\"changes\":[{\"op\":\"add\",\"task\":{...}}|{\"op\":\"update\",\"task_id\":\"...\",\"fields\":{...}}|{\"op\":\"cancel\",\"task_id\":\"...\"}]}\n     该建议将提交决策者裁决（批准后应用）；确无必要请不要输出。\n"
}

/// M4 三档恢复·第二档：原执行提示词 + 结构化交接书（目标/已完成/剩余/约束/产物路径）。
/// 以**新会话**续作，提示词明确「已完成部分不要重做」。
pub fn handoff_prompt(
    project: Option<&Project>,
    task: &Task,
    guidance: Option<&str>,
    doc: &crate::recovery::HandoffDoc,
    knowledge: Option<&str>,
) -> String {
    let mut s = exec_prompt(project, task, guidance, knowledge);
    s.push_str("\n【中断恢复 · 交接摘要】上一轮执行会话因服务进程重启而中断，本轮以新会话续作。\n");
    s.push_str("请先完整阅读下面的交接书：已完成部分不要重做，从「剩余」部分继续，直到任务完成。\n\n");
    s.push_str(&doc.markdown);
    s
}

pub fn accept_prompt(project: &Project, task: &Task, log_tail: &str, knowledge: Option<&str>) -> String {
    let mut s = String::new();
    push_knowledge_section(&mut s, knowledge);
    s.push_str("你是项目的验收者 agent（acceptor）。请根据【任务计划 + 验收标准】审核执行结果与交付产物。\n\n");
    s.push_str(&format!("项目名称：{}\n\n", project.name));
    s.push_str(&format!("任务标题：{}\n任务描述：\n{}\n\n", task.title, task.description));
    if let Some(plan) = task.execution_plan.as_deref().filter(|p| !p.trim().is_empty()) {
        s.push_str(&format!("任务计划（你此前审查通过，作为验收基准之一）：\n{plan}\n\n"));
    }
    s.push_str(&format!(
        "验收标准：{}\n\n执行日志（尾部摘要）：\n{log_tail}\n\n",
        task.acceptance_criteria.as_deref().unwrap_or("(未指定)")
    ));
    s.push_str("当前工作目录就是项目工作区，你可以查看其中的文件来检验执行结果。\n");
    s.push_str("输出要求：你的回复中必须包含明确的验收结论行：`VERDICT: pass` 或 `VERDICT: reject`（大小写不限）。");
    s.push_str("若结论为 reject，请在结论行的下一行写明拒收理由。\n");
    s.push_str(replan_hint());
    s
}

// ---------- M3 验收标准协商（executor ↔ acceptor 多轮）----------

/// executor 提案提示词：携带任务上下文 + 历史轮次与上轮 acceptor 意见，输出可检验标准。
pub fn negotiate_propose_prompt(
    project: Option<&Project>,
    task: &Task,
    round: i64,
    feedback: Option<&str>,
    history: &[Value],
    knowledge: Option<&str>,
) -> String {
    let mut s = String::new();
    push_knowledge_section(&mut s, knowledge);
    s.push_str("你是项目的执行者 agent（executor）。项目处于规划阶段，你需要与验收者（acceptor）就【验收标准】进行协商，直到双方一致。\n");
    s.push_str("验收标准必须具体、可检验（可对照产物/文件/行为判定通过或退回）。\n\n");
    if let Some(p) = project {
        s.push_str(&format!("项目名称：{}\n项目简介：{}\n\n", p.name, p.brief));
    }
    s.push_str(&format!("任务标题：{}\n任务描述：\n{}\n\n", task.title, task.description));
    if !history.is_empty() {
        s.push_str("历史协商轮次（较早在上）：\n");
        for h in history {
            let r = h["round"].as_i64().unwrap_or(0);
            match h["role"].as_str() {
                Some("executor") => s.push_str(&format!(
                    "- 第 {r} 轮 executor 提案：{}\n",
                    h["proposal"].as_str().unwrap_or("")
                )),
                Some("acceptor") => {
                    let agree = h["agree"].as_bool().unwrap_or(false);
                    s.push_str(&format!(
                        "- 第 {r} 轮 acceptor 结论：{}｜意见：{}\n",
                        if agree { "同意" } else { "反对" },
                        h["comment"].as_str().unwrap_or("")
                    ));
                }
                _ => {}
            }
        }
        s.push('\n');
    }
    if let Some(f) = feedback.filter(|f| !f.trim().is_empty()) {
        s.push_str(&format!("验收者上一轮的意见（本轮必须据此修订）：{f}\n\n"));
    }
    s.push_str(&format!("这是第 {round} 轮提案。\n"));
    s.push_str("输出要求：只输出【修订后的验收标准】本身（若干条可检验条目，可用换行或分号分隔），不要输出 JSON、不要输出解释或寒暄。\n");
    s
}

/// acceptor 审核提示词：判定该轮提案是否可接受；同意可给出最终标准。
pub fn negotiate_review_prompt(
    project: Option<&Project>,
    task: &Task,
    round: i64,
    proposal: &str,
    knowledge: Option<&str>,
) -> String {
    let mut s = String::new();
    push_knowledge_section(&mut s, knowledge);
    s.push_str("你是项目的验收者 agent（acceptor）。执行者已就【验收标准】给出提案，请你审核其是否具体、可检验、能覆盖任务目标。\n\n");
    if let Some(p) = project {
        s.push_str(&format!("项目名称：{}\n项目简介：{}\n\n", p.name, p.brief));
    }
    s.push_str(&format!("任务标题：{}\n任务描述：\n{}\n\n", task.title, task.description));
    s.push_str(&format!("第 {round} 轮验收标准提案：\n{proposal}\n\n"));
    s.push_str("输出要求（务必严格遵守）：只输出一个 JSON 对象，首字符 '{'，末字符 '}'，不要输出 Markdown、代码围栏或任何解释。\n");
    s.push_str("格式：{\"agree\":true|false,\"comment\":\"简短意见（不同意时给出需要补充/修订的点）\",\"criteria\":\"同意时可选：最终采用的验收标准\"}\n");
    s.push_str("判断标准：标准是否可检验、是否覆盖任务目标、是否存在歧义或不可验证项。\n");
    s
}

pub fn plan_prompt(project: &Project, agents: &[Agent], knowledge: Option<&str>) -> String {
    let mut s = String::new();
    push_knowledge_section(&mut s, knowledge);
    s.push_str(&format!(
        "你是项目「{}」的规划者 agent（planner）。请根据下面的项目信息与团队情况，把项目拆解成一组可执行、可验收的任务。\n\n项目名称：{}\n项目简介：{}\n\n当前团队（agent 池）：\n",
        project.name, project.name, project.brief
    ));
    for a in agents {
        s.push_str(&format!(
            "- {}（id: {}，适配器: {}，角色: {}）\n",
            a.name, a.id, a.adapter, a.roles.join("/")
        ));
    }
    s.push_str("\n角色分配由后端完成，你不需要也不应该在 JSON 中指定任何 agent。\n\n");
    s.push_str("输出要求（务必严格遵守）：\n");
    s.push_str("- 你输出的第一个字符必须是 '{'，最后一个字符必须是 '}'；除了这一个 JSON 对象外不得输出任何其他内容。\n");
    s.push_str("- 不要输出 Markdown、代码围栏、解释、标题或第二个 JSON 对象。\n");
    s.push_str("- 所有键名必须用双引号括起的字符串。\n");
    s.push_str("格式如下：\n");
    s.push_str("{\"tasks\":[{\"title\":\"...\",\"description\":\"...\",\"depends_on\":[0],\"acceptance_required\":true,\"acceptance_criteria\":\"...\",\"acceptance_checks\":[{\"type\":\"file_exists\",\"path\":\"relative/path\"},{\"type\":\"command\",\"cmd\":\"cargo test\",\"expect_exit\":0}]}],\"milestones\":[{\"name\":\"里程碑一 · <内容主题>\",\"goal\":\"...\",\"effect\":\"...\",\"tasks\":[0,1]}]}\n\n");
    s.push_str("约束：\n");
    s.push_str("- depends_on 引用其它任务在该数组中的下标（从 0 开始），无依赖写 []；不得成环。\n");
    s.push_str("- 每个任务都要有标题、具体的描述、以及明确可检验的验收标准；acceptance_required 保持 true。\n");
    s.push_str("- acceptance_checks 是**机器可执行**的验收证据，0~3 条，只允许三种：\n");
    s.push_str("  {\"type\":\"file_exists\",\"path\":...} / {\"type\":\"file_not_exists\",\"path\":...} / {\"type\":\"command\",\"cmd\":...,\"expect_exit\":0}。\n");
    s.push_str("- 路径必须是任务工作区内的相对路径（不得含 .. 或绝对路径）；命令必须确定性、可在 2 分钟内跑完，不要依赖网络或交互输入。\n");
    s.push_str("- 无法机器判定的要求写进 acceptance_criteria（由验收者用工具核查），不要硬凑 acceptance_checks。\n");
    s.push_str("- 任务数量适中（2~6 个），粒度应可独立完成，按依赖排好先后。\n");
    s.push_str("- milestones 里程碑规划：当任务可按交付批次分组时，用里程碑把任务分组。每个里程碑给 name（必填，命名直接以『里程碑』开头，如『里程碑一 · 会话稳定性』，不要用『阶段』等其它词）、goal/effect（可选），tasks 为该里程碑成员任务在 tasks 数组中的下标；同一个任务只放进一个里程碑；里程碑之间可以存在跨里程碑依赖（后一里程碑任务依赖前一里程碑任务）。不需要里程碑时 milestones 写 []。\n");
    s
}

/// 人工介入：把当前任务全貌 + 用户说明交给规划者，产出可执行的调整方案。
pub fn intervene_prompt(project: &Project, tasks: &[Task], note: &str, knowledge: Option<&str>) -> String {
    let mut s = String::new();
    push_knowledge_section(&mut s, knowledge);
    s.push_str(&format!(
        "你是项目「{}」的规划者 agent（planner）。用户是项目负责人，正在人工介入。请基于介入说明与当前任务现状，制定调整方案并说明理由。\n\n项目名称：{}\n项目目标：{}\n\n任务清单：\n",
        project.name, project.name, project.brief
    ));
    if tasks.is_empty() {
        s.push_str("（当前没有任何任务）\n");
    }
    for t in tasks {
        let dep_titles: Vec<String> = deps_by_titles(tasks, t);
        let criteria = if t.acceptance_required {
            t.acceptance_criteria.clone().unwrap_or_default()
        } else {
            "（已豁免验收）".into()
        };
        s.push_str(&format!(
            "- id: {} ｜ 标题: {} ｜ 状态: {} ｜ 尝试: {} ｜ 退回: {} ｜ 失败: {} ｜ 验收: {} ｜ 描述: {} ｜ 依赖: {}\n",
            t.id,
            t.title,
            t.status,
            t.attempt,
            t.rejection_count,
            t.failure_count,
            criteria,
            t.description,
            if dep_titles.is_empty() { "无".into() } else { dep_titles.join("、") }
        ));
    }
    s.push_str(&format!("\n用户的介入说明：\n{note}\n\n"));
    s.push_str("输出要求（务必严格遵守）：\n");
    s.push_str("- 你输出的第一个字符必须是 '{'，最后一个字符必须是 '}'；除这一个 JSON 对象外不得有任何其他内容、Markdown 或解释。\n");
    s.push_str("- 所有键名必须双引号。changes 数组可以为空（如果现状无需改动，在 reply 里解释原因）。\n");
    s.push_str("格式如下：\n");
    s.push_str("{\"analysis\":\"对现状与问题的分析\",\"reply\":\"给用户的回复与建议（用于探讨）\",\"changes\":[");
    s.push_str("{\"op\":\"add\",\"task\":{\"title\":\"...\",\"description\":\"...\",\"depends_on\":[\"<依赖任务的标题>\"],\"acceptance_required\":true,\"acceptance_criteria\":\"...\"}},");
    s.push_str("{\"op\":\"update\",\"task_id\":\"<上面清单中的 id>\",\"fields\":{\"title\":..,\"description\":..,\"depends_on\":[标题数组],\"acceptance_required\":true,\"acceptance_criteria\":..}},");
    s.push_str("{\"op\":\"cancel\",\"task_id\":\"<上面清单中的 id>\"}]}\n\n");
    s.push_str("约束：\n");
    s.push_str("- add 的 depends_on 用任务标题（可引用其他 add 的新标题）；update/cancel 的 task_id 必须来自上面清单，只能改非终态（done/cancelled 除外）的任务。\n");
    s.push_str("- 不要在 JSON 中指定任何 agent 或 id；executor/acceptor 由后端分配。\n");
    s.push_str("- 方案要克制：只改真正需要改的，避免大拆大改。\n");
    s
}

/// Titles of the tasks `t` depends on (for the human-readable task listing in
/// `intervene_prompt`); unknown ids fall back to the raw id.
fn deps_by_titles(tasks: &[Task], t: &Task) -> Vec<String> {
    t.depends_on
        .iter()
        .map(|d| {
            tasks
                .iter()
                .find(|x| &x.id == d)
                .map(|x| x.title.clone())
                .unwrap_or_else(|| d.clone())
        })
        .collect()
}

/// M3 项目总结报告：planner 依据事实快照撰写「报告概述 + 里程碑汇报段落」。
/// 输出为 Markdown 叙述文本（无代码围栏），由后端嵌入报告第一节。
pub fn report_prompt(project: &Project, facts: &Value) -> String {
    let mut s = format!(
        "你是项目「{}」的规划者 agent（planner）。项目已进入 COMPLETED，请撰写项目总结报告的叙述部分。\n\n项目名称：{}\n项目目标：{}\n\n以下是从系统事件溯源中汇总出的客观事实快照（JSON），请只依据其中的事实叙述，不要编造：\n{}\n\n输出要求：\n- 直接输出 Markdown 正文，不要输出代码围栏、不要输出 JSON、不要复述整张事实表。\n- 第一段写「报告概述」：项目最终交付了什么、整体完成度与质量。\n- 随后按里程碑撰写「里程碑汇报」段落：逐个说明该里程碑目标（goal）、gate 结果（通过/退回）与影响。\n- 最后用一小段总结风险与后续建议（可留待改进项）。\n- 语言精炼、面向项目负责人。\n",
        project.name,
        project.name,
        project.brief,
        serde_json::to_string(facts).unwrap_or_else(|_| "{}".into()),
    );
    s.push_str("\n注意：以上事实快照已由后端保证完整；你只负责叙述，不要再输出统计表格。\n");
    s
}

/// M3 报告：驱动 opencode planner 产出叙述；CLI 失败/空输出即视为报告生成失败
/// （记录 domo.log，保留 failed 状态供手工重试）。
pub async fn report_narrative(
    state: Arc<App>,
    project: Project,
    facts: Value,
    agent: Agent,
) -> Result<String, String> {
    let prompt = report_prompt(&project, &facts);
    let session = report_session(&project.id);
    let out = run_cli(
        &state,
        None,
        None,
        &session,
        model_of(&agent).as_deref(),
        PLAN_TIMEOUT_MS,
        &prompt,
    )
    .await
    .map_err(|e| format!("报告生成失败（planner '{}'）：{e}", agent.name))?;
    let text = out.text.trim().to_string();
    if text.is_empty() {
        return Err(format!("报告生成失败（planner '{}' 输出为空）", agent.name));
    }
    Ok(text)
}

pub fn decide_prompt(
    project: Option<&Project>,
    task: Option<&Task>,
    decision: &crate::models::Decision,
) -> String {
    let mut s = String::new();
    s.push_str("你是项目的决策者 agent（decision_maker）。请根据决策请求作出客观裁定，不要随意升级到人。\n\n");
    if let Some(p) = project {
        s.push_str(&format!("项目名称：{}\n\n", p.name));
    }
    if let Some(t) = task {
        s.push_str(&format!(
            "关联任务：标题={} 状态={} 尝试={} 退回={} 失败={}\n描述：{}\n验收标准：{}\n\n",
            t.title,
            t.status,
            t.attempt,
            t.rejection_count,
            t.failure_count,
            t.description,
            t.acceptance_criteria.as_deref().unwrap_or("(未指定)")
        ));
    }
    s.push_str(&format!("决策类型：{}（scope={}）\n", decision.kind, decision.scope));
    s.push_str(&format!("决策请求：{}\n\n", serde_json::to_string_pretty(&decision.request).unwrap_or_default()));
    s.push_str("权限分层（必须遵守）：\n");
    s.push_str("- 战术事项由你自决并留痕：豁免（免验收）裁定、验收标准协商失败裁决、不触及已确认里程碑/项目目标的任务级变更。\n");
    s.push_str("- 战略事项一律转人工（系统已直接把它们放进决策收件箱，不会派给你）：触及已确认里程碑/项目目标的变更、新增角色或启用新 agent、卡死升级（执行失败 ≥3 次 / 验收退回 ≥2 轮）。\n");
    s.push_str("- 若请求实际触及上述战略事项，不要自批：给 approve=false，并在 rationale 里写明「应转人工」及原因。\n");
    s.push_str("输出要求：\n");
    s.push_str("- 只输出一个 JSON 对象，首字符 '{'，末字符 '}'。\n");
    s.push_str("- 格式：{\"approve\":true|false,\"rationale\":\"简短理由\",\"criteria\":\"可选：协商失败时可给出建议的验收标准\"}\n");
    s.push_str("- 建议按最小改动原则裁定；不触及里程碑/项目目标/新增 agent 的任务级变更优先 approve。\n");
    if decision.kind == "replan" {
        s.push_str("- 这是执行中上报的「规划问题」重规划请求：仅当问题确实成立、且调整不改变项目目标时才 approve；\n");
        s.push_str("  批准后系统会复用规划会话重新产出任务调整（新增任务默认带验收、插入 DAG 前做环校验）。\n");
    }
    s
}

// ---------- workspace resolution ----------
/// M4：任务 cwd = 任务沙盒（永远在 DOMO 管辖的项目工作区内）——
/// 代码任务为 worktree（`<prj>/wt/<tsk>/<repo>/`），非代码任务为独立子目录
/// （`<prj>/tasks/<tsk>/`）；源仓库仅作为 worktree 的源，不被直接修改。
fn task_cwd(state: &Arc<App>, task: &Task) -> Result<PathBuf, String> {
    let (project, source) = {
        let c = state.db.lock().unwrap();
        let Some(proj) = db::get_project(&c, &task.project_id) else {
            return Err("project not found".into());
        };
        let src = db::project_workdir(&c, &proj);
        (proj, src)
    };
    let Some(source) = source else {
        events::log_line_state(state, &task.id, "未设置工作区或路径无效（project has no workspace）; task attempt failed");
        return Err("project has no workspace; set a workspace before dispatching tasks".into());
    };
    if !source.is_dir() {
        events::log_line_state(state, &task.id, &format!("未设置工作区或路径无效（workspace path does not exist: {}）; task attempt failed", source.display()));
        return Err(format!("workspace path does not exist: {}", source.display()));
    }
    let sandbox = crate::sandbox::prepare(state, &project, task);
    match sandbox {
        Ok(sb) => Ok(sb.cwd),
        Err(e) => {
            events::log_line_state(state, &task.id, &format!("任务沙盒准备失败（sandbox prepare failed: {e}）"));
            Err(e)
        }
    }
}

// ---------- CLI execution ----------

/// Result of one CLI run.
pub struct RunOutcome {
    /// raw stdout (JSON stream when using --format json)
    #[allow(dead_code)]
    pub raw: String,
    /// assistant text extracted from the stream (what a human would read)
    pub text: String,
    /// real opencode session id if discovered in the stream
    pub session_id: Option<String>,
}

/// Whether this CLI build accepts arbitrary `--session <custom-id>` values.
/// The free tier refuses them ("only usable from within OpenCode"), so we
/// remember the answer in-process and fall back to server-assigned sessions.
static CUSTOM_SESSION_OK: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0); // 0 unknown, 1 yes, 2 no

/// M4：执行派发的真实会话 id 持久化上下文。CLI 流里一解析到 `sessionID`
/// 就落 SQLite `session_map`（不再只存 `state.sessions` 内存），kill -9 重启后
/// 三档恢复第一档据此用**同一个真实 id** 原生 resume。
#[derive(Debug, Clone)]
pub struct PersistCtx {
    /// 逻辑会话名（如 `tsk_x_a1`）
    pub logical: String,
    pub task_id: Option<String>,
    pub phase: Option<String>,
    pub attempt: Option<i64>,
}

/// Spawn `opencode run [--session <s>] [--model m] --auto --format json
/// "<prompt>"` and stream stdout line-by-line into the task log (when
/// `log_task` is set). Returns the run outcome (assistant text + session id).
pub async fn run_cli(
    state: &Arc<App>,
    log_task: Option<&str>,
    cwd: Option<&Path>,
    session: &str,
    model: Option<&str>,
    timeout_ms: u64,
    prompt: &str,
) -> Result<RunOutcome, String> {
    run_cli_ext(state, log_task, cwd, session, model, timeout_ms, prompt, None, None).await
}

/// `run_cli` 的扩展（M4 三档恢复）：`resume_session` 显式指定要续接的真实会话 id
/// （第一档原生 resume，失败即视为该档失败）；`persist` 声明本次执行的真实 id
/// 持久化上下文（执行派发专用，落 SQLite 供重启后恢复）。
pub async fn run_cli_ext(
    state: &Arc<App>,
    log_task: Option<&str>,
    cwd: Option<&Path>,
    session: &str,
    model: Option<&str>,
    timeout_ms: u64,
    prompt: &str,
    resume_session: Option<String>,
    persist: Option<PersistCtx>,
) -> Result<RunOutcome, String> {
    fn free_tier_like(e: &str) -> bool {
        e.contains("within OpenCode") || e.contains("Expected a string starting with")
    }    let mut try_custom: Vec<bool> = Vec::new();
    if CUSTOM_SESSION_OK.load(std::sync::atomic::Ordering::Relaxed) != 2 {
        try_custom.push(true);
    }
    try_custom.push(false);
    let mut last_err = String::new();
    for custom in try_custom {
        match run_cli_once(state, log_task, cwd, session, model, timeout_ms, prompt, custom, &resume_session, &persist).await {
            Ok(o) => {
                CUSTOM_SESSION_OK.store(if custom { 1 } else { 2 }, std::sync::atomic::Ordering::Relaxed);
                return Ok(o);
            }
            Err(e) => {
                if custom && free_tier_like(&e) {
                    CUSTOM_SESSION_OK.store(2, std::sync::atomic::Ordering::Relaxed);
                    if let Some(t) = log_task {
                        events::log_line_state(state, t, "custom --session ids rejected by this opencode build; falling back to server-assigned sessions");
                    }
                    last_err = e;
                    continue;
                }
                return Err(e);
            }
        }
    }
    Err(last_err)
}

async fn run_cli_once(
    state: &Arc<App>,
    log_task: Option<&str>,
    cwd: Option<&Path>,
    session: &str,
    model: Option<&str>,
    timeout_ms: u64,
    prompt: &str,
    custom_session: bool,
    resume_session: &Option<String>,
    persist: &Option<PersistCtx>,
) -> Result<RunOutcome, String> {
    // 优先级：显式 resume id（三档恢复第一档）> 进程内记忆 > 自定义/新建会话。
    let stored = resume_session
        .clone()
        .or_else(|| state.sessions.lock().unwrap().get(session).cloned());
    // A stored real id wins: continue that session (memory across runs).
    let (session_arg, session_label): (Option<String>, String) = match &stored {
        Some(real) => (Some(real.clone()), real.clone()),
        None if custom_session => (Some(session.to_string()), session.to_string()),
        None => (None, "<new>".into()),
    };
    if let Some(t) = log_task {
        events::log_line_state(
            state,
            t,
            &format!(
                "$ opencode run{}{} --auto --format json \"<prompt>\" (session {})",
                session_arg
                    .as_ref()
                    .map(|s| format!(" --session {s}"))
                    .unwrap_or_default(),
                model.as_ref().map(|m| format!(" --model {m}")).unwrap_or_default(),
                session_label,
            ),
        );
    }
    let prompt_owned = cap_prompt_arg(prompt);
    let mut cmd = tokio::process::Command::new("opencode");
    // --standalone: private per-run server, so `cwd` is honored as the
    // project workdir (the shared background service pins its own directory).
    cmd.args(["run", "--standalone", "--auto", "--format", "json"]);
    if let Some(s) = &session_arg {
        cmd.args(["--session", s]);
    }
    if let Some(m) = model {
        cmd.args(["--model", m]);
    }
    cmd.arg(prompt_owned);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
        cmd.env("PWD", dir);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.stdin(Stdio::null());
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn opencode CLI: {e}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let raw = Arc::new(Mutex::new(String::new()));
    let text = Arc::new(Mutex::new(String::new()));
    let sid = Arc::new(Mutex::new(None::<String>));
    let errb = Arc::new(Mutex::new(String::new()));
    let emsg = Arc::new(Mutex::new(None::<String>));
    // M5：不再限制总时长，改为**空闲看门狗**——按输出间隔判卡死（阈值复用配置的超时）
    let last_activity = Arc::new(Mutex::new(std::time::Instant::now()));

    tracing::info!(target: "opencode", session = %session, custom_session, model = model.unwrap_or("<default>"), timeout_ms, "opencode run spawn");

    let r_out = tokio::spawn(stream_json(stdio_ok(stdout), state.clone(), log_task.map(String::from), raw.clone(), text.clone(), sid.clone(), emsg.clone(), persist.clone(), last_activity.clone()));
    let r_err = tokio::spawn(stream_stderr(stdio_ok(stderr), state.clone(), log_task.map(String::from), errb.clone(), last_activity.clone()));

    // 空闲看门狗：只要 agent 持续产出（stdout/stderr 有行）就继续跑，不做总时长限制；
    // 连续 idle_ms 无任何输出 → 判定卡死，终止进程（错误保留 timed out 字样，
    // 由错误分类归入 Timeout → 换下一优先级 agent 重试）。
    let idle_ms = timeout_ms;
    let killed_idle = Arc::new(Mutex::new(false));
    let child_pid = child.id();
    let watchdog = {
        let last = last_activity.clone();
        let killed = killed_idle.clone();
        let session_name = session.to_string();
        tokio::spawn(async move {
            let tick = Duration::from_millis(1000.min(idle_ms.max(1)));
            loop {
                tokio::time::sleep(tick).await;
                let idle = last.lock().unwrap().elapsed().as_millis() as u64;
                if idle >= idle_ms {
                    *killed.lock().unwrap() = true;
                    tracing::warn!(target: "opencode", session = %session_name, idle_ms,
                        "opencode run idle for {idle_ms}ms (no output) — treating as stuck, terminating");
                    if let Some(pid) = child_pid {
                        // 先 TERM 给一次收尾机会，3 秒后仍在则 KILL
                        let _ = std::process::Command::new("kill").args(["-TERM", &pid.to_string()]).status();
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        let _ = std::process::Command::new("kill").args(["-KILL", &pid.to_string()]).status();
                    }
                    break;
                }
            }
        })
    };

    let status = match child.wait().await {
        Ok(st) => st,
        Err(e) => {
            watchdog.abort();
            let _ = child.kill().await;
            r_err.abort();
            r_out.abort();
            return Err(format!("waiting on opencode failed: {e}"));
        }
    };
    watchdog.abort();
    if *killed_idle.lock().unwrap() {
        let _ = child.kill().await;
        r_err.abort();
        r_out.abort();
        return Err(format!(
            "opencode run timed out after {idle_ms}ms idle (no output; session {session}, killed)"
        ));
    }
    // pipes are closed; let the readers finish draining, then harvest the text
    let _ = r_out.await;
    let _ = r_err.await;
    let out = RunOutcome {
        raw: raw.lock().unwrap().clone(),
        text: text.lock().unwrap().clone(),
        session_id: sid.lock().unwrap().clone(),
    };
    tracing::info!(target: "opencode", session = %session, exit = %status, text_len = out.text.len(), ok = status.success(), "opencode run finished");

    if status.success() && emsg.lock().unwrap().is_none() {
        // remember the real session id for this logical session (audit + reuse)
        if let Some(real) = &out.session_id {
            state.sessions
                .lock()
                .unwrap()
                .insert(session.to_string(), real.clone());
        }
        archive_agent_output(state, session, &out, persist.as_ref(), "success");
        Ok(out)
    } else {
        let cli_err = emsg.lock().unwrap().clone().unwrap_or_default();
        let detail = format!(
            "opencode run failed (exit {status}); {cli_err}; stderr tail: {}",
            tail_chars(&errb.lock().unwrap(), 600)
        );
        tracing::warn!(target: "opencode", session = %session, "{detail}");
        // 失败运行同样归档（含原始流），便于事后复核结论/排障
        archive_agent_output(state, session, &out, persist.as_ref(), "failed");
        Err(detail)
    }
}

/// Last `max_chars` characters of `s` (char-boundary safe), for error tails.
fn tail_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().rev().take(max_chars).collect::<Vec<_>>().into_iter().rev().collect()
}

// ---------- prompt size guards ----------
//
// The whole prompt is handed to `opencode run` as a SINGLE argv element. On
// Linux one argument may be at most MAX_ARG_STRLEN (32 pages = 128 KiB); going
// over that makes execve fail with E2BIG (`Argument list too long`), which for
// acceptor runs used to be misread as an acceptance verdict ("reject"). Prompts
// built from execution logs can grow far beyond that, so every launch is
// capped here and the log tail feeding acceptance prompts is capped upstream.

/// Hard cap for the prompt argv element (bytes), far below MAX_ARG_STRLEN.
const PROMPT_ARG_MAX: usize = 96 * 1024;

/// Byte-safe prefix slice: never splits a UTF-8 char, output stays valid UTF-8.
fn head_bytes(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Byte-safe suffix slice: never splits a UTF-8 char, output stays valid UTF-8.
fn tail_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Cap a single line for prompt embedding (byte-safe truncation).
fn cut_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    format!("{}…", head_bytes(s, max.saturating_sub(3)))
}

/// M5：把一次 agent 运行的完整输出写入归档表（模型输出已由 DOMO 获得 → 自行存档）。
/// 审计可回放原始输出；验收结论亦据此复核（不要求模型做任何额外动作）。
fn archive_agent_output(
    state: &Arc<App>,
    logical_session: &str,
    out: &RunOutcome,
    persist: Option<&PersistCtx>,
    outcome: &str,
) {
    const TEXT_MAX: usize = 200_000; // 助手文本上限
    const RAW_MAX: usize = 400_000; // 原始 stdout 流上限
    let text = cut_bytes(&out.text, TEXT_MAX);
    let raw = cut_bytes(&out.raw, RAW_MAX);
    let c = state.db.lock().unwrap();
    let project_id = persist
        .and_then(|p| p.task_id.as_deref())
        .and_then(|tid| db::get_task(&c, tid))
        .map(|t| t.project_id)
        .unwrap_or_else(|| "-".into());
    db::insert_agent_output(
        &c,
        &db::AgentOutputRecord {
            task_id: persist.and_then(|p| p.task_id.as_deref()),
            project_id: &project_id,
            phase: persist.and_then(|p| p.phase.as_deref()).unwrap_or(""),
            attempt: persist.and_then(|p| p.attempt).unwrap_or(0),
            session: logical_session,
            real_session_id: out.session_id.as_deref(),
            outcome,
            text: &text,
            raw: &raw,
        },
    );
}

/// Belt-and-braces guard before spawn: if the prompt still exceeds the argv
/// budget, drop the bulky middle (task description / log bulk) and keep the
/// head (role instructions) and the tail (output requirements).
fn cap_prompt_arg(prompt: &str) -> String {
    if prompt.len() <= PROMPT_ARG_MAX {
        return prompt.to_string();
    }
    const NOTE: &str = "\n\n…[prompt middle was truncated: single-argument length cap]…\n\n";
    let keep = PROMPT_ARG_MAX - NOTE.len();
    let head = head_bytes(prompt, keep / 3);
    let back = tail_bytes(prompt, keep - head.len());
    format!("{head}{NOTE}{back}")
}

/// Shape the execution-log tail embedded in acceptance prompts: cap each line,
/// then keep at most `max_lines` most-recent lines within `total_max` bytes;
/// when lines were dropped from the front, mark the elision.
fn shape_log_tail(lines: Vec<String>, max_lines: usize, total_max: usize, total_len: usize) -> String {
    let kept: Vec<&String> = {
        let mut budget = 0usize;
        let window_from = lines.len().saturating_sub(max_lines);
        let mut v = Vec::new();
        for (i, l) in lines.iter().enumerate().rev() {
            if i < window_from {
                break;
            }
            budget += l.len() + 1;
            if !v.is_empty() && budget > total_max {
                break;
            }
            v.push(l);
        }
        v.reverse();
        v
    };
    let mut out = String::new();
    if kept.len() < total_len {
        out.push_str(&format!("…（更早的 {} 行日志已省略，仅保留最近 {} 行）…\n", total_len - kept.len(), kept.len()));
    }
    out.push_str(&kept.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"));
    out
}

/// Type helper for spawned readers (ChildStdout/stderr is Send+Unpin).
fn stdio_ok(s: impl tokio::io::AsyncRead + Unpin + Send + 'static) -> impl tokio::io::AsyncRead + Unpin + Send + 'static {
    s
}

/// Read the stdout JSON stream line-by-line: append raw text, extract the
/// session id and assistant text, and (for tasks) write readable lines into
/// the logs table, which also broadcasts them on SSE (`log`).
async fn stream_json(
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    state: Arc<App>,
    task_id: Option<String>,
    raw: Arc<Mutex<String>>,
    text: Arc<Mutex<String>>,
    sid: Arc<Mutex<Option<String>>>,
    emsg: Arc<Mutex<Option<String>>>,
    persist: Option<PersistCtx>,
    last_activity: Arc<Mutex<std::time::Instant>>,
) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) | Err(_) => break,
        };
        *last_activity.lock().unwrap() = std::time::Instant::now();
        {
            let mut b = raw.lock().unwrap();
            b.push_str(&line);
            b.push('\n');
        }
        let parsed: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                if let Some(tid) = &task_id {
                    let c = state.db.lock().unwrap();
                    events::log_line(&state, &c, tid, &line);
                }
                continue;
            }
        };
        if let Some(s) = parsed["sessionID"].as_str().filter(|s| !s.is_empty()) {
            let changed = {
                let mut guard = sid.lock().unwrap();
                if guard.as_deref() == Some(s) {
                    false
                } else {
                    *guard = Some(s.to_string());
                    true
                }
            };
            if changed {
                if let Some(p) = &persist {
                    // M4：执行派发解析到的真实会话 id 立即落 SQLite（不再只存内存），
                    // kill -9 重启后三档恢复第一档据此原生 resume。
                    {
                        let c = state.db.lock().unwrap();
                        db::session_put(&c, &p.logical, s, p.task_id.as_deref(), p.phase.as_deref(), p.attempt);
                    }
                    state.sessions.lock().unwrap().insert(p.logical.clone(), s.to_string());
                }
                if let Some(tid) = &task_id {
                    let c = state.db.lock().unwrap();
                    events::log_line(&state, &c, tid, &format!("opencode session: {s}"));
                }
            }
        }
        if parsed["type"] == "error" {
            let msg = parsed["error"]["message"].as_str().unwrap_or("").to_string();
            let shown = if msg.is_empty() { line.clone() } else { msg };
            let mut guard = emsg.lock().unwrap();
            if guard.is_none() {
                *guard = Some(shown.clone());
            }
            if let Some(tid) = &task_id {
                let c = state.db.lock().unwrap();
                events::log_line(&state, &c, tid, &format!("[opencode error] {shown}"));
            }
            continue;
        }
        let readable = match parsed["type"].as_str() {
            Some("text") => parsed["part"]["text"].as_str().map(|s| s.to_string()),
            Some("tool_use") => {
                let tool = parsed["part"]["tool"].as_str().unwrap_or("?").to_string();
                let summary = parsed["part"]["state"]["output"]
                    .as_str()
                    .or_else(|| parsed["part"]["state"]["title"].as_str())
                    .unwrap_or("")
                    .to_string();
                Some(format!("[tool] {tool}: {summary}"))
            }
            // step_start / step_finish / others: skip (noise)
            _ => None,
        };
        if let Some(r) = readable {
            if r.trim().is_empty() {
                continue;
            }
            let mut b = text.lock().unwrap();
            b.push_str(&r);
            b.push('\n');
            if let Some(tid) = &task_id {
                let c = state.db.lock().unwrap();
                events::log_line(&state, &c, tid, &r);
            }
        }
    }
}

/// Plain stderr reader: tagged lines into the log + buffer for error tails.
async fn stream_stderr(
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    state: Arc<App>,
    task_id: Option<String>,
    buf: Arc<Mutex<String>>,
    last_activity: Arc<Mutex<std::time::Instant>>,
) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) | Err(_) => break,
        };
        *last_activity.lock().unwrap() = std::time::Instant::now();
        if let Ok(mut b) = buf.lock() {
            b.push_str(&line);
            b.push('\n');
        }
        if let Some(tid) = &task_id {
            let c = state.db.lock().unwrap();
            events::log_line(&state, &c, tid, &format!("[stderr] {line}"));
        }
    }
}

pub async fn run_adjudication(
    state: Arc<App>,
    task: Option<Task>,
    decision: crate::models::Decision,
    agent: Agent,
) -> Result<crate::decision::Ruling, String> {
    {
        let c = state.db.lock().unwrap();
        let log_tid = decision.task_id.as_deref().unwrap_or("-");
        events::log_line(
            &state,
            &c,
            log_tid,
            &format!(
                "opencode 决策者 '{}' 开始裁定决策请求 {}（kind={}，session {}）",
                agent.name,
                decision.id,
                decision.kind,
                decide_session(&decision.id)
            ),
        );
    }
    let cwd = match task {
        Some(ref t) => task_cwd(&state, t).ok(),
        None => None,
    };
    let project = {
        let c = state.db.lock().unwrap();
        db::get_project(&c, &decision.project_id)
    };
    let prompt = decide_prompt(project.as_ref(), task.as_ref(), &decision);
    let session = decide_session(&decision.id);
    let log_tid = decision.task_id.as_deref();
    let out = run_cli(
        &state,
        log_tid,
        cwd.as_deref(),
        &session,
        model_of(&agent).as_deref(),
        DECIDE_TIMEOUT_MS,
        &prompt,
    )
    .await?;
    let v = crate::api::extract_json_value(&out.text)
        .ok_or_else(|| "决策者输出中没有可解析的 JSON 对象".to_string())?;
    let approve = v["approve"].as_bool().unwrap_or(false);
    let rationale = v["rationale"].as_str().unwrap_or("").trim().to_string();
    if rationale.is_empty() {
        return Err("决策者未提供 rationale".into());
    }
    let criteria = v["criteria"].as_str().filter(|s| !s.is_empty()).map(String::from);
    // M4 合并冲突裁定：可选机械动作（rebase | rework | manual）
    let parsed_action = v["effect"]["action"]
        .as_str()
        .or_else(|| v["action"].as_str())
        .filter(|a| matches!(*a, "rebase" | "rework" | "manual"))
        .map(String::from);
    {
        let c = state.db.lock().unwrap();
        let log_tid = decision.task_id.as_deref().unwrap_or("-");
        events::log_line(
            &state,
            &c,
            log_tid,
            &format!("决策者裁定：approve={} rationale={rationale}", approve),
        );
    }
    Ok(crate::decision::Ruling { approve, rationale, criteria, action: parsed_action })
}

// ---------- task entry points (mirror mock::run_execution / run_acceptance) ----------

/// Execute one attempt with the real CLI. Workspace is mandatory (DESIGN §13):
/// a missing/invalid workspace fails the attempt immediately.
///
/// M4 三档恢复：`mode` 决定会话形态——`Fresh`=新会话（attempt+1 由调度器负责）；
/// `Resume`=用同一真实 id 原生续作（attempt 不自增、逻辑会话名不变）；`Handoff`=新会话
/// + 交接书提示词（真实 id 必然 ≠ 原 id）。执行派发解析到的真实会话 id 立即落 SQLite
/// （`persist`），kill -9 重启后据此恢复。
pub async fn run_execution(
    state: Arc<App>,
    task: Task,
    agent: Agent,
    mode: ExecMode,
) -> Result<ExecOutcome, String> {
    let logical = mode.session_name(&task.id, task.attempt);
    {
        let c = state.db.lock().unwrap();
        let how = match &mode {
            ExecMode::Fresh => String::new(),
            ExecMode::Resume { real_id } => format!("（三档恢复·原生 resume --session {real_id}）"),
            ExecMode::Handoff { session, .. } => format!("（三档恢复·交接重建，新会话 {session}）"),
        };
        events::log_line(
            &state,
            &c,
            &task.id,
            &format!(
                "opencode executor '{}' started attempt {} for task '{}' (session {}){how}",
                agent.name,
                task.attempt,
                task.title,
                logical
            ),
        );
        if let Some(g) = db::latest_guidance(&c, &task.id) {
            events::log_line(&state, &c, &task.id, &format!("carrying human guidance: {g}"));
        }
    }
    let cwd = task_cwd(&state, &task)?;

    let (project, guidance) = {
        let c = state.db.lock().unwrap();
        (db::get_project(&c, &task.project_id), db::latest_guidance(&c, &task.id))
    };
    let prompt = crate::knowledge::assemble(
        &state, &task.project_id, Some(&task.id), "executor", "execute",
        |k| match &mode {
            ExecMode::Handoff { doc, .. } => {
                events::log_line_state(
                    &state,
                    &task.id,
                    &format!("交接书已生成：{}（目标/已完成/剩余/约束/产物路径）", doc.path),
                );
                handoff_prompt(project.as_ref(), &task, guidance.as_deref(), doc, k)
            }
            _ => exec_prompt(project.as_ref(), &task, guidance.as_deref(), k),
        },
    );
    let resume = match &mode {
        ExecMode::Resume { real_id } => Some(real_id.clone()),
        _ => None,
    };

    let out = run_cli_ext(
        &state,
        Some(&task.id),
        Some(&cwd),
        &logical,
        model_of(&agent).as_deref(),
        timeout_of(&agent),
        &prompt,
        resume,
        Some(PersistCtx {
            logical: logical.clone(),
            task_id: Some(task.id.clone()),
            phase: Some("execute".into()),
            attempt: Some(task.attempt),
        }),
    )
    .await?;

    {
        let c = state.db.lock().unwrap();
        events::log_line(
            &state,
            &c,
            &task.id,
            &format!(
                "opencode execution succeeded for task '{}' (exit 0, session {})",
                task.title,
                out.session_id.as_deref().unwrap_or("(not reported)")
            ),
        );
    }
    // M4：执行输出中若含 REPLAN 块 → 登记规划修改建议（决策者裁决；失败不影响主流程）
    if let Some(changes) = crate::replan::parse_replan_block(&out.text) {
        let reason = changes.get("reason").and_then(|r| r.as_str()).unwrap_or("agent 在执行中发现规划需调整");
        let change_list = changes.get("changes").cloned().filter(|v| v.is_array()).unwrap_or(json!([]));
        if change_list.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            let c = state.db.lock().unwrap();
            crate::replan::propose_from_agent(
                &state, &c, &task, &agent.id, reason,
                "执行者 agent 在执行过程中提出的规划变更建议", change_list,
            );
            events::log_line(&state, &c, &task.id, "已按 REPLAN 块登记规划修改建议，提交决策者裁决");
        }
    }
    // output artifacts live in the workspace; no synthetic artifact path is recorded
    Ok(ExecOutcome { artifacts: vec![], session_id: out.session_id })
}

/// M4 计划阶段：执行者用真实 CLI 制定任务计划（输出计划正文，非 JSON）。
pub async fn run_task_plan(state: Arc<App>, task: Task, agent: Agent, review_feedback: Option<&str>) -> Result<String, String> {
    let session = task_plan_session(&task.id, task.attempt);
    {
        let c = state.db.lock().unwrap();
        events::log_line(
            &state, &c, &task.id,
            &format!("opencode executor '{}' 制定任务计划（session {session}）", agent.name),
        );
    }
    let cwd = task_cwd(&state, &task)?;
    let (project, guidance) = {
        let c = state.db.lock().unwrap();
        (db::get_project(&c, &task.project_id), db::latest_guidance(&c, &task.id))
    };
    let prompt = crate::knowledge::assemble(
        &state, &task.project_id, Some(&task.id), "executor", "plan_drafting",
        |k| task_plan_prompt(
            project.as_ref(),
            &task,
            guidance.as_deref(),
            task.execution_plan.as_deref(),
            review_feedback,
            k,
        ),
    );
    let out = run_cli_ext(
        &state,
        Some(&task.id),
        Some(&cwd),
        &session,
        model_of(&agent).as_deref(),
        timeout_of(&agent),
        &prompt,
        None,
        Some(PersistCtx {
            logical: session.clone(),
            task_id: Some(task.id.clone()),
            phase: Some("plan_task".into()),
            attempt: Some(task.attempt),
        }),
    )
    .await?;
    let plan = out.text.trim().to_string();
    if plan.is_empty() {
        return Err("plan output is empty".into());
    }
    {
        let c = state.db.lock().unwrap();
        events::log_line(&state, &c, &task.id, "任务计划已产出，提交验收者审查");
    }
    Ok(plan)
}

/// M4 计划阶段：验收者审查任务计划（复用 VERDICT 解析；基础设施失败带前缀）。
pub async fn run_plan_review(state: Arc<App>, task: Task, agent: Agent) -> Result<(), String> {
    let session = plan_review_session(&task.id, task.attempt);
    {
        let c = state.db.lock().unwrap();
        events::log_line(
            &state, &c, &task.id,
            &format!("opencode acceptor '{}' 审查任务计划（session {session}）", agent.name),
        );
    }
    let cwd = task_cwd(&state, &task)?;
    let prompt = {
        let c = state.db.lock().unwrap();
        let Some(p) = db::get_project(&c, &task.project_id) else {
            return Err("project not found".into());
        };
        drop(c);
        crate::knowledge::assemble(
            &state, &task.project_id, Some(&task.id), "acceptor", "plan_review",
            |k| plan_review_prompt(&p, &task, task.execution_plan.as_deref().unwrap_or("(计划缺失)"), k),
        )
    };
    let out = run_cli_ext(
        &state,
        Some(&task.id),
        Some(&cwd),
        &session,
        model_of(&agent).as_deref(),
        timeout_of(&agent),
        &prompt,
        None,
        Some(PersistCtx {
            logical: session.clone(),
            task_id: Some(task.id.clone()),
            phase: Some("plan_review".into()),
            attempt: Some(task.attempt),
        }),
    )
    .await
    .map_err(|e| format!("{INFRA_ACCEPT_ERROR_PREFIX}{e}"))?;
    parse_verdict(&out.text)
}

/// Run acceptance with the real CLI; the reply must contain a final
/// `VERDICT: pass` / `VERDICT: reject` (case-insensitive, tolerant of code
/// fences and surrounding prose). No verdict at all counts as reject.
pub async fn run_acceptance(state: Arc<App>, task: Task, agent: Agent) -> Result<(), String> {
    {
        let c = state.db.lock().unwrap();
        events::log_line(
            &state,
            &c,
            &task.id,
            &format!(
                "opencode acceptor '{}' started reviewing task '{}' (session {})",
                agent.name,
                task.title,
                accept_session(&task.id, task.attempt)
            ),
        );
    }
    let cwd = task_cwd(&state, &task)?;

    let log_tail = {
        let c = state.db.lock().unwrap();
        let logs = db::task_logs(&c, &task.id);
        // Execution logs routinely contain huge tool-output lines; embedded
        // verbatim they blew past the single-argument argv cap (E2BIG). Cap
        // every line and keep only the most recent ones within a budget.
        const LINE_MAX: usize = 2_000;
        const LINES_MAX: usize = 40;
        const TAIL_MAX: usize = 60_000;
        let total_len = logs.len();
        shape_log_tail(
            logs.iter()
                .map(|l| cut_bytes(&format!("[{}] {}", l.ts, l.line), LINE_MAX))
                .collect(),
            LINES_MAX,
            TAIL_MAX,
            total_len,
        )
    };
    let evidence = {
        let c = state.db.lock().unwrap();
        db::latest_acceptance_evidence(&c, &task.id)
    };
    let mut prompt = {
        let c = state.db.lock().unwrap();
        let project = db::get_project(&c, &task.project_id);
        drop(c);
        match project {
            Some(p) => crate::knowledge::assemble(
                &state, &task.project_id, Some(&task.id), "acceptor", "accept",
                |k| opencode_accept_prompt_inner(&p, &task, &log_tail, k),
            ),
            None => return Err("project not found".into()),
        }
    };
    if let Some(ev) = &evidence {
        prompt.push_str("\n\n## 自动机械检查结果（objective evidence）\n");
        prompt.push_str(&serde_json::to_string_pretty(ev).unwrap_or_else(|_| ev.to_string()));
        prompt.push_str(
            "\n以上结果由系统在任务工作区内自动执行，未经模型解释。验收结论必须逐项引用这些结果；\n\
             若某项失败或未执行，必须说明它是否影响任务目标的达成，不能忽略。\n",
        );
    }
    let session = accept_session(&task.id, task.attempt);

    let out = run_cli_ext(
        &state,
        Some(&task.id),
        Some(&cwd),
        &session,
        model_of(&agent).as_deref(),
        timeout_of(&agent),
        &prompt,
        None,
        Some(PersistCtx {
            logical: session.clone(),
            task_id: Some(task.id.clone()),
            phase: Some("accept".into()),
            attempt: Some(task.attempt),
        }),
    )
    .await
    .map_err(|e| format!("{INFRA_ACCEPT_ERROR_PREFIX}{e}"))?;

    // M4：验收输出中的 REPLAN 块 → 登记规划修改建议
    if let Some(changes) = crate::replan::parse_replan_block(&out.text) {
        let reason = changes.get("reason").and_then(|r| r.as_str()).unwrap_or("agent 在验收中发现规划需调整");
        let change_list = changes.get("changes").cloned().filter(|v| v.is_array()).unwrap_or(json!([]));
        if change_list.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            let c = state.db.lock().unwrap();
            crate::replan::propose_from_agent(
                &state, &c, &task, &agent.id, reason,
                "验收者 agent 在验收过程中提出的规划变更建议", change_list,
            );
            events::log_line(&state, &c, &task.id, "已按 REPLAN 块登记规划修改建议，提交决策者裁决");
        }
    }

    parse_verdict(&out.text)
}

fn opencode_accept_prompt_inner(p: &Project, t: &Task, log_tail: &str, knowledge: Option<&str>) -> String {
    accept_prompt(p, t, log_tail, knowledge)
}

/// M3 协商：executor 用真实 CLI 提出/修订验收标准。会话按任务复用（memory 连续），
/// 提示词同时显式携带历史轮次与上轮意见，输出为标准正文（非 JSON）。
pub async fn negotiate_propose(
    state: Arc<App>,
    task: Task,
    agent: Agent,
    round: i64,
    feedback: Option<&str>,
) -> Result<String, String> {
    {
        let c = state.db.lock().unwrap();
        events::log_line(
            &state, &c, &task.id,
            &format!(
                "opencode executor '{}' 提出第 {round} 轮验收标准（session {}）",
                agent.name, negotiate_exec_session(&task.id)
            ),
        );
    }
    let cwd = task_cwd(&state, &task)?;
    let (project, history) = {
        let c = state.db.lock().unwrap();
        (db::get_project(&c, &task.project_id), crate::negotiation::history(&c, &task.id))
    };
    let prompt = crate::knowledge::assemble(&state, &task.project_id, Some(&task.id), "executor", "negotiate", |k| {
        negotiate_propose_prompt(project.as_ref(), &task, round, feedback, &history, k)
    });
    let session = negotiate_exec_session(&task.id);
    let out = run_cli(
        &state,
        Some(&task.id),
        Some(&cwd),
        &session,
        model_of(&agent).as_deref(),
        timeout_of(&agent),
        &prompt,
    )
    .await
    .map_err(|e| format!("协商提案运行失败（executor '{}'）：{e}", agent.name))?;
    let text = strip_fences(out.text.trim());
    if text.is_empty() {
        return Err(format!("executor '{}' 未输出验收标准", agent.name));
    }
    Ok(cap_chars(&text, 4000))
}

/// M3 协商：acceptor 用真实 CLI 审核提案，必须输出 JSON `{agree, comment, criteria}`。
pub async fn negotiate_review(
    state: Arc<App>,
    task: Task,
    agent: Agent,
    proposal: &str,
) -> Result<crate::agents::Review, String> {
    {
        let c = state.db.lock().unwrap();
        events::log_line(
            &state, &c, &task.id,
            &format!(
                "opencode acceptor '{}' 审核提案（session {}）",
                agent.name, negotiate_acc_session(&task.id)
            ),
        );
    }
    let cwd = task_cwd(&state, &task)?;
    let (project, round) = {
        let c = state.db.lock().unwrap();
        let round = db::get_task(&c, &task.id)
            .map(|t| t.negotiation_round.max(0))
            .unwrap_or(0)
            + 1;
        (db::get_project(&c, &task.project_id), round)
    };
    let prompt = crate::knowledge::assemble(&state, &task.project_id, Some(&task.id), "acceptor", "negotiate", |k| {
        negotiate_review_prompt(project.as_ref(), &task, round, proposal, k)
    });
    let session = negotiate_acc_session(&task.id);
    let out = run_cli(
        &state,
        Some(&task.id),
        Some(&cwd),
        &session,
        model_of(&agent).as_deref(),
        timeout_of(&agent),
        &prompt,
    )
    .await
    .map_err(|e| format!("协商审核运行失败（acceptor '{}'）：{e}", agent.name))?;
    let v = crate::api::extract_json_value(&out.text)
        .ok_or_else(|| "acceptor 协商输出中没有可解析的 JSON 对象".to_string())?;
    let agree = v["agree"].as_bool().unwrap_or(false);
    let comment = v["comment"].as_str().unwrap_or("").trim().to_string();
    let criteria = v["criteria"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok(crate::agents::Review { agree, comment, criteria })
}

/// 去掉可能的 Markdown 代码围栏，保留正文。
fn strip_fences(s: &str) -> String {
    let t = s.trim();
    if !t.starts_with("```") {
        return t.to_string();
    }
    let mut lines = t.lines();
    let _ = lines.next(); // 开围栏
    let mut body: Vec<&str> = Vec::new();
    for l in lines {
        if l.trim_start().starts_with("```") {
            break;
        }
        body.push(l);
    }
    body.join("\n").trim().to_string()
}

fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// ASCII-only case fold: preserves byte offsets (unicode lowering may change
/// byte widths), so reason slices keep their original text.
fn fold_lower(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_uppercase() { c.to_ascii_lowercase() } else { c })
        .collect()
}

/// Find the LAST verdict line in the text (`VERDICT: pass` / `VERDICT: reject`,
/// case-insensitive). Tolerates ``` fences and prose around it; classification
/// happens on the verdict line itself.
pub fn parse_verdict(text: &str) -> Result<(), String> {
    // M4.5 加固：只承认「独占一行的结论行」，并排除提示词回声（指令模板里写着
    // `VERDICT: pass 或 VERDICT: reject`），避免把指令文本误判为真实结论。
    // 完全解析不出结论 → 按**基建失败**处理（走换人/重试），不再默认当退回。
    let lines: Vec<&str> = text.lines().collect();
    let mut last: Option<(usize, bool)> = None; // (行号, pass?)
    for (idx, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let low = fold_lower(line);
        let Some(vpos) = low.find("verdict") else { continue };
        let after = low[(vpos + 7).min(low.len())..].trim_start_matches(|ch: char| {
            ch.is_whitespace() || ch == ':' || ch == '：' || ch == '*' || ch == '`' || ch == '"' || ch == '\''
        });
        let pass = if after.starts_with("pass") {
            true
        } else if after.starts_with("reject") {
            false
        } else {
            continue;
        };
        // 回声判定（仅针对结论行本身）：
        //  - 同一行同时出现 pass 与 reject（指令模板形态）
        //  - 出现指令措辞（例如 / 若结论 / 大小写不限 / 「或」连接两个结论）
        let has_pass = low.contains("pass");
        let has_reject = low.contains("reject");
        let echo_like = (has_pass && has_reject)
            || low.contains("例如")
            || low.contains("若结论")
            || low.contains("大小写不限")
            || (low.contains('或') && has_pass && has_reject);
        if echo_like {
            continue;
        }
        last = Some((idx, pass));
    }
    match last {
        Some((_, true)) => Ok(()),
        Some((idx, false)) => Err(reject_reason_from(lines[idx], &lines[idx + 1..])),
        None => Err(format!(
            "{INFRA_ACCEPT_ERROR_PREFIX}验收者未给出可解析的结论行（`VERDICT: pass` 或 `VERDICT: reject`，需独占一行）；疑似输出格式问题或提示词回声，按基建失败重试"
        )),
    }
}

/// 退回理由：结论行中 `reject` 之后的内容；为空则取后续第一个非空行。
fn reject_reason_from(verdict_line: &str, following: &[&str]) -> String {
    let low = fold_lower(verdict_line);
    let at = low.find("verdict").unwrap_or(0);
    let mut seg = verdict_line[(at + 7).min(verdict_line.len())..].trim();
    seg = seg.trim_start_matches(|ch: char| {
        ch.is_whitespace() || ch == ':' || ch == '：' || ch == '*' || ch == '`' || ch == '"' || ch == '\''
    });
    let low_seg = fold_lower(seg);
    if low_seg.starts_with("reject") {
        seg = seg[6..].trim();
        seg = seg.trim_start_matches(|ch: char| {
            ch.is_whitespace() || ch == ':' || ch == '：' || ch == '-' || ch == '—' || ch == '*' || ch == '`'
        });
    }
    if !seg.is_empty() {
        return seg.to_string();
    }
    following
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| l.to_string())
        .unwrap_or_else(|| "验收者退回（未附理由）".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_variants() {
        assert!(parse_verdict("一切正常。\nVERDICT: pass").is_ok());
        assert!(parse_verdict("```text\nverdict: PASS\n```").is_ok());
        assert!(parse_verdict("结论 Verdict：PASS。").is_ok());
        // last occurrence wins
        assert!(parse_verdict("VERDICT: reject\nlater reconsidered\nVERDICT: pass").is_ok());
        // reject with reason on next line
        assert_eq!(
            parse_verdict("不错。\nVERDICT: reject\n缺少验收记录").unwrap_err(),
            "缺少验收记录"
        );
        // reject with reason on same line / in fenced output (original casing kept)
        assert_eq!(parse_verdict("```\nVERDICT: REJECT — 文件缺失\n```").unwrap_err(), "文件缺失");
        // M4.5：无结论行 → 基建失败（走换人/重试），不再当作退回
        assert!(parse_verdict("看起来没问题")
            .unwrap_err()
            .starts_with(INFRA_ACCEPT_ERROR_PREFIX));
        // keyword confusion: reject after pass on the same verdict line
        assert!(parse_verdict("VERDICT: pass").is_ok());
        assert!(parse_verdict("VERDICT: reject").is_err());
    }

    #[test]
    fn decide_prompt_contains_request() {        let d = crate::models::Decision {
            id: "dec_x".into(),
            project_id: "p".into(),
            task_id: Some("t".into()),
            kind: "exemption".into(),
            request: serde_json::json!({"reason": "simple"}),
            scope: "tactical".into(),
            status: "pending".into(),
            ruling: None,
            attempt: 0,
            created_at: 0,
            resolved_at: None,
        };
        let s = decide_prompt(None, None, &d);
        assert!(s.contains("exemption"));
        assert!(s.contains("simple"));
    }

    /// M5：知识段注入——planner/executor/acceptor 三类提示词在有条目时开头含规范段，
    /// 无条目时不含；正文在段内可检索。
    #[test]
    fn knowledge_injected_into_all_role_prompts() {
        let proj = Project {
            id: "prj1".into(),
            engineering_id: None,
            name: "P".into(),
            brief: "brief".into(),
            status: "running".into(),
            plan: None,
            workspace: None,
            agent_pool: None,
            repos: Vec::new(),
            created_at: 0,
        };
        let task = Task {
            id: "t1".into(),
            project_id: "prj1".into(),
            milestone_id: None,
            title: "T".into(),
            description: "desc".into(),
            depends_on: Vec::new(),
            assignee_role: "executor".into(),
            executor_agent_id: None,
            acceptor_agent_id: None,
            acceptance_required: true,
            acceptance_criteria: Some("c".into()),
            negotiation_status: "agreed".into(),
            negotiation_round: 0,
            execution_plan: None,
            plan_rejection_count: 0,
            scope_repos: Vec::new(),
            agent_pinned: false,
            tried_agents: serde_json::json!({}),
            status: "ready".into(),
            attempt: 0,
            rejection_count: 0,
            failure_count: 0,
            artifacts: Vec::new(),
            created_at: 0,
        };
        let body = "禁止在未评审时合并到 main";
        let ksec = format!("{}-{}", crate::knowledge::SECTION_MARK, body);

        let with = |ks: Option<&str>| -> [String; 5] {
            [
                plan_prompt(&proj, &[], ks),
                task_plan_prompt(Some(&proj), &task, None, None, None, ks),
                exec_prompt(Some(&proj), &task, None, ks),
                plan_review_prompt(&proj, &task, "plan", ks),
                accept_prompt(&proj, &task, "tail", ks),
            ]
        };
        for p in with(Some(&ksec)) {
            assert!(p.starts_with(crate::knowledge::SECTION_MARK), "规范段必须在提示词开头");
            assert!(p.contains(body), "提示词须包含规范正文");
        }
        // 无条目（未注入项目）→ 一律不含规范段与正文
        for p in with(None) {
            assert!(!p.contains(crate::knowledge::SECTION_MARK));
            assert!(!p.contains(body));
        }
        // 空白段等同于未注入
        for p in with(Some("   ")) {
            assert!(!p.contains(crate::knowledge::SECTION_MARK));
        }
    }

    #[test]
    fn prompt_arg_cap_keeps_head_and_tail() {
        let mut p = String::from("HEAD-INSTRUCTIONS\n");
        p.push_str(&"x".repeat(300_000));
        p.push_str("\nTAIL-REQUIREMENTS");
        let capped = cap_prompt_arg(&p);
        assert!(capped.len() <= PROMPT_ARG_MAX);
        assert!(capped.starts_with("HEAD-INSTRUCTIONS"));
        assert!(capped.ends_with("TAIL-REQUIREMENTS"));
        assert!(capped.contains("truncated"));
        // short prompts pass through untouched
        assert_eq!(cap_prompt_arg("short"), "short");
    }

    #[test]
    fn byte_helpers_never_split_utf8() {
        let s = "你好".repeat(50);
        // boundary-safe: a 4-byte cap keeps exactly one 3-byte char
        assert_eq!(head_bytes(&s, 4), "你");
        assert_eq!(head_bytes(&s, 3), "你");
        assert_eq!(tail_bytes(&s, 3), "好");
        let cut = cut_bytes(&s, 5);
        assert!(cut.len() <= 5);
    }

    #[test]
    fn log_tail_shape_marks_elision() {
        let lines: Vec<String> = (0..100).map(|i| format!("line{i}")).collect();
        let shaped = shape_log_tail(lines.clone(), 40, 60_000, lines.len());
        assert!(shaped.contains("已省略"));
        assert!(shaped.contains("line99"));
        assert!(!shaped.contains("line59")); // dropped from the front
        // small input passes through verbatim, no marker
        let small: Vec<String> = vec!["a".into(), "b".into()];
        assert_eq!(shape_log_tail(small.clone(), 40, 60_000, small.len()), "a\nb");
    }
}

#[cfg(test)]
mod verdict_tests {
    use super::*;

    #[test]
    fn accepts_plain_pass() {
        assert!(parse_verdict("复核完成\n\nVERDICT: pass").is_ok());
    }

    #[test]
    fn accepts_markdown_emphasized_pass() {
        assert!(parse_verdict("结论如下：\n**VERDICT: pass**\n").is_ok());
    }

    #[test]
    fn reject_with_reason_on_next_line() {
        let e = parse_verdict("核对后发现产物缺失。\nVERDICT: reject\n缺少 summary.txt 与验证记录").unwrap_err();
        assert!(e.contains("缺少 summary.txt"), "{e}");
        assert!(!e.starts_with(INFRA_ACCEPT_ERROR_PREFIX), "{e}");
    }

    #[test]
    fn reject_reason_on_same_line() {
        let e = parse_verdict("VERDICT: reject —— 产物为空").unwrap_err();
        assert!(e.contains("产物为空"), "{e}");
    }

    /// 关键回归：提示词回声（指令模板里含 `VERDICT: pass 或 VERDICT: reject`）
    /// 不得被当作真实结论；解析不出结论 → 基建失败（走换人/重试）而非退回。
    #[test]
    fn prompt_echo_is_not_a_verdict() {
        let echo = "输出要求：你的回复中必须包含明确的验收结论行：`VERDICT: pass` 或 `VERDICT: reject`（大小写不限）。\n若结论为 reject，请在结论行的下一行写明拒收理由。\n如果你在执行中发现现有规划需要调整，请在回复末尾单独一行输出：replan: {...}";
        let e = parse_verdict(echo).unwrap_err();
        assert!(e.starts_with(INFRA_ACCEPT_ERROR_PREFIX), "应判为基建失败: {e}");
    }

    #[test]
    fn real_verdict_wins_over_echo() {
        let mixed = "输出要求：必须包含 `VERDICT: pass` 或 `VERDICT: reject`（大小写不限）。\n\n核对完成，产物齐全。\nVERDICT: pass";
        assert!(parse_verdict(mixed).is_ok());
    }

    #[test]
    fn empty_or_no_verdict_is_infra_failure() {
        assert!(parse_verdict("").unwrap_err().starts_with(INFRA_ACCEPT_ERROR_PREFIX));
        assert!(parse_verdict("我看了文件，看起来没问题。").unwrap_err().starts_with(INFRA_ACCEPT_ERROR_PREFIX));
    }
}

#[cfg(test)]
mod idle_watchdog_tests {
    /// 看门狗判据：空闲时长达到阈值即判卡死（阈值复用配置的超时）。
    fn stuck(idle_ms: u64, threshold_ms: u64) -> bool {
        idle_ms >= threshold_ms
    }

    #[test]
    fn idle_watchdog_decision() {
        assert!(!stuck(0, 60_000));
        assert!(!stuck(59_999, 60_000));
        assert!(stuck(60_000, 60_000));
        assert!(stuck(3_600_000, 3_600_000));
    }
}
