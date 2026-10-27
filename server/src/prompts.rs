//! Prompt contracts for jobs. Every job receives an absolute result path and must
//! write JSON there; stdout is captured for audit but is not the protocol channel.

use crate::models::{
    MessageRow, JOB_ROLE_EXECUTOR, JOB_ROLE_GOAL, JOB_ROLE_SUMMARY, JOB_ROLE_VERIFIER,
    REPORT_MESSAGE_ROLE,
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
}

pub fn render(data: &PromptData) -> String {
    match data.role.as_str() {
        JOB_ROLE_GOAL => goal_prompt(data),
        JOB_ROLE_EXECUTOR => executor_prompt(data),
        JOB_ROLE_VERIFIER => verifier_prompt(data),
        JOB_ROLE_SUMMARY => summary_prompt(data),
        _ => format!("未知作业角色: {}", data.role),
    }
}

fn common_header(data: &PromptData) -> String {
    format!(
        "你是 DOMO 本地多 Agent 协作平台中的团队成员。当前工作目录就是你的工作区，所有文件操作都必须限制在该目录内。\n\
         工作区（唯一可写目录）：{}\n\
         禁止 cd 到仓库主工作树或其他工作区，禁止在工作区以外读写文件。\n\
         作业目录（用于写结果，不在 Git 仓库内）：{}\n\
         项目：{} ({})\n\
         项目目标：\n{}\n\
         仓库主工作树（只读参考，禁止写入）：{}\n\
         base 分支：{}，项目交付分支：{}\n",
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

fn goal_prompt(data: &PromptData) -> String {
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
    let replan = replan_section(data);
    format!(
        "{}你当前扮演**规划者**。先判断项目目标是否足够明确：只有确实缺少会影响实现的关键信息时才提问；\
         如果目标已经足够清晰，直接给出任务规划。不要为了提问而提问。\n\
         \n## 目标确认对话\n{}\n\
         {}\
         \n## 规划约束\n\
         - 任务必须是可以独立执行、独立验收的最小单元；\n\
         - 任务之间用 deps 表示前置依赖，必须是无环 DAG；\n\
         - 每个任务都要有清晰描述和可核查的验收标准；\n\
         - 任务数量控制在 1 到 8 个，能合并的不要拆碎；\n\
         - 任务产物由执行者在独立任务 worktree 中完成，验收通过后由 DOMO 自动合并到项目交付分支；\n\
         - 验收标准不要要求执行者直接提交、合并或推送到项目交付分支，也不要要求执行者执行 git commit；\n\
         - 不要修改任何代码或文件，当前工作区只用于阅读；\n\
         - 需要阅读仓库时，优先看 README.md、docs/DEV.md 和相关源码。\n\
         \n## 输出 schema\n\
         如果需要澄清，输出：\n\
         {{\"kind\":\"questions\",\"summary\":\"为什么需要澄清\",\"questions\":[\"问题1\",\"问题2\"]}}\n\
         如果可以直接规划，输出：\n\
         {{\"kind\":\"plan\",\"summary\":\"规划说明\",\"tasks\":[{{\"key\":\"T1\",\"title\":\"短标题\",\"description\":\"详细目标和边界\",\"acceptance\":[\"可执行的验收标准\"],\"deps\":[]}},{{\"key\":\"T2\",\"title\":\"...\",\"description\":\"...\",\"acceptance\":[\"...\"],\"deps\":[\"T1\"]}}]}}\n\
         key 使用 T1、T2 这类短标识；deps 引用其他任务的 key。\n\
         {}",
        common_header(data),
        history,
        replan,
        result_rules(
            data,
            "{\"kind\":\"plan\",\"summary\":\"...\",\"tasks\":[...]} 或 {\"kind\":\"questions\",\"questions\":[...]}"
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
        "禁止派生子任务；派生任务只能完成自身工作或请求重新规划。"
    } else {
        "如果任务仍然过于复杂，可以派生子任务：单次 1 到 8 个、deps 必须为空、key 在项目活动任务中唯一，且每个子任务必须写清描述和验收标准。"
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
         \n## 人工指导（必须遵守）\n{}\n\
         \n## 验收标准\n{}\n\
         \n## 前置任务结果\n{}\n\
         \n## 上一轮验收意见（必须逐条处理）\n{}\n\
         {}\
         \n## 执行要求\n\
         - 在当前工作区内完成实现，不要修改工作区以外的文件；禁止 cd 到仓库主工作树或其他工作区；\n\
         - 可以阅读仓库中的文档和源码，遵循项目现有结构、命名和风格；\n\
         - 必须实际运行与改动相关的构建或测试（在工作区内执行），并在结果里说明运行了什么；\n\
         - 不要执行 git commit、git checkout、git reset、git merge、git rebase 等改变历史或分支的命令；DOMO 会负责提交和合并；\n\
         - 如果无法完成，写清失败原因，不要伪造成功；\n\
         - 如果原计划存在事实错误，导致当前任务无法按描述完成，输出 replan 请求并说明原因；\n\
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
    format!(
        "{}你当前扮演**验收者**。你所在的是任务分支的独立只读工作区，用于核验执行结果。项目交付分支会在验收通过后由 DOMO 自动合并；不要因为任务改动尚未出现在项目交付分支而判定失败，也不要把“已提交到项目交付分支”当作验收前置条件。\n\
         \n## 任务\n{} - {}\n\n{}\n\
         \n## 验收标准\n{}\n\
         \n## 历史拒绝意见\n{}\n\
         \n## 改动规模\n{}\n\
         \n## 验收要求\n\
         - 必须实际检查代码和改动，并运行相关构建或测试；不能只看执行者的自述；\n\
         - 所有检查命令必须在当前验收工作区内执行，禁止 cd 到仓库主工作树或其他工作区；\n\
         - 只判断任务描述和验收标准是否被满足，不扩大范围；\n\
         - 如果发现真实缺陷、测试失败、遗漏或伪造，判定不通过；\n\
         - 不要修改任何文件；验收工作区在结束后会被删除；\n\
         - 即使无法验收，也要写结果文件。\n\
         {}",
        common_header(data),
        task.key,
        task.title,
        task.description,
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
         - 使用 Markdown，面向用户说明最终交付了什么、关键改动、验证情况、已知风险；\n\
         - 不要编造没有发生的测试或产物；\n\
         - 报告是用户验收项目的依据，要具体、可核查；\n\
         - 不要修改任何代码或文件。\n\
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
