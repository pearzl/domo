//! Prompt contracts for jobs. Every job receives an absolute result path and must
//! write JSON there; stdout is captured for audit but is not the protocol channel.

use crate::domain::{MAX_DERIVED_TASKS, MAX_PLAN_TASKS};
use crate::models::{
    MessageRow, JOB_ROLE_ACCEPTANCE, JOB_ROLE_ANALYSIS, JOB_ROLE_DIRECT, JOB_ROLE_EXECUTOR,
    JOB_ROLE_SUMMARY, JOB_ROLE_VERIFIER, REPLAN_SOURCE_ACCEPTANCE, REPLAN_SOURCE_EXECUTOR,
    REPLAN_SOURCE_HUMAN, REPORT_MESSAGE_ROLE,
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
    pub workspace_type: String,
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
    pub acceptance_criteria: Vec<String>,
    pub acceptance_result: Option<String>,
    pub project_summary: Option<String>,
    pub project_assumptions: Vec<String>,
    pub confirm_mode: String,
}

pub fn render(data: &PromptData) -> String {
    match data.role.as_str() {
        JOB_ROLE_ANALYSIS => analysis_prompt(data),
        JOB_ROLE_DIRECT => direct_prompt(data),
        JOB_ROLE_EXECUTOR => executor_prompt(data),
        JOB_ROLE_VERIFIER => verifier_prompt(data),
        JOB_ROLE_ACCEPTANCE => acceptance_prompt(data),
        JOB_ROLE_SUMMARY => summary_prompt(data),
        _ => format!("未知作业角色: {}", data.role),
    }
}

/// Follow-up message for a run that exited cleanly without a result file. A
/// resumable session keeps the original prompt; other agents receive the
/// original prompt followed by this message.
pub fn continuation(data: &PromptData) -> String {
    let mut text = if data.role == JOB_ROLE_EXECUTOR && data.derived_from.is_none() {
        "继续。\n如果任务仍然过于复杂，可以派生子任务。\n".to_string()
    } else {
        "继续。\n".to_string()
    };
    let guidance = data
        .task
        .as_ref()
        .map(|task| task.guidance.trim())
        .unwrap_or_default();
    if !guidance.is_empty() {
        text.push_str(&format!("\n人工指导：{guidance}\n"));
    }
    text
}

fn common_header(data: &PromptData) -> String {
    let workspace = if data.workspace_type == crate::models::WORKSPACE_PLAIN {
        format!(
            "## 工作区\n\
             - 工作目录（唯一可写）：{}\n\
             - 作业目录（写结果）：{}\n\
             \n## 项目目标\n\
             - {} ({})：{}\n\
             \n## 只读边界\n\
             - 只能在工作目录或作业目录内创建和修改文件。\n",
            data.cwd, data.job_dir, data.project_name, data.project_id, data.goal
        )
    } else {
        format!(
            "## 工作区\n\
             - 工作目录（唯一可写）：{}\n\
             - 作业目录（写结果，不在 Git 仓库内）：{}\n\
             \n## 项目目标\n\
             - {} ({})：{}\n\
             \n## 仓库与分支\n\
             - 主工作树（只读参考）：{}\n\
             - base 分支：{}\n\
             - 项目交付分支：{}\n\
             \n## 只读边界\n\
             - 仓库主工作树与其他工作区只读；只能在工作区或作业目录内创建和修改文件。\n",
            data.cwd,
            data.job_dir,
            data.project_name,
            data.project_id,
            data.goal,
            data.repo_path,
            data.base_branch,
            data.project_branch
        )
    };
    let mut header = format!("你是 DOMO 本地多 Agent 协作平台中的团队成员。\n\n{workspace}");
    header.push_str(long_command_rules());
    header
}

/// 普通工作区没有分支、worktree 与合入步骤，任务产物直接留在交付目录。
fn plain_workspace(data: &PromptData) -> bool {
    data.workspace_type == crate::models::WORKSPACE_PLAIN
}

