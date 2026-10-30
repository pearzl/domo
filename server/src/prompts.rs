//! Prompt contracts for jobs. Every job receives an absolute result path and must
//! write JSON there; stdout is captured for audit but is not the protocol channel.

use crate::domain::{MAX_DERIVED_TASKS, MAX_PLAN_TASKS};
use crate::models::{
    MessageRow, JOB_ROLE_ANALYSIS, JOB_ROLE_DIRECT, JOB_ROLE_EXECUTOR, JOB_ROLE_SUMMARY,
    JOB_ROLE_VERIFIER, REPORT_MESSAGE_ROLE,
};

#[derive(Debug, Clone)]
pub struct DependencyInfo {
    pub key: String,
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub key: String,
    pub title: String,
    pub description: String,
    pub acceptance: Vec<String>,
    pub guidance: String,
}

#[derive(Debug, Clone)]
pub struct SummaryTaskInfo {
    pub key: String,
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct DerivedTaskInfo {
    pub key: String,
    pub title: String,
    pub stage: String,
    pub status: Option<String>,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct PromptData {
    pub role: String,
    pub project_id: String,
    pub project_name: String,
    pub goal: String,
    pub cwd: String,
    pub repo_path: String,
    pub base_branch: String,
    pub project_branch: String,
    pub task: Option<TaskInfo>,
    pub dependencies: Vec<DependencyInfo>,
    pub derived_from: Option<DependencyInfo>,
    pub derived_tasks: Vec<DerivedTaskInfo>,
    pub messages: Vec<MessageRow>,
    pub verifier_issues: Vec<String>,
    pub summary_tasks: Vec<SummaryTaskInfo>,
    pub completed_tasks: Vec<SummaryTaskInfo>,
    pub canceled_tasks: Vec<SummaryTaskInfo>,
    pub diff_stat: String,
    pub job_dir: String,
    pub result_path: String,
    pub attempt: i64,
    pub report_feedback: Option<String>,
    pub replan_reason: Option<String>,
    pub replan_source: Option<String>,
    pub confirm_mode: String,
}

pub fn render(data: &PromptData) -> String {
    match data.role.as_str() {
        JOB_ROLE_ANALYSIS => analysis_prompt(data),
        JOB_ROLE_DIRECT => direct_prompt(data),
        JOB_ROLE_EXECUTOR => executor_prompt(data),
        JOB_ROLE_VERIFIER => verifier_prompt(data),
        JOB_ROLE_SUMMARY => summary_prompt(data),
        _ => format!("未知作业角色: {}", data.role),
    }
}

fn common_header(data: &PromptData) -> String {
    format!(
        "你是 DOMO 本地多 Agent 协作平台中的团队成员。\n\
         \n## 工作区\n\
         - 当前工作目录就是你的工作区，也是唯一可写目录：{}\n\
         \n## 作业目录\n\
         - 用于写结果，不在 Git 仓库内：{}\n\
         \n## 项目目标\n\
         - 项目：{} ({})\n\
         - 目标：{}\n\
         \n## 仓库与分支\n\
         - 仓库主工作树（只读参考）：{}\n\
         - base 分支：{}\n\
         - 项目交付分支：{}\n\
         \n## 只读边界\n\
         - 仓库主工作树与其他工作区只读，只能在工作区内创建和修改文件。\n",
        data.cwd,
        data.job_dir,
        data.project_name,
        data.project_id,
        data.goal,
        data.repo_path,
        data.base_branch,
        data.project_branch
    )
}

fn result_rules(data: &PromptData, schema: &str) -> String {
    format!(
        "\n## 结果文件（必须执行）\n\
         完成工作后，必须把唯一的结果 JSON 写入这个绝对路径：\n{}\n\
         要求：\n\
         - 文件内容必须是单个合法 JSON 对象，不要加 Markdown 代码围栏；\n\
         - 即使任务失败，也必须写结果文件；\n\
         - 最终消息可以简要说明，但验收方只读取结果文件；\n\
         - 结构必须符合下面的 schema：\n{}\n",
        data.result_path, schema
    )
}

fn conversation_history(data: &PromptData) -> String {
    let mut history = String::new();
    for message in &data.messages {
        history.push_str(&format!(
            "\n[{}]\n{}\n",
            if message.role == "user" {
                "用户"
            } else {
                "规划者"
            },
            message.content
        ));
    }
    if history.is_empty() {
        history.push_str("\n（暂无对话）\n");
    }
    history
}

fn analysis_prompt(data: &PromptData) -> String {
    let auto = data.confirm_mode == crate::models::CONFIRM_AUTO;
    let replan = replan_section(data);
    format!(
        "{}你当前扮演**规划者**，负责项目分析：理解目标，并给出可以直接执行的项目方案。\n\
         \n## 分析要求\n\
         - 自主判断需要了解哪些上下文再下结论；不确定的内容先核实，不要凭空假设现状；\n\
         - 方案要贴合项目实际类型与既有结构，由你决定任务拆分的粒度和验收方式；\n\
         - 方案必须具体到可执行，禁止只给方法论、最佳实践清单或泛泛的目标复述；\n\
         - 必须说明可交付物、范围边界（明确不做什么）、关键假设、验收口径和主要风险；\n\
         - 验收口径要可观察或可执行，例如具体命令、文件内容或接口行为；\n\
         - 需要拆分任务时，每个任务写清要实现的目标、边界和验收方式；\n\
         - 单个方案最多包含 {MAX_PLAN_TASKS} 个任务；大型项目按阶段规划，先拆出能拿到关键进展的这一批；\n\
         - 关键进展完成后仍有大量工作未覆盖时，通过重规划派发下一批任务继续推进项目；不要试图用一次方案覆盖大型项目的全部工作；\n\
         - 任务必须可以独立执行、独立验收；任务之间用 deps 表示前置依赖，必须是无环 DAG；\n\
         - 任务产物由执行者在独立任务 worktree 中完成，验收通过后由 DOMO 自动合并到项目交付分支；\n\
         - 验收标准不要要求执行者直接提交、合并或推送到项目交付分支，也不要要求执行者执行 git commit；\n\
         - 不要修改任何代码或文件，当前工作区只用于阅读；\n\
         - 只有答案会改变任务拆分时才提问，最多 3 个；其余信息写进 assumptions，由用户在确认方案时修正；\n\
         - 项目目标是最初输入；对话中的后续消息可能包含补充或修订，以对话为准；\n\
         - 用户在对话中提出修改意见时，基于历史对话修订方案，不要从头重新分析；\n\
         - 目标简单到不需要拆分时输出 direct；由你判断是否存在需要单独安排的架构决策或验证环节。\n\
         {}\
         \n## 人工确认设置\n{}\n\
         \n## 项目分析对话\n{}\n\
         \n## 输出 schema\n\
         如果确实缺少会改变方案的关键信息，输出：\n\
         {{\"kind\":\"questions\",\"summary\":\"为什么必须澄清\",\"questions\":[\"问题1\",\"问题2\"]}}\n\
         需要拆分任务时输出：\n\
         {{\"kind\":\"plan\",\"summary\":\"分析结论：现状、方案取舍与风险\",\"goal\":\"确认后的目标\",\"assumptions\":[\"关键假设\"],\"acceptance\":[\"项目级验收口径\"],\"tasks\":[{{\"key\":\"T1\",\"title\":\"短标题\",\"description\":\"要实现的目标、边界和涉及范围\",\"acceptance\":[\"可执行的验收标准\"],\"deps\":[]}},{{\"key\":\"T2\",\"title\":\"...\",\"description\":\"...\",\"acceptance\":[\"...\"],\"deps\":[\"T1\"]}}],\"skip_confirmation\":false}}\n\
         可以直接完成时输出：\n\
         {{\"kind\":\"direct\",\"summary\":\"为什么不需要拆分\",\"goal\":\"确认后的目标\",\"plan\":[\"执行步骤\"],\"verification\":[\"自检方式\"],\"skip_confirmation\":false}}\n\
         key 使用 T1、T2 这类短标识；deps 引用其他任务的 key。\n\
         {}",
        common_header(data),
        replan,
        if auto {
            "当前为自动确认模式：方案足够明确时可以把 skip_confirmation 设为 true，DOMO 会直接开始执行；仍有歧义时必须提问或等待用户确认。"
        } else {
            "当前为人工确认模式：skip_confirmation 必须为 false，DOMO 会等待用户确认方案后开始执行。"
        },
        conversation_history(data),
        result_rules(
            data,
            "{\"kind\":\"questions\",\"questions\":[...]}、{\"kind\":\"plan\",\"goal\":\"...\",\"tasks\":[...],...} 或 {\"kind\":\"direct\",\"plan\":[...],\"verification\":[...],...}"
        )
    )
}

fn direct_prompt(data: &PromptData) -> String {
    let feedback = data
        .report_feedback
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("\n## 上一轮报告意见（本次必须处理）\n{value}\n"))
        .unwrap_or_default();
    format!(
        "{}你当前扮演**规划者**，并已被授权直接完成目标。此模式不拆分任务，由你一次性完成实现、自检和总结。\n\
         \n## 执行要求\n\
         - 自行决定实现方式、涉及范围和核验方式，目标是把项目目标真正落实为可用的结果；\n\
         - 如实记录做了什么、验证了哪些方面，以及仍然存在的风险，不要伪造结果；\n\
         - 不要在工作区以外读写文件；\n\
         - 不要执行 git commit、git merge、git push，DOMO 会在作业成功后提交变更；\n\
         - 完成后必须把结果 JSON 写入结果文件。\n\
         {}\
         \n## 项目分析对话\n{}\n\
         \n## 输出 schema\n\
         {{\"kind\":\"direct_done\",\"summary\":\"完成摘要\",\"report\":\"Markdown 项目报告，包含变更、自检结果和风险\"}}\n\
         {}",
        common_header(data),
        feedback,
        conversation_history(data),
        result_rules(
            data,
            "{\"kind\":\"direct_done\",\"summary\":\"...\",\"report\":\"Markdown 报告\"}"
        )
    )
}

fn replan_section(data: &PromptData) -> String {
    let Some(reason) = data
        .replan_reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return String::new();
    };
    let source = match data.replan_source.as_deref() {
        Some("executor") => "执行者",
        Some("human") => "人工",
        _ => "未知",
    };
    let completed = if data.completed_tasks.is_empty() {
        "（没有已完成任务）".to_string()
    } else {
        data.completed_tasks
            .iter()
            .map(|task| {
                format!(
                    "- {} {}：{}",
                    task.key,
                    task.title,
                    if task.summary.is_empty() {
                        "已完成"
                    } else {
                        &task.summary
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let canceled = if data.canceled_tasks.is_empty() {
        "（没有被取消的任务）".to_string()
    } else {
        data.canceled_tasks
            .iter()
            .map(|task| {
                format!(
                    "- {} {}：{}",
                    task.key,
                    task.title,
                    if task.summary.is_empty() {
                        "已取消"
                    } else {
                        &task.summary
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "\n## 重规划上下文\n\
         重规划原因（来源：{source}）：\n{reason}\n\
         \n已完成任务（保留在交付分支中，不要重新规划、不要重复实现）：\n{completed}\n\
         \n已取消任务（已退出活动 DAG，不要把它们作为前置依赖）：\n{canceled}\n\
         \n## 重规划约束\n\
         - 新计划只包含仍需完成的工作，不要重复已完成任务；\n\
         - 新计划最多包含 {MAX_PLAN_TASKS} 个任务；大型项目按阶段推进，本次只规划距离关键进展最近的一批，后续再通过重规划派发新任务；\n\
         - deps 只能引用新计划里的任务 key，不得引用已完成或已取消的旧任务；\n\
         - 可以复用已取消任务的 key；\n\
         - 如果重规划原因表明原计划存在事实错误，必须在新计划中修正。\n"
    )
}

fn executor_prompt(data: &PromptData) -> String {
    let task = match &data.task {
        Some(task) => task,
        None => return "执行作业缺少任务信息".into(),
    };
    let acceptance = task
        .acceptance
        .iter()
        .enumerate()
        .map(|(i, item)| format!("{}. {}", i + 1, item))
        .collect::<Vec<_>>()
        .join("\n");
    let deps = if data.dependencies.is_empty() {
        "（无前置任务）".to_string()
    } else {
        data.dependencies
            .iter()
            .map(|dep| {
                format!(
                    "- {} {}：{}",
                    dep.key,
                    dep.title,
                    if dep.summary.is_empty() {
                        "已完成"
                    } else {
                        &dep.summary
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let issues = if data.verifier_issues.is_empty() {
        "（首次执行，暂无验收反馈）".to_string()
    } else {
        data.verifier_issues
            .iter()
            .map(|issue| format!("- {issue}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let guidance = if task.guidance.trim().is_empty() {
        "（暂无人工指导）".to_string()
    } else {
        task.guidance.trim().to_string()
    };
    let derivation = match &data.derived_from {
        Some(parent) => format!(
            "\n## 派生任务约束\n\
             本任务是从父任务 {} {} 派生的子任务。完成工作后跳过验收阶段，由 DOMO 直接提交并合入父任务分支。\n\
             禁止再次派生新的子任务；如果任务仍然过于复杂，请在结果中说明失败原因或请求重新规划。\n",
            parent.key, parent.title
        ),
        None => String::new(),
    };
    let derived_children = if data.derived_tasks.is_empty() {
        String::new()
    } else {
        let items = data
            .derived_tasks
            .iter()
            .map(|child| {
                let state = child.state_label();
                format!(
                    "- {} {}（{}）：{}",
                    child.key,
                    child.title,
                    state,
                    if child.summary.is_empty() {
                        "尚无结果"
                    } else {
                        &child.summary
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n## 派生子任务\n{items}\n")
    };
    let derive_rule = if data.derived_from.is_some() {
        "禁止派生子任务；派生任务只能完成自身工作或请求重新规划。".to_string()
    } else {
        format!(
            "如果任务仍然过于复杂，可以派生子任务：单次 1 到 {MAX_DERIVED_TASKS} 个、deps 必须为空、key 在项目活动任务中唯一，且每个子任务必须写清描述和验收标准。"
        )
    };
    let schema = if data.derived_from.is_some() {
        "{\"kind\":\"done\",\"summary\":\"完成了什么、测试运行情况\",\"details\":\"可选\"}、\
         {\"kind\":\"failed\",\"reason\":\"失败原因\"} 或 \
         {\"kind\":\"replan\",\"reason\":\"需要重新规划的原因\"}"
    } else {
        "{\"kind\":\"done\",\"summary\":\"完成了什么、测试运行情况\",\"details\":\"可选\"}、\
         {\"kind\":\"failed\",\"reason\":\"失败原因\"}、\
         {\"kind\":\"replan\",\"reason\":\"需要重新规划的原因\"} 或 \
         {\"kind\":\"derive\",\"summary\":\"为什么需要派生\",\"tasks\":[{\"key\":\"T1.1\",\"title\":\"短标题\",\"description\":\"详细目标和边界\",\"acceptance\":[\"可核查的验收标准\"],\"deps\":[]}]}"
    };
    format!(
        "{}你当前扮演**执行者**，只负责下面这个任务。第 {} 次执行。\n\
         {}\
         \n## 任务\n{} - {}\n\n{}\n\
         \n## 人工指导\n{}\n\
         \n## 验收标准\n{}\n\
         \n## 前置任务结果\n{}\n\
         \n## 上一轮验收意见（必须逐条处理）\n{}\n\
         {}\
         \n## 执行要求\n\
         - 由你决定实现步骤和核验方式，目标是把任务描述和验收标准真正满足；\n\
         - 如实核验结果，说明验证了什么、怎么验证、结果如何，不要伪造结果；\n\
         - 不要在工作区以外读写文件；\n\
         - 不要执行 git commit、git checkout、git reset、git merge、git rebase 等改变历史或分支的命令；DOMO 会负责提交和合并；\n\
         - 如果无法完成，写清失败原因，不要伪造成功；\n\
         - 如果原计划存在事实错误，导致当前任务无法按描述完成，输出 replan 请求并说明原因；\n\
         - 大型项目按阶段推进：当前计划内的关键进展完成后，如果仍有大量工作没有纳入计划，输出 replan 请求，由规划者派发下一批任务；本批还有任务可以继续执行时不要请求重规划；\n\
         - {}；\n\
         - 结果文件路径固定，不要写到其他位置。\n\
         {}",
        common_header(data),
        data.attempt + 1,
        derivation,
        task.key,
        task.title,
        task.description,
        guidance,
        acceptance,
        deps,
        issues,
        derived_children,
        derive_rule,
        result_rules(data, schema)
    )
}

impl DerivedTaskInfo {
    fn state_label(&self) -> &str {
        if self.stage == crate::models::TASK_STAGE_COMPLETED {
            "已完成"
        } else if self.status.as_deref() == Some(crate::models::STATUS_BLOCKED) {
            "已阻塞"
        } else if self.status.as_deref() == Some(crate::models::STATUS_CANCELED) {
            "已取消"
        } else if self.stage == crate::models::TASK_STAGE_EXECUTING {
            "执行中"
        } else {
            "待执行"
        }
    }
}

fn verifier_prompt(data: &PromptData) -> String {
    let task = match &data.task {
        Some(task) => task,
        None => return "验收作业缺少任务信息".into(),
    };
    let acceptance = task
        .acceptance
        .iter()
        .enumerate()
        .map(|(i, item)| format!("{}. {}", i + 1, item))
        .collect::<Vec<_>>()
        .join("\n");
    let issues = if data.verifier_issues.is_empty() {
        "（无历史拒绝记录）".to_string()
    } else {
        data.verifier_issues
            .iter()
            .map(|issue| format!("- {issue}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let guidance = if task.guidance.trim().is_empty() {
        "（暂无人工指导）".to_string()
    } else {
        task.guidance.trim().to_string()
    };
    format!(
        "{}你当前扮演**验收者**。你所在的是任务分支的独立只读工作区，用于核验执行结果。项目交付分支会在验收通过后由 DOMO 自动合并；不要因为任务改动尚未出现在项目交付分支而判定失败，也不要把“已提交到项目交付分支”当作验收前置条件。\n\
         \n## 任务\n{} - {}\n\n{}\n\
         \n## 人工指导\n{}\n\
         \n## 验收标准\n{}\n\
         \n## 历史拒绝意见\n{}\n\
         \n## 改动规模\n{}\n\
         \n## 验收要求\n\
         - 由你决定核验方式，以任务描述和验收标准为唯一判据；\n\
         - 如实验证，不能只看执行者的自述，也不能伪造核验结果；\n\
         - 只判断任务描述和验收标准是否被满足，不扩大范围；\n\
         - 如果发现真实缺陷、验证失败、遗漏或伪造，判定不通过；\n\
         - 不要在工作区以外读写文件，也不要修改任何文件；验收工作区在结束后会被删除；\n\
         - 即使无法验收，也要写结果文件。\n\
         {}",
        common_header(data),
        task.key,
        task.title,
        task.description,
        guidance,
        acceptance,
        issues,
        if data.diff_stat.is_empty() {
            "（没有检测到改动）"
        } else {
            &data.diff_stat
        },
        result_rules(
            data,
            "{\"pass\":true,\"reason\":\"通过原因\",\"issues\":[]} 或 {\"pass\":false,\"reason\":\"拒绝原因\",\"issues\":[\"具体问题\"]}"
        )
    )
}

fn summary_prompt(data: &PromptData) -> String {
    let tasks = data
        .summary_tasks
        .iter()
        .map(|task| {
            format!(
                "- {} {}：{}",
                task.key,
                task.title,
                if task.summary.is_empty() {
                    "已完成"
                } else {
                    &task.summary
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let reviews: Vec<&MessageRow> = data
        .messages
        .iter()
        .filter(|message| message.role == REPORT_MESSAGE_ROLE)
        .collect();
    let feedback = if reviews.is_empty() {
        // 兼容只保存了最新一条意见的历史项目。
        data.report_feedback
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|value| format!("\n## 用户对上一版报告的意见\n{value}\n"))
            .unwrap_or_default()
    } else {
        let items = reviews
            .iter()
            .enumerate()
            .map(|(index, message)| format!("{}. {}", index + 1, message.content))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "\n## 人工对报告的评议记录（共 {} 条，必须逐条回应）\n{items}\n",
            reviews.len()
        )
    };
    format!(
        "{}你当前扮演**规划者**，负责撰写项目总结报告。所有任务已经完成并合并到项目交付分支。\n\
         \n## 任务结果\n{}\n\
         \n## 交付分支改动规模\n{}\n\
         {}\n\
         ## 报告要求\n\
         - 使用 Markdown，面向用户说明最终交付了什么、关键思路、验证情况、已知风险；\n\
         - 如实验证并说明依据，不要编造没有发生的验证、测试或产物；\n\
         - 报告是用户验收项目的依据，要具体、可核查；\n\
         - 不要在工作区以外读写文件，也不要修改任何代码或文件。\n\
         {}",
        common_header(data),
        tasks,
        if data.diff_stat.is_empty() {
            "（没有检测到改动）"
        } else {
            &data.diff_stat
        },
        feedback,
        result_rules(
            data,
            "{\"kind\":\"report\",\"report\":\"Markdown 格式的项目报告\"}"
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{CONFIRM_AUTO, CONFIRM_MANUAL, REPORT_MESSAGE_ROLE, TASK_STAGE_COMPLETED};

    const RESULT_PATH: &str = "/workspace/jobs/job-1/result.json";
    const ACCEPTANCE: &str = "运行项目自带的检查并确保任务目标成立";
    const GUIDANCE: &str = "优先保证现有行为不变";

    fn sample_task() -> TaskInfo {
        TaskInfo {
            key: "T1".into(),
            title: "整理结果".into(),
            description: "把任务结果整理成可核查的输出".into(),
            acceptance: vec![ACCEPTANCE.into()],
            guidance: GUIDANCE.into(),
        }
    }

    fn sample_data(role: &str) -> PromptData {
        PromptData {
            role: role.into(),
            project_id: "proj-1".into(),
            project_name: "示例项目".into(),
            goal: "让交付结果可用".into(),
            cwd: "/workspace".into(),
            repo_path: "/repos/example".into(),
            base_branch: "main".into(),
            project_branch: "domo/p-1".into(),
            task: Some(sample_task()),
            dependencies: vec![DependencyInfo {
                key: "T0".into(),
                title: "前置任务".into(),
                summary: "已完成".into(),
            }],
            derived_from: None,
            derived_tasks: Vec::new(),
            messages: vec![MessageRow {
                id: "msg-1".into(),
                project_id: "proj-1".into(),
                role: "user".into(),
                content: "请按计划推进".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            }],
            verifier_issues: Vec::new(),
            summary_tasks: vec![SummaryTaskInfo {
                key: "T1".into(),
                title: "整理结果".into(),
                summary: "已完成".into(),
            }],
            completed_tasks: vec![SummaryTaskInfo {
                key: "T1".into(),
                title: "整理结果".into(),
                summary: "已完成".into(),
            }],
            canceled_tasks: vec![SummaryTaskInfo {
                key: "T2".into(),
                title: "旧任务".into(),
                summary: "已取消".into(),
            }],
            diff_stat: "1 file changed".into(),
            job_dir: "/workspace/jobs/job-1".into(),
            result_path: RESULT_PATH.into(),
            attempt: 0,
            report_feedback: None,
            replan_reason: None,
            replan_source: None,
            confirm_mode: CONFIRM_MANUAL.into(),
        }
    }

    fn render_role(role: &str) -> String {
        render(&sample_data(role))
    }

    fn result_schema_text(role: &str) -> &'static str {
        match role {
            JOB_ROLE_ANALYSIS => "\"kind\":\"questions\",\"questions\":[...]",
            JOB_ROLE_DIRECT => {
                "\"kind\":\"direct_done\",\"summary\":\"...\",\"report\":\"Markdown 报告\""
            }
            JOB_ROLE_EXECUTOR => "\"kind\":\"done\",\"summary\":\"完成了什么、测试运行情况\"",
            JOB_ROLE_VERIFIER => "\"pass\":true,\"reason\":\"通过原因\",\"issues\":[]",
            JOB_ROLE_SUMMARY => "\"kind\":\"report\",\"report\":\"Markdown 格式的项目报告\"",
            other => panic!("未知的作业角色: {other}"),
        }
    }

    #[test]
    fn every_role_includes_result_path_and_schema() {
        for role in [
            JOB_ROLE_ANALYSIS,
            JOB_ROLE_DIRECT,
            JOB_ROLE_EXECUTOR,
            JOB_ROLE_VERIFIER,
            JOB_ROLE_SUMMARY,
        ] {
            let prompt = render_role(role);
            assert!(prompt.contains(RESULT_PATH), "{role} 缺少结果文件路径");
            assert!(
                prompt.contains(result_schema_text(role)),
                "{role} 缺少输出 schema"
            );
        }
    }

    #[test]
    fn analysis_prompt_leaves_context_choice_to_planner() {
        let prompt = analysis_prompt(&sample_data(JOB_ROLE_ANALYSIS));
        let readme = ["README", ".md"].concat();
        let dev_doc = ["docs/", "DEV", ".md"].concat();
        let read_first = ["先阅读", "仓库"].concat();
        for forbidden in [readme, dev_doc, read_first] {
            assert!(
                !prompt.contains(&forbidden),
                "分析提示词不应限定固定上下文: {forbidden}"
            );
        }
        assert!(
            prompt.contains(&format!("最多包含 {MAX_PLAN_TASKS} 个任务")),
            "缺少方案任务数量上限"
        );
        assert!(prompt.contains("重规划"), "缺少大型项目分阶段推进说明");
        assert!(prompt.contains("\"kind\":\"plan\""), "缺少 plan schema");
        assert!(prompt.contains("\"kind\":\"direct\""), "缺少 direct schema");
        assert!(
            prompt.contains("\"kind\":\"questions\""),
            "缺少 questions schema"
        );
        assert!(prompt.contains("无环 DAG"), "缺少无环依赖约束");
        assert!(prompt.contains("最多 3 个"), "缺少提问数量约束");
        assert!(prompt.contains("不要修改任何代码或文件"), "缺少只读约束");
    }

    #[test]
    fn replan_and_derivation_headings_are_stable() {
        let mut data = sample_data(JOB_ROLE_ANALYSIS);
        data.replan_reason = Some("原计划与仓库现状冲突".into());
        data.replan_source = Some("executor".into());
        let prompt = analysis_prompt(&data);
        assert!(prompt.contains("## 重规划上下文"), "缺少重规划标题");
        assert!(prompt.contains("已完成任务"), "缺少已完成任务标题");
        assert!(prompt.contains("已取消任务"), "缺少已取消任务标题");
        assert!(prompt.contains("原计划与仓库现状冲突"), "缺少重规划原因");
        assert!(
            prompt.contains(&format!("新计划最多包含 {MAX_PLAN_TASKS} 个任务")),
            "缺少重规划任务数量上限"
        );

        let mut child = sample_data(JOB_ROLE_EXECUTOR);
        child.derived_from = Some(DependencyInfo {
            key: "T1".into(),
            title: "父任务".into(),
            summary: "进行中".into(),
        });
        let child_prompt = render(&child);
        assert!(child_prompt.contains("## 派生任务约束"), "缺少派生约束标题");
        assert!(child_prompt.contains("跳过验收阶段"), "缺少跳过验收说明");
        assert!(
            child_prompt.contains("禁止再次派生新的子任务"),
            "缺少禁止再次派生说明"
        );

        let mut parent = sample_data(JOB_ROLE_EXECUTOR);
        parent.derived_tasks = vec![DerivedTaskInfo {
            key: "T1.1".into(),
            title: "子任务".into(),
            stage: TASK_STAGE_COMPLETED.into(),
            status: None,
            summary: "已完成".into(),
        }];
        let parent_prompt = render(&parent);
        assert!(
            parent_prompt.contains("## 派生子任务"),
            "缺少派生子任务标题"
        );
        assert!(parent_prompt.contains("T1.1"), "缺少子任务标识");
        assert!(
            parent_prompt.contains(&format!("1 到 {MAX_DERIVED_TASKS} 个")),
            "缺少派生子任务数量上限"
        );
    }

    #[test]
    fn executor_and_verifier_keep_task_acceptance_and_guidance() {
        for role in [JOB_ROLE_EXECUTOR, JOB_ROLE_VERIFIER] {
            let prompt = render_role(role);
            assert!(prompt.contains("T1"), "{role} 缺少任务标识");
            assert!(prompt.contains("整理结果"), "{role} 缺少任务标题");
            assert!(prompt.contains(ACCEPTANCE), "{role} 缺少验收标准");
            assert!(prompt.contains(GUIDANCE), "{role} 缺少人工指导");
        }
    }

    #[test]
    fn each_role_states_readonly_and_commit_boundaries() {
        for role in [
            JOB_ROLE_ANALYSIS,
            JOB_ROLE_DIRECT,
            JOB_ROLE_EXECUTOR,
            JOB_ROLE_VERIFIER,
            JOB_ROLE_SUMMARY,
        ] {
            let prompt = render_role(role);
            assert!(prompt.contains("/workspace"), "{role} 缺少工作区");
            assert!(
                prompt.contains("/workspace/jobs/job-1"),
                "{role} 缺少作业目录"
            );
            assert!(prompt.contains("main"), "{role} 缺少 base 分支");
            assert!(prompt.contains("domo/p-1"), "{role} 缺少项目交付分支");
            assert!(prompt.contains("只读边界"), "{role} 缺少只读边界");
        }
    }

    #[test]
    fn summary_prompt_keeps_review_feedback() {
        let mut data = sample_data(JOB_ROLE_SUMMARY);
        data.messages.push(MessageRow {
            id: "msg-2".into(),
            project_id: "proj-1".into(),
            role: REPORT_MESSAGE_ROLE.into(),
            content: "补充验证情况".into(),
            created_at: "2026-01-01T00:00:01Z".into(),
        });
        let prompt = render(&data);
        assert!(prompt.contains("评议记录"), "缺少评议记录标题");
        assert!(prompt.contains("补充验证情况"), "缺少评议内容");
    }

    #[test]
    fn auto_confirmation_mode_is_reflected() {
        let mut data = sample_data(JOB_ROLE_ANALYSIS);
        data.confirm_mode = CONFIRM_AUTO.into();
        let prompt = render(&data);
        assert!(prompt.contains("自动确认模式"), "缺少自动确认说明");
        assert!(
            prompt.contains("skip_confirmation 设为 true"),
            "缺少跳过确认说明"
        );
    }
}
