import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import type { Task } from "../types";
import {
  QueueControls,
  TaskFormCard,
  TaskList,
  TaskPanel,
  TaskPanelActions,
  TaskRow,
} from "./TaskBoard";

function task(key: string, deps: string[] = []): Task {
  return {
    id: `id-${key}`,
    project_id: "p1",
    seq: 1,
    queue_order: 1,
    queue_position: null,
    key,
    title: `任务 ${key}`,
    description: "",
    acceptance: [],
    deps,
    parent_id: null,
    milestone: false,
    human_gate: null,
    forced_agent_id: null,
    forced_agent_role: null,
    stage: "pending",
    status: null,
    agent_id: null,
    last_agent_id: null,
    worktree: null,
    branch: null,
    artifacts_released_at: null,
    attempts: 0,
    rejections: 0,
    last_error: null,
    last_failure_kind: null,
    waiting_reason: null,
    result_summary: null,
    verifier_issues: [],
    guidance: "",
    created_at: "2026-01-10T10:00:00Z",
    updated_at: "2026-01-10T10:00:00Z",
  };
}

const TASKS: Task[] = [task("A"), task("B", ["A"])];

/// 从标记里截取给定类名容器的完整开闭标签片段（按同名标签配对）。
function elementMarkup(
  markup: string,
  tag: string,
  className: string,
  from?: number,
): string {
  const needle = `<${tag} class="${className}"`;
  const start =
    from === undefined ? markup.indexOf(needle) : markup.lastIndexOf(needle, from);
  expect(start).toBeGreaterThanOrEqual(0);
  const closeTag = `</${tag}>`;
  let depth = 0;
  let index = start;
  while (index < markup.length) {
    const nextOpen = markup.indexOf(`<${tag}`, index + 1);
    const nextClose = markup.indexOf(closeTag, index + 1);
    if (nextClose < 0) throw new Error(`未找到 ${className} 的闭合标签`);
    if (nextOpen >= 0 && nextOpen < nextClose) {
      depth += 1;
      index = nextOpen;
      continue;
    }
    if (depth === 0) return markup.slice(start, nextClose + closeTag.length);
    depth -= 1;
    index = nextClose;
  }
  throw new Error(`未找到 ${className} 的闭合标签`);
}

describe("任务面板表单与列表是平级容器", () => {
  function renderPanel(addTaskOpen: boolean) {
    return renderToStaticMarkup(
      <TaskPanel
        title={<h2>任务</h2>}
        actions={<TaskPanelActions><button>添加任务</button></TaskPanelActions>}
        form={
          addTaskOpen ? (
            <TaskFormCard>
              <form className="form add-task-form">表单</form>
            </TaskFormCard>
          ) : null
        }
        list={
          <TaskList
            tasks={TASKS}
            activeTasks={TASKS}
            selectedTaskId={null}
            onSelect={() => {}}
            agentName={() => "—"}
            queueIds={[]}
            archived={false}
            busy={false}
            onReorder={() => {}}
          />
        }
      />,
    );
  }

  it("打开表单后表单容器与列表容器是带各自类名的兄弟节点", () => {
    const markup = renderPanel(true);
    const card = elementMarkup(markup, "div", "add-task-card");
    const list = elementMarkup(markup, "div", "task-list");

    expect(card).toContain("add-task-form");
    expect(list).toContain("task-row");

    // 互不嵌套：两个容器各自完整，且不包含对方的开始标签。
    expect(card).not.toContain('class="task-list"');
    expect(list).not.toContain('class="add-task-card"');

    // 平级：表单卡片出现在列表之前，二者位于同一父节点中（panel 内）。
    expect(markup.indexOf('class="add-task-card"')).toBeLessThan(
      markup.indexOf('class="task-list"'),
    );
    const panel = elementMarkup(markup, "section", "panel task-panel");
    expect(panel).toContain('class="add-task-card"');
    expect(panel).toContain('class="task-list"');
  });

  it("表单未打开时不渲染表单容器，列表仍在面板内", () => {
    const markup = renderPanel(false);
    expect(markup).not.toContain("add-task-card");
    expect(markup).toContain('class="task-list"');
  });
});