fn long_command_rules() -> &'static str {
    "\n## 长命令进度\n\
     - 最少每隔5分钟要有一次内容输出，否则会被判定为卡死而被关闭。\n"
}

fn verification_rules() -> &'static str {
    "- 可以运行测试、探针、构建和检查器，核对实际退出码与报告；这些命令可以创建缓存、日志、临时数据和报告。\n\
     - 验证产物只能写入工作区内的验收产物目录或作业目录；Git 工作区使用已忽略目录，普通工作区使用独立验收产物目录；通过命令参数或环境变量指定输出位置。\n\
     - 不要修改交付源码、配置、文档、输入资源或现有交付物，也不要修改忽略规则；不得执行 git add、git commit、git merge、git rebase、git reset、git clean、git push 等改变暂存区、历史或分支或清理工作区的命令。\n\
     - 不要在工作区和作业目录以外读写文件；结果 JSON 写入指定结果文件。\n"
}

fn result_rules(data: &PromptData, schema: &str) -> String {
    format!(
        "\n## 结果文件（必须执行）\n\
         把唯一的结果 JSON 写入：{}\n\
         - 单个合法 JSON 对象，不加 Markdown 代码围栏；\n\
         - 即使失败也必须写入，验收方只读取该文件；\n\
         - 结构必须符合 schema：\n{}\n",
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
    let task_rules = if plain_workspace(data) {
        "- 任务产物直接写入普通工作区；任务按依赖顺序依次执行，不创建分支或 worktree；\n\
         - 验收标准不得要求执行者提交、合并、推送或 git commit；\n\
         - 本工作区不使用 Git，不要执行改变仓库状态的版本控制命令；\n"
    } else {
        "- 任务产物在独立 worktree 完成，验收通过后由 DOMO 合并到交付分支；合并冲突时退回执行者解决并重新验收；\n\
         - 验收标准不得要求执行者提交、合并、推送或 git commit；\n\
         - 不要修改任何代码或文件，不执行改变暂存区、历史或分支的 git 命令，仅阅读；\n"
    };
    format!(
        "{}你当前扮演**规划者**，负责项目分析：理解目标并给出可执行的项目方案。\n\
         \n## 分析要求\n\
         - 方案贴合实际，具体可执行，禁止方法论或目标复述；\n\
         - 说明可交付物、范围边界（不做什么）、关键假设、验收口径与风险；验收口径须可观察或可执行；\n\
         - 每个任务写清目标、边界和验收方式；单个方案最多包含 {MAX_PLAN_TASKS} 个任务；大型项目分阶段，后续通过重规划派发下一批；\n\
         - 任务可独立执行与验收；deps 表示前置依赖，须为无环 DAG；\n\
         {task_rules}\
         - 仅在会改变任务拆分时提问，最多 3 个；其余写入 assumptions；\n\
         - 以最新对话为准，按历史修订意见；\n\
         - 目标简单到不需要拆分时输出 direct。\n\
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
            "当前为自动确认模式：方案足够明确时可以把 skip_confirmation 设为 true；重规划草稿生成后由 DOMO 直接应用。"
        } else {
            "当前为人工确认模式：skip_confirmation 必须为 false，首次分析和重规划都必须等待用户确认方案后开始执行。"
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
         - 自行决定实现方式、涉及范围和核验方式，把项目目标落实为可用的结果；\n\
         - 如实说明做了什么、验证了哪些方面以及仍然存在的风险，不要伪造结果；\n\
         - 不要在工作区以外读写文件；\n\
         - 不要执行 git add、git commit、git merge、git push 等改变暂存区、历史或分支的命令，DOMO 会在作业成功后提交变更；\n\
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
        Some(REPLAN_SOURCE_EXECUTOR) => "执行者",
        Some(REPLAN_SOURCE_ACCEPTANCE) => "项目验收",
        Some(REPLAN_SOURCE_HUMAN) => "人工",
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
         \n完整保留的交付任务链（不要重新规划、不要重复实现）：\n{completed}\n\
         \n不保留任务（未完成任务；已经独立验收并交付的子任务仍保留）：\n{canceled}\n\
         \n## 重规划约束\n\
         - 必须沿用当前项目目标，重规划不得修改目标；只有用户主动提交新目标才能改变它；\n\
         - 新计划只包含仍需完成的工作，不要重复已完成任务；\n\
         - 新计划最多包含 {MAX_PLAN_TASKS} 个任务；大型项目按阶段推进，本次只规划距离关键进展最近的一批，后续再通过重规划派发新任务；\n\
         - deps 只能引用新计划里的任务 key，不得引用已完成或已取消的旧任务；\n\
         - 可以复用当前未完成或已取消任务的 key；\n\
         - 如果重规划原因表明原计划存在事实错误，必须在新计划中修正；\n\
         - 不得降低项目级验收口径；如需调整范围，必须在方案摘要中说明原因和影响。\n"
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
             本任务是从父任务 {} {} 派生的子任务。完成工作后接受独立验收，里程碑还需人工验收，{}。\n\
             禁止再次派生新的子任务；如果任务仍然过于复杂，请在结果中说明失败原因或请求重新规划。\n",
            parent.key,
            parent.title,
            if plain_workspace(data) {
                "产物直接保留在普通工作区"
            } else {
                "验收通过后由 DOMO 合入项目交付分支"
            }
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
            "任务仍复杂时可派生子任务：单次 1 到 {MAX_DERIVED_TASKS} 个、deps 为空、key 在活动任务中唯一，每个子任务写清描述与验收标准。"
        )
    };
    let git_rules = if plain_workspace(data) {
        "- 产物直接写入普通工作区；不要执行 git add、git commit、git merge 等版本控制命令；\n\
         - 无法完成时写清原因，不伪造成功；\n"
    } else {
        "- 不要执行 git add、git commit、git checkout、git reset、git merge、git rebase 等改变暂存区、历史或分支的命令；DOMO 负责暂存、提交和合并；\n\
         - 合并冲突状态下，编辑冲突文件消除标记并保留所需改动；可运行只读 git 查看状态，但不要执行会改变暂存区、历史或分支的 git 命令；DOMO 完成提交并重新验收；\n\
         - 无法完成时写清原因，不伪造成功；\n"
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
        "{}你当前扮演**执行者**，只负责下面任务。第 {} 次执行。\n\
         {}\
         \n## 任务\n{} - {}\n\n{}\n\
         \n## 人工指导\n{}\n\
         \n## 验收标准\n{}\n\
         \n## 前置任务结果\n{}\n\
         \n## 上一轮反馈（必须逐条处理）\n{}\n\
         {}\
         \n## 执行要求\n\
         - 由你决定实现与核验方式，满足任务与验收标准；\n\
         - 如实核验并说明结果，不伪造；\n\
         - 不要在工作区以外读写文件；\n\
         {git_rules}\
         - 仅当事实前提错误、无法按描述完成或约束冲突需规划者裁决时输出 replan；\n\
         - 不要判断其他任务或整个项目计划的进度；\n\
         - {}。\n\
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
    let (opening, workspace_note) = if plain_workspace(data) {
        (
            "你当前扮演**验收者**，在同一普通工作区核验执行结果；产物直接留在此目录，不存在分支或合入步骤。",
            "验收产物与交付物使用不同目录；",
        )
    } else {
        (
            "你当前扮演**验收者**，在任务分支的独立验收工作区核验执行结果。项目交付分支会在验收通过后由 DOMO 自动合并；合并冲突时任务会退回执行者解决并再次验收。不要因为任务改动尚未出现在项目交付分支而判定失败，也不要把“已提交到项目交付分支”当作验收前置条件。",
            "验收工作区在结束后会被删除；需要保留的验证报告写入作业目录；",
        )
    };
    format!(
        "{}{}\n\
         \n## 任务\n{} - {}\n\n{}\n\
         \n## 人工指导\n{}\n\
         \n## 验收标准\n{}\n\
         \n## 历史问题与拒绝意见\n{}\n\
         \n## 改动规模\n{}\n\
         \n## 验收要求\n\
         - 以任务描述和验收标准为唯一判据，由你决定核验方式，不扩大范围；\n\
         - 如实验证，不能只看执行者的自述，也不能伪造核验结果；\n\
         - 如果历史问题包含合并冲突，先确认执行者已解决冲突并消除冲突标记，再按任务验收标准复核；冲突处理本身不作为验收标准；\n\
         - 发现真实缺陷、验证失败、遗漏或伪造，判定不通过；\n\
         - {workspace_note}\n\
         {}\
         - 即使无法验收，也要写结果文件。\n\
         {}",
        common_header(data),
        opening,
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
        verification_rules(),
        result_rules(
            data,
            "{\"pass\":true,\"reason\":\"通过原因\",\"issues\":[]} 或 {\"pass\":false,\"reason\":\"拒绝原因\",\"issues\":[\"具体问题\"]}"
        )
    )
}

fn dialogue_history(data: &PromptData) -> String {
    let mut history = String::new();
    for message in &data.messages {
        if message.role != "user" {
            continue;
        }
        history.push_str(&format!("\n[用户]\n{}\n", message.content));
    }
    if history.is_empty() {
        history.push_str("\n（暂无对话）\n");
    }
    history
}

fn acceptance_criteria(data: &PromptData) -> String {
    if data.acceptance_criteria.is_empty() {
        "（项目方案未给出验收条目，请从项目目标和对话派生可核查的验收条目）".to_string()
    } else {
        data.acceptance_criteria
            .iter()
            .enumerate()
            .map(|(index, item)| format!("{}. {}", index + 1, item))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn acceptance_prompt(data: &PromptData) -> String {
    let tasks = data
        .completed_tasks
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
    let canceled = if data.canceled_tasks.is_empty() {
        "（没有被取消的任务）".to_string()
    } else {
        data.canceled_tasks
            .iter()
            .map(|task| format!("- {} {}：{}", task.key, task.title, task.summary))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let summary = data
        .project_summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("\n## 项目方案摘要\n{value}\n"))
        .unwrap_or_default();
    let assumptions = if data.project_assumptions.is_empty() {
        "（方案未记录关键假设）".to_string()
    } else {
        data.project_assumptions
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let replan = data
        .replan_reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|reason| format!("\n## 上次重规划原因（本次验收需核对是否已闭合）\n{reason}\n"))
        .unwrap_or_default();
    let (completion_note, workspace_label, evidence_rule, git_rule) = if plain_workspace(data) {
        (
            "当前计划的活动任务已经完成；你只做验收，不写项目报告。",
            "工作区改动规模",
            "逐条对照项目验收条目，以工作区的实际状态、可执行命令和可核查文件为准；任务结果摘要只作线索；",
            "不要修改源码、配置或文档，不要执行改变仓库状态的版本控制命令；",
        )
    } else {
        (
            "当前计划的活动任务已经完成并合入项目交付分支；你只做验收，不写项目报告。",
            "交付分支改动规模",
            "逐条对照项目验收条目，以交付分支的实际状态、可执行命令和可核查文件为准；任务结果摘要只作线索；",
            "不要修改源码、配置或文档，不要执行 git add、git commit、git merge、git rebase、git push 等改变暂存区、历史或分支的命令；",
        )
    };
    format!(
        "{}你当前扮演**规划者**，负责项目整体验收。{completion_note}\n\
         {}\
         \n## 项目验收条目\n{}\
         \n## 关键假设与范围\n{}\
         \n## 已完成任务\n{}\
         \n## 已取消任务\n{}\
         \n## {workspace_label}\n{}\
         \n## 项目对话\n{}\
         {}\
         \n## 验收要求\n\
         - {evidence_rule}\n\
         {}\
         - {git_rule}\n\
         - 每条验收条目给出 verdict：met（有可核查证据）、unmet（代理本应完成但未完成）、unverifiable（代理应能验证但当前无法验证）、human_required（只能由人完成的签字、试听、实机对照等）；\n\
         - 方案明确允许以 UNKNOWN 收口的条目，判为 met，并在 evidence 中引用允许该口径的方案条文；不为通过验收而放宽标准；\n\
         - human_required 不是重规划理由，会作为人工待办写入项目报告；只有 unmet 和 unverifiable 需要重新规划；\n\
         - 证据必须具体，写清命令与退出码、文件路径、提交号或作业 id，不要只复述任务摘要；\n\
         - 存在 unmet 或 unverifiable 时，在 replan_reason 中列出需要规划者补齐的工作；否则留空；\n\
         - 不要写项目报告。\n\
         {}",
        common_header(data),
        summary,
        acceptance_criteria(data),
        assumptions,
        tasks,
        canceled,
        if data.diff_stat.is_empty() {
            "（没有检测到改动）"
        } else {
            &data.diff_stat
        },
        dialogue_history(data),
        replan,
        verification_rules(),
        result_rules(
            data,
            "{\"kind\":\"project_acceptance\",\"summary\":\"结论摘要\",\"criteria\":[{\"id\":\"A1\",\"source\":\"proposal_acceptance\",\"criterion\":\"项目验收条目原文\",\"verdict\":\"met|unmet|unverifiable|human_required\",\"evidence\":[\"command: 命令 -> 退出码\",\"file: 路径 -> 结论\"],\"gap\":\"未满足或无法验证的原因\"}],\"replan_reason\":\"存在 unmet 或 unverifiable 时的重规划原因，否则为空\"}"
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
        String::new()
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
    let acceptance = data
        .acceptance_result
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|result| {
            format!("\n## 项目验收结论（已通过，报告必须与此一致）\n```json\n{result}\n```\n")
        })
        .unwrap_or_default();
    let (completion_note, workspace_label) = if plain_workspace(data) {
        ("项目整体验收已经通过，所有任务已经完成。", "工作区改动规模")
    } else {
        (
            "项目整体验收已经通过，所有任务已经完成并合并到项目交付分支。",
            "交付分支改动规模",
        )
    };
    format!(
        "{}你当前扮演**规划者**，负责撰写项目总结报告。{completion_note}\n\
         \n## 任务结果\n{}\n\
         \n## {workspace_label}\n{}\n\
         {}{}\n\
         ## 报告要求\n\
         - 使用 Markdown，面向用户说明最终交付了什么、关键思路、验证情况和已知风险；\n\
         - 报告必须与项目验收结论一致；verdict 为 human_required 的条目作为人工待办列出；\n\
         - 如实验证并说明依据，不要编造没有发生的验证、测试或产物；\n\
         - 报告要具体、可核查；\n\
         - 不要在工作区以外读写文件，也不要修改任何代码或文件。\n\
         {}",
        common_header(data),
        tasks,
        if data.diff_stat.is_empty() {
            "（没有检测到改动）"
        } else {
            &data.diff_stat
        },
        acceptance,
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
            workspace_type: crate::models::WORKSPACE_GIT.into(),
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
            acceptance_criteria: vec!["项目验收条目".into()],
            acceptance_result: None,
            project_summary: Some("项目方案摘要".into()),
            project_assumptions: vec!["关键假设".into()],
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
            JOB_ROLE_ACCEPTANCE => "\"kind\":\"project_acceptance\"",
            other => panic!("未知的作业角色: {other}"),
        }
    }

    #[test]
    fn continuation_only_asks_to_continue_and_offers_derivation() {
        let prompt = continuation(&sample_data(JOB_ROLE_EXECUTOR));
        assert!(prompt.contains("继续"));
        assert!(prompt.contains("派生子任务"));
        assert!(prompt.contains(GUIDANCE));
        assert!(!prompt.contains(RESULT_PATH));

        let verifier = continuation(&sample_data(JOB_ROLE_VERIFIER));
        assert!(verifier.contains("继续"));
        assert!(!verifier.contains("派生子任务"));
    }

    #[test]
    fn every_role_includes_result_path_and_schema() {
        for role in [
            JOB_ROLE_ANALYSIS,
            JOB_ROLE_DIRECT,
            JOB_ROLE_EXECUTOR,
            JOB_ROLE_VERIFIER,
            JOB_ROLE_ACCEPTANCE,
            JOB_ROLE_SUMMARY,
        ] {
            let prompt = render_role(role);
            assert!(prompt.contains(RESULT_PATH), "{role} 缺少结果文件路径");
            assert!(
                prompt.contains(result_schema_text(role)),
                "{role} 缺少输出 schema"
            );
            assert!(prompt.contains("长命令进度"), "{role} 缺少长命令进度说明");
            assert!(
                prompt.contains("最少每隔5分钟要有一次内容输出，否则会被判定为卡死而被关闭"),
                "{role} 缺少卡死判定说明"
            );
            assert!(
                !prompt.contains("连续 30 分钟没有 stdout/stderr 输出"),
                "{role} 的长命令口径不符合要求"
            );
        }
    }

    #[test]
    fn long_command_rules_are_brief() {
        let rules = long_command_rules();
        let lines = rules.lines().filter(|line| !line.trim().is_empty()).count();
        assert!(lines <= 3, "长命令说明不应超过 3 行，实际 {lines} 行");
        assert!(
            rules.contains("最少每隔5分钟要有一次内容输出，否则会被判定为卡死而被关闭"),
            "缺少卡死判定口径"
        );
        for forbidden in ["Start-Process", "nohup", "sleep 120", "tail -n 5"] {
            assert!(
                !rules.contains(forbidden),
                "长命令说明不应包含示例: {forbidden}"
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
        assert!(
            prompt.contains("不得降低项目级验收口径"),
            "缺少重规划验收口径约束"
        );

        let mut child = sample_data(JOB_ROLE_EXECUTOR);
        child.derived_from = Some(DependencyInfo {
            key: "T1".into(),
            title: "父任务".into(),
            summary: "进行中".into(),
        });
        let child_prompt = render(&child);
        assert!(child_prompt.contains("## 派生任务约束"), "缺少派生约束标题");
        assert!(child_prompt.contains("接受独立验收"), "缺少独立验收说明");
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
        let executor = render_role(JOB_ROLE_EXECUTOR);
        assert!(
            executor.contains("不要判断其他任务或整个项目计划的进度"),
            "执行者不应承担全局进度判断"
        );
        assert!(
            !executor.contains("本批还有任务可以继续执行"),
            "执行者 replan 条件不应要求全局批次信息"
        );

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
    fn verification_permissions_allow_reports_and_protect_deliverables() {
        for role in [JOB_ROLE_VERIFIER, JOB_ROLE_ACCEPTANCE] {
            for workspace_type in ["git", crate::models::WORKSPACE_PLAIN] {
                let mut data = sample_data(role);
                data.workspace_type = workspace_type.into();
                let criterion = "运行表格核对工具 --out var/acceptance，核对退出码与报告";
                data.task.as_mut().unwrap().acceptance = vec![criterion.into()];
                data.acceptance_criteria = vec![criterion.into()];
                let prompt = render(&data);
                assert!(prompt.contains(criterion));
                assert!(prompt.contains("可以创建缓存、日志、临时数据和报告"));
                assert!(prompt.contains("Git 工作区使用已忽略目录"));
                assert!(prompt.contains("普通工作区使用独立验收产物目录"));
                assert!(prompt.contains("或作业目录"));
                assert!(prompt.contains("不要修改交付源码、配置、文档、输入资源或现有交付物"));
                assert!(prompt.contains("也不要修改忽略规则"));
                assert!(prompt.contains("不得执行 git add、git commit"));
                assert!(prompt.contains("不要在工作区和作业目录以外读写文件"));
                assert!(prompt.contains(&data.result_path));
                for conflict in [
                    "不要修改任何文件",
                    "不要修改结果文件以外的任何文件",
                    "独立只读工作区",
                    "运行只读验证命令",
                ] {
                    assert!(
                        !prompt.contains(conflict),
                        "{role}/{workspace_type}: {conflict}"
                    );
                }
            }
        }
    }

    #[test]
    fn each_role_states_readonly_and_commit_boundaries() {
        for role in [
            JOB_ROLE_ANALYSIS,
            JOB_ROLE_DIRECT,
            JOB_ROLE_EXECUTOR,
            JOB_ROLE_VERIFIER,
            JOB_ROLE_ACCEPTANCE,
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
    fn acceptance_prompt_lists_criteria_tasks_and_command_permission() {
        let mut data = sample_data(JOB_ROLE_ACCEPTANCE);
        data.acceptance_criteria = vec!["产物可以运行".into(), "记录完整".into()];
        data.completed_tasks = vec![SummaryTaskInfo {
            key: "T1".into(),
            title: "实现产物".into(),
            summary: "已生成产物".into(),
        }];
        data.diff_stat = "1 file changed".into();
        let prompt = render(&data);
        assert!(prompt.contains("产物可以运行"), "缺少项目验收条目");
        assert!(prompt.contains("项目方案摘要"), "缺少项目方案摘要");
        assert!(prompt.contains("关键假设"), "缺少关键假设");
        assert!(prompt.contains("实现产物"), "缺少已完成任务");
        assert!(prompt.contains("1 file changed"), "缺少改动规模");
        assert!(
            prompt.contains("可以运行测试、探针、构建和检查器"),
            "缺少验证命令权限"
        );
        assert!(prompt.contains("human_required"), "缺少人工待办口径");
        assert!(prompt.contains("不要写项目报告"), "缺少不写报告约束");
    }

    #[test]
    fn summary_prompt_includes_acceptance_result() {
        let mut data = sample_data(JOB_ROLE_SUMMARY);
        data.acceptance_result = Some(
            r#"{"kind":"project_acceptance","criteria":[{"id":"A1","verdict":"met"}]}"#.into(),
        );
        let prompt = render(&data);
        assert!(prompt.contains("项目验收结论"), "缺少验收结论标题");
        assert!(prompt.contains("\"verdict\":\"met\""), "缺少验收结论内容");
        assert!(
            prompt.contains("报告必须与项目验收结论一致"),
            "缺少一致性约束"
        );
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
    fn executor_and_verifier_explain_merge_conflict_handoff() {
        let executor = render_role(JOB_ROLE_EXECUTOR);
        assert!(
            executor.contains("合并冲突状态"),
            "执行者缺少合并冲突处理说明"
        );
        assert!(
            executor.contains("不要执行会改变暂存区、历史或分支的 git 命令"),
            "执行者缺少冲突处理时的 Git 边界"
        );
        let verifier = render_role(JOB_ROLE_VERIFIER);
        assert!(
            verifier.contains("合并冲突时任务会退回执行者解决并再次验收"),
            "验收者缺少合并冲突流程说明"
        );
        assert!(
            verifier.contains("历史问题与拒绝意见"),
            "验收者缺少历史问题标题"
        );
        let analysis = render_role(JOB_ROLE_ANALYSIS);
        assert!(
            analysis.contains("合并冲突时退回执行者解决并重新验收"),
            "规划者缺少合并冲突流程说明"
        );
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
