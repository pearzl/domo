import { useEffect, useState } from "react";
import { api, subscribeEvents } from "./api";
import type { Health } from "./types";
import { browserDraftStorage, pruneDrafts } from "./presentation/draft";
import { ProjectsPage } from "./pages/ProjectsPage";
import { ProjectPage } from "./pages/ProjectPage";
import { AgentsPage } from "./pages/AgentsPage";
import { UsagePage } from "./pages/UsagePage";
import { AttentionPage } from "./pages/AttentionPage";
import { Icon } from "./ui";

const HEALTH_POLL_MS = 5000;

// 路由以无尾斜杠的形式比较与呈现；根路径保持为 "/"。
function normalizePath(raw: string): string {
  const trimmed = raw.replace(/\/+$/, "");
  return trimmed === "" ? "/" : trimmed;
}

export default function App() {
  const [path, setPath] = useState(() => normalizePath(window.location.pathname));
  const [health, setHealth] = useState<Health | null>(null);
  const [healthOnline, setHealthOnline] = useState(false);
  const [eventsOnline, setEventsOnline] = useState(false);
  const [attentionCount, setAttentionCount] = useState(0);

  useEffect(() => {
    const canonical = normalizePath(window.location.pathname);
    if (window.location.pathname !== canonical) {
      window.history.replaceState(window.history.state, "", `${canonical}${window.location.search}${window.location.hash}`);
    }
  }, []);

  useEffect(() => {
    pruneDrafts(browserDraftStorage());
  }, []);

  useEffect(() => {
    const handlePop = () => setPath(normalizePath(window.location.pathname));
    window.addEventListener("popstate", handlePop);
    return () => window.removeEventListener("popstate", handlePop);
  }, []);

  useEffect(() => {
    let active = true;

    async function checkHealth() {
      try {
        const next = await api.health();
        if (!active) return;
        setHealth(next);
        setHealthOnline(next.ok);
      } catch {
        if (active) setHealthOnline(false);
      }
    }

    void checkHealth();
    const timer = window.setInterval(() => void checkHealth(), HEALTH_POLL_MS);
    const unsubscribe = subscribeEvents(
      () => undefined,
      (connected) => {
        if (active) setEventsOnline(connected);
      },
    );

    return () => {
      active = false;
      window.clearInterval(timer);
      unsubscribe();
    };
  }, []);

  useEffect(() => {
    let active = true;
    async function refreshAttention() {
      try {
        const projects = await api.listProjects();
        if (active) {
          setAttentionCount(projects.reduce((count, project) => count + project.attention_items.length, 0));
        }
      } catch {
        // 列表页与待我处理页各自展示加载错误，导航计数保持上次值。
      }
    }
    void refreshAttention();
    const unsubscribe = subscribeEvents(() => void refreshAttention());
    const timer = window.setInterval(() => void refreshAttention(), HEALTH_POLL_MS);
    return () => {
      active = false;
      window.clearInterval(timer);
      unsubscribe();
    };
  }, []);

  function navigate(next: string) {
    const target = new URL(next, window.location.href);
    const pathname = normalizePath(target.pathname);
    window.history.pushState({}, "", `${pathname}${target.search}${target.hash}`);
    setPath(pathname);
    window.scrollTo(0, 0);
  }

  const projectMatch = path.match(/^\/projects\/([^/]+)$/);
  const isAgents = path === "/agents";
  const isUsage = path === "/usage";
  const isAttention = path === "/attention";
  // 项目详情、根路径与未知路径都回退或落在项目视图，导航高亮“项目”。
  const isProjects = !isAgents && !isUsage && !isAttention;
  const online = healthOnline || eventsOnline;

  return (
    <div className="app">
      <a className="skip-link" href="#main-content">跳到主要内容</a>
      <aside className="sidebar">
        <button className="brand" onClick={() => navigate("/")} aria-label="DOMO 首页">
          <span className="brand-mark" aria-hidden="true">D</span>
          <span className="brand-text">DOMO<span className="brand-caption">个人 AI 交付工作台</span></span>
        </button>

        <div className="nav-section-label">工作空间</div>

        {/* 导航在窄屏顶部横向排布，焦点顺序与桌面一致。 */}
        <nav className="nav-links sidebar-nav" aria-label="主导航">
          <button
            className={isProjects ? "nav-link active" : "nav-link"}
            aria-current={isProjects ? "page" : undefined}
            onClick={() => navigate("/")}
          >
            <Icon name="projects" />项目
          </button>
          <button
            className={isAttention ? "nav-link active" : "nav-link"}
            aria-current={isAttention ? "page" : undefined}
            onClick={() => navigate("/attention")}
          >
            <Icon name="attention" />待我处理
            {attentionCount > 0 ? (
              <span className="nav-count">{attentionCount}</span>
            ) : null}
          </button>
          <button
            className={isAgents ? "nav-link active" : "nav-link"}
            aria-current={isAgents ? "page" : undefined}
            onClick={() => navigate("/agents")}
          >
            <Icon name="agents" />Agent 团队
          </button>
          <button
            className={isUsage ? "nav-link active" : "nav-link"}
            aria-current={isUsage ? "page" : undefined}
            onClick={() => navigate("/usage")}
          >
            <Icon name="usage" />成本用量
          </button>
        </nav>

        <div className="sidebar-footer">
          <div className="sidebar-meta" aria-label="版本与提交信息">
            <div className="sidebar-meta-row">
              <span>版本</span>
              <span className="mono">{health ? `v${health.version}` : "—"}</span>
            </div>
            <div className="sidebar-meta-row">
              <span>提交</span>
              <span className="mono" title={health ? health.git_hash : "提交信息暂不可用"}>
                {health ? health.git_hash : "—"}
              </span>
            </div>
          </div>
          <div
            className={online ? "connection connection-online" : "connection connection-offline"}
            role="status"
            aria-live="polite"
          >
            <span className="connection-dot" aria-hidden="true" />
            <span>{online ? "服务器在线" : "服务器断开"}</span>
          </div>
        </div>
      </aside>

      <main className="content" id="main-content" tabIndex={-1}>
        {projectMatch ? (
          <ProjectPage
            key={projectMatch[1]}
            id={projectMatch[1]}
            onBack={() => navigate("/")}
          />
        ) : isAttention ? (
          <AttentionPage onOpen={(id, taskId) => navigate(`/projects/${id}${taskId ? `?tab=tasks&task=${encodeURIComponent(taskId)}` : ""}`)} />
        ) : isAgents ? (
          <AgentsPage />
        ) : isUsage ? (
          <UsagePage />
        ) : (
          <ProjectsPage onOpen={(id) => navigate(`/projects/${id}`)} />
        )}
      </main>
    </div>
  );
}