describe("任务行层级", () => {
  it("首行包含 key、标题与状态徽标，依赖与 Agent 在第二行", () => {
    const markup = renderToStaticMarkup(
      <TaskList
        tasks={TASKS}
        activeTasks={TASKS}
        selectedTaskId="id-B"
        onSelect={() => {}}
        agentName={() => "Agent 甲"}
        queueIds={[]}
        archived={false}
        busy={false}
        onReorder={() => {}}
      />,
    );
    // 第二个任务行（B），依赖信息应落在第二行；文本与相邻标签之间可能有注释分隔。
    const secondRow = markup.indexOf("任务 B</span>");
    const main = elementMarkup(
      markup,
      "div",
      "task-row-main",
      markup.indexOf(">", secondRow),
    );
    expect(main).toContain('class="task-key"');
    expect(main).toContain("任务 B");
    expect(main).toContain('class="task-state"');
    expect(main).not.toContain("依赖 A");
    expect(main).not.toContain("Agent 甲");

    expect(markup).toContain('class="task-row-meta"');
    const meta = elementMarkup(
      markup,
      "div",
      "task-row-meta",
      markup.lastIndexOf('class="task-row-meta"'),
    );
    expect(meta).toContain("依赖 A");
    expect(meta).toContain("Agent 甲");
  });

  it("任务行保留可调整队列的重排入口", () => {
    const markup = renderToStaticMarkup(
      <TaskRow
        task={{ ...task("C"), queue_position: 1 }}
        selected={false}
        agent="Agent 乙"
        counts="执行 1"
        onSelect={() => {}}
      >
        <QueueControls
          task={task("C")}
          summary={{
            position: 1,
            total: 2,
            label: "可调整 1/2",
            canMoveUp: false,
            canMoveDown: true,
          }}
          busy={false}
          onReorder={() => {}}
        />
      </TaskRow>,
    );
    expect(markup).toContain("队列 #1");
    expect(markup).toContain("可调整 1/2");
    expect(markup).toContain("C 排队下移");
  });
});

/// 样式约束测试读取源码原文；断言只针对本任务涉及的选择器。
const STYLES = readFileSync("src/styles.css", "utf8");

interface CssRule {
  /// 规则所在的 @media 条件，顶层规则为 null。
  media: string | null;
  selector: string;
  body: string;
}

/// 极简 CSS 规则扫描：按大括号层级收集选择器、声明块与所在媒体查询。
function parseRules(css: string): CssRule[] {
  const source = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const rules: CssRule[] = [];
  const stack: string[] = [];
  let buffer = "";
  let ruleHead = "";
  for (const char of source) {
    if (char === "{") {
      ruleHead = buffer.trim();
      stack.push(ruleHead);
      buffer = "";
    } else if (char === "}") {
      const closed = stack.pop() ?? "";
      if (closed === ruleHead && !ruleHead.startsWith("@media")) {
        const media = stack.find((item) => item.startsWith("@media")) ?? null;
        rules.push({
          media: media ? media.slice(6).trim().replace(/^\(|\)$/g, "") : null,
          selector: ruleHead,
          body: buffer,
        });
      }
      ruleHead = closed;
      buffer = "";
    } else {
      buffer += char;
    }
  }
  return rules;
}

const RULES = parseRules(STYLES);

/// 给定视口宽度下某选择器生效的声明合并结果（按源码顺序覆盖）。
function declarationsAt(selector: string, width: number): Record<string, string> {
  const result: Record<string, string> = {};
  for (const rule of RULES) {
    if (!rule.selector.split(",").map((item) => item.trim()).includes(selector)) {
      continue;
    }
    if (rule.media) {
      const match = /^max-width:\s*(\d+)px$/.exec(rule.media);
      if (!match) continue;
      if (width > Number(match[1])) continue;
    }
    for (const declaration of rule.body.split(";")) {
      const index = declaration.indexOf(":");
      if (index < 0) continue;
      result[declaration.slice(0, index).trim()] = declaration
        .slice(index + 1)
        .trim();
    }
  }
  return result;
}

