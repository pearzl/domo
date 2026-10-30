import { useEffect, useState } from "react";
import { api, subscribeEvents } from "./api";
import type { Health } from "./types";
import { ProjectsPage } from "./pages/ProjectsPage";
import { ProjectPage } from "./pages/ProjectPage";
import { AgentsPage } from "./pages/AgentsPage";
import { UsagePage } from "./pages/UsagePage";

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

  useEffect(() => {
    const canonical = normalizePath(window.location.pathname);
    if (window.location.pathname !== canonical) {
      window.history.replaceState(window.history.state, "", canonical);
    }
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

  function navigate(next: string) {
    const target = normalizePath(next);
    window.history.pushState({}, "", target);
    setPath(target);
    window.scrollTo(0, 0);
  }

  const projectMatch = path.match(/^\/projects\/([^/]+)$/);
  const isAgents = path === "/agents";
  const isUsage = path === "/usage";
  // 项目详情、根路径与未知路径都回退或落在项目视图，导航高亮“项目”。
  const isProjects = !isAgents && !isUsage;
  const online = healthOnline || eventsOnline;

  return (
    <div className="app">
      <aside className="sidebar">
        <button className="brand" onClick={() => navigate("/")} aria-label="DOMO 首页">
          <span className="brand-mark" aria-hidden="true">D</span>
          <span className="brand-text">DOMO</span>
        </button>

        {/* 窄屏下同一组导航按钮改为顶部横向排布，DOM 结构保持不变。 */}
        <nav className="nav-links sidebar-nav" aria-label="主导航">
          <button
            className={isProjects ? "nav-link active" : "nav-link"}
            onClick={() => navigate("/")}
          >
            项目
          </button>
          <button
            className={isAgents ? "nav-link active" : "nav-link"}
            onClick={() => navigate("/agents")}
          >
            Agent 团队
          </button>
          <button
            className={isUsage ? "nav-link active" : "nav-link"}
            onClick={() => navigate("/usage")}
          >
            用量
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
            {online ? "服务器在线" : "服务器断开"}
          </div>
        </div>
      </aside>

      <main className="content">
        {projectMatch ? (
          <ProjectPage
            id={projectMatch[1]}
            onBack={() => navigate("/")}
          />
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
