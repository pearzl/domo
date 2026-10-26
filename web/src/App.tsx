import { useEffect, useState } from "react";
import { ProjectsPage } from "./pages/ProjectsPage";
import { ProjectPage } from "./pages/ProjectPage";
import { AgentsPage } from "./pages/AgentsPage";

export default function App() {
  const [path, setPath] = useState(window.location.pathname);

  useEffect(() => {
    const handlePop = () => setPath(window.location.pathname);
    window.addEventListener("popstate", handlePop);
    return () => window.removeEventListener("popstate", handlePop);
  }, []);

  function navigate(next: string) {
    window.history.pushState({}, "", next);
    setPath(next);
  }

  const projectMatch = path.match(/^\/projects\/([^/]+)$/);
  const isAgents = path === "/agents";

  return (
    <div className="app">
      <nav className="topbar">
        <button className="brand" onClick={() => navigate("/")}>
          <span className="brand-mark">D</span>
          <span>DOMO</span>
        </button>
        <div className="nav-links">
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
        </div>
      </nav>

      {projectMatch ? (
        <ProjectPage
          id={projectMatch[1]}
          onBack={() => navigate("/")}
        />
      ) : isAgents ? (
        <AgentsPage />
      ) : (
        <ProjectsPage onOpen={(id) => navigate(`/projects/${id}`)} />
      )}
    </div>
  );
}