function marginTopPx(selector: string, width: number): number {
  const value = declarationsAt(selector, width)["margin-top"] ?? "0";
  const match = /^(-?\d+(?:\.\d+)?)px$/.exec(value);
  expect(match, `${selector} 的 margin-top 应为具体像素值，实际为 ${value}`).not.toBeNull();
  return Number(match![1]);
}

describe("任务面板样式边界", () => {
  it("按钮与列表不再共用相邻边框（旧的贴合规则已删除）", () => {
    const removed = [
      // 表单自带边框与上下外边距，会与面板/列表边界贴合。
      ".add-task-form {\n  padding: 16px;\n  margin: 14px 0 18px;\n  border: 1px solid var(--line);",
      // 列表容器自带整圈边框，紧贴上方按钮。
      ".task-list {\n  display: flex;\n  flex-direction: column;\n  border: 1px solid var(--line);",
    ];
    for (const rule of removed) expect(STYLES).not.toContain(rule);
    // 列表行自带边框，容器不再画整圈边框。
    expect(declarationsAt(".task-list", 1180).border).toBeUndefined();
  });

  it("1180px 与 760px 两档宽度下按钮与列表之间都有可见间距", () => {
    for (const width of [1180, 760]) {
      const actions = declarationsAt(".task-panel-actions", width);
      const headMargin = declarationsAt(".task-panel .panel-head", width);
      const listGap = marginTopPx(".task-list", width);
      const cardMargin = declarationsAt(".add-task-card", width).margin ?? "0";
      expect(actions.border).toBeUndefined();
      expect(headMargin["row-gap"] ?? headMargin.gap ?? "0").not.toBe("0");
      expect(listGap).toBeGreaterThanOrEqual(8);
      expect(cardMargin).not.toBe("0");
    }
  });

  it("添加任务表单是完全展开的下拉卡片，与任务列表之间有明显分隔", () => {
    const card = declarationsAt(".add-task-card", 1180);
    const cardMargin = card.margin;
    expect(cardMargin).toBe("16px 0 20px");
    expect(card.border).toContain("1px solid");
    expect(card["box-shadow"]).toBeTruthy();
    // 表单本身不再带外框，卡片承担边框与间距。
    const form = declarationsAt(".add-task-form", 1180);
    expect(form.border).toBe("0");
    expect(form.margin).toBe("0");
  });

  it("添加任务卡片与任务行有进入/悬停过渡，并遵循减少动效偏好", () => {
    expect(declarationsAt(".add-task-card", 1180).animation).toContain("task-card-in");
    expect(declarationsAt(".task-row", 1180).transition).toContain("background");
    const reduced = STYLES.slice(
      STYLES.indexOf("@media (prefers-reduced-motion: reduce) {"),
    );
    expect(reduced).toContain(".add-task-card,");
    expect(reduced).toContain("animation: none;");
  });
});

describe("任务行窄屏布局", () => {
  it("窄屏下状态徽标仍在首行，依赖与 Agent 落到第二行", () => {
    // 840px 与 560px 两档都只调整行内两块的排布，不再把徽标挤到明细行。
    const narrow = RULES.filter(
      (rule) =>
        rule.media === "max-width: 840px" || rule.media === "max-width: 560px",
    );
    const rowGrid = narrow.filter((rule) => rule.selector === ".task-row");
    expect(rowGrid.some((rule) => /grid-template-columns/.test(rule.body))).toBe(
      false,
    );
    // 560px 档把 key 与状态徽标放在同一行的两个区域，标题另起一行。
    const areas = declarationsAt(".task-row-main", 560)["grid-template-areas"];
    expect(areas).toContain("key");
    expect(areas).toContain("state");
    expect(areas).toContain("title");
    // 依赖与 Agent 在标记上就位于第二行容器内。
    expect(declarationsAt(".task-row-meta", 560)["padding-left"]).toBe("0");
  });
});
