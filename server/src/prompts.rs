//! Prompt contracts for jobs. Every job receives an absolute result path and must
//! write JSON there; stdout is captured for audit but is not the protocol channel.

use crate::models::{
    MessageRow, JOB_ROLE_EXECUTOR, JOB_ROLE_GOAL, JOB_ROLE_SUMMARY, JOB_ROLE_VERIFIER,
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
}

#[derive(Debug, Clone)]
pub struct SummaryTaskInfo {
    pub key: String,
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct PromptData {
    pub role: String,
    pub project_id: String,
    pub project_name: String,
    pub goal: String,
    pub repo_path: String,
    pub base_branch: String,
    pub project_branch: String,
    pub task: Option<TaskInfo>,
    pub dependencies: Vec<DependencyInfo>,
    pub messages: Vec<MessageRow>,
    pub verifier_issues: Vec<String>,
    pub summary_tasks: Vec<SummaryTaskInfo>,
    pub diff_stat: String,
    pub job_dir: String,
    pub result_path: String,
    pub attempt: i64,
    pub report_feedback: Option<String>,
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
         作业目录（用于写结果，不在 Git 仓库内）：{}\n\
         项目：{} ({})\n\
         项目目标：\n{}\n\
         仓库：{}\n\
         base 分支：{}，项目交付分支：{}\n",
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
    format!(
        "{}你当前扮演**规划者**。先判断项目目标是否足够明确：只有确实缺少会影响实现的关键信息时才提问；\
         如果目标已经足够清晰，直接给出任务规划。不要为了提问而提问。\n\
         \n## 目标确认对话\n{}\n\
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
        result_rules(
            data,
            "{\"kind\":\"plan\",\"summary\":\"...\",\"tasks\":[...]} 或 {\"kind\":\"questions\",\"questions\":[...]}"
        )
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
    format!(
        "{}你当前扮演**执行者**，只负责下面这个任务。第 {} 次执行。\n\
         \n## 任务\n{} - {}\n\n{}\n\
         \n## 验收标准\n{}\n\
         \n## 前置任务结果\n{}\n\
         \n## 上一轮验收意见（必须逐条处理）\n{}\n\
         \n## 执行要求\n\
         - 在当前工作目录内完成实现，不要修改工作目录以外的文件；\n\
         - 可以阅读仓库中的文档和源码，遵循项目现有结构、命名和风格；\n\
         - 必须实际运行与改动相关的构建或测试，并在结果里说明运行了什么；\n\
         - 不要执行 git commit、git checkout、git reset、git merge、git rebase 等改变历史或分支的命令；DOMO 会负责提交和合并；\n\
         - 如果无法完成，写清失败原因，不要伪造成功；\n\
         - 结果文件路径固定，不要写到其他位置。\n\
         {}",
        common_header(data),
        data.attempt + 1,
        task.key,
        task.title,
        task.description,
        acceptance,
        deps,
        issues,
        result_rules(
            data,
            "{\"kind\":\"done\",\"summary\":\"完成了什么、测试运行情况\",\"details\":\"可选\"} 或 {\"kind\":\"failed\",\"reason\":\"失败原因\"}"
        )
    )
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
    let feedback = data
        .report_feedback
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|value| format!("\n## 用户对上一版报告的意见\n{}\n", value))
        .unwrap_or_default();
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
