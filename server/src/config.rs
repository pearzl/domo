//! Process configuration. Values are resolved once at startup; runtime changes are not supported.

use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub data_root: PathBuf,
    pub workspace_root: PathBuf,
    pub log_root: PathBuf,
    pub web_dist: PathBuf,
    pub bind: String,
    pub port: u16,
    pub job_timeout: Duration,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let home = env::var("HOME").unwrap_or_else(|_| ".".into());
        let data_root = path_from_env("DOMO_DATA_ROOT", &home, ".domo/data");
        let workspace_root = path_from_env("DOMO_WORKSPACE_ROOT", &home, ".domo/workspace");
        let log_root = path_from_env("DOMO_LOG_ROOT", &home, ".domo/log");
        let port = env::var("DOMO_SERVER_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8787);
        let bind = env::var("DOMO_BIND").unwrap_or_else(|_| "127.0.0.1".into());
        let job_timeout = env::var("DOMO_JOB_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(30 * 60));
        let web_dist = env::var("DOMO_WEB_DIST")
            .map(PathBuf::from)
            .unwrap_or_else(|_| discover_web_dist());

        Ok(Self {
            data_root,
            workspace_root,
            log_root,
            web_dist,
            bind,
            port,
            job_timeout,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_root.join("domo.db")
    }
}

fn path_from_env(key: &str, home: &str, fallback: &str) -> PathBuf {
    env::var(key)
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(home).join(fallback))
        .components()
        .collect()
}

fn discover_web_dist() -> PathBuf {
    let candidates = [PathBuf::from("../web/dist"), PathBuf::from("web/dist")];
    for candidate in candidates {
        if candidate.join("index.html").is_file() {
            return candidate;
        }
    }
    PathBuf::from("../web/dist")
}
