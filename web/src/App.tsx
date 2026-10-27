import { useEffect, useState } from "react";
import { api, subscribeEvents } from "./api";
import type { Health } from "./types";
import { ProjectsPage } from "./pages/ProjectsPage";
import { ProjectPage } from "./pages/ProjectPage";
import { AgentsPage } from "./pages/AgentsPage";
import { UsagePage } from "./pages/UsagePage";

const HEALTH_POLL_MS = 5000;

export default function App() {
  const [path, setPath] = useState(window.location.pathname);
  const [health, setHealth] = useState<Health | null>(null);
  const [healthOnline, setHealthOnline] = useState(false);
  const [eventsOnline, setEventsOnline] = useState(false);

  useEffect(() => {
    const handlePop = () => setPath(window.location.pathname);
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
    window.history.pushState({}, "", next);
    setPath(next);
  }

  const projectMatch = path.match(/^\/projects\/([^/]+)$/);
  const isAgents = path === "/agents";
  const isUsage = path === "/usage";
  const online = healthOnline || eventsOnline;

  return (
    <div className="app">
      <aside className="sidebar">
        <button className="brand" onClick={() => navigate("/")}>
          <span className="brand-mark">D</span>
          <span>DOMO</span>
        </button>

        <nav className="nav-links sidebar-nav">
          <button
            className={path === "/" || projectMatch ? "nav-link active" : "nav-link"}
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
          <div className="sidebar-meta-row">
            <span>版本</span>
            <span className="mono">{health ? `v${health.version}` : "—"}</span>
          </div>
          <div className="sidebar-meta-row">
            <span>提交</span>
            <span className="mono">{health ? health.git_hash : "—"}</span>
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
