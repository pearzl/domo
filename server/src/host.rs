//! 执行宿主（`opencode serve`）管理。
//!
//! 模型（ARCH §6）：**每个工作区（cwd）一个宿主**，而不是每个任务一个。
//! - 单仓：每个任务有自己的 worktree → 天然一任务一宿主，可并行；
//! - 多仓：项目/任务都在工程目录内串行 → 共用该目录的宿主，切分支时销毁重建；
//! - 同一任务的执行、验收与重试复用同一宿主；worktree 回收时一并销毁。
//!
//! - 宿主以独立进程组启动，终止按组回收（`TERM → 3s → KILL`）。
//! - 宿主数量有上限（`DOMO_MAX_HOSTS`，默认 4）；超出时调用方退化为
//!   `opencode run --standalone`（一进程一会话）。
//! - 健康探测：等端口可连接即视为就绪；HTTP 调用统一走 `curl`（本地、无额外依赖），
//!   后续若需要流式事件再换原生客户端。
//! - 本模块只负责进程与端口；会话语义由适配器决定。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub struct Host {
    /// 键 = 规范化后的工作区绝对路径
    pub key: String,
    pub port: u16,
    pub pid: u32,
    pub cwd: PathBuf,
    pub last_used: Instant,
}

fn key_of(cwd: &Path) -> String {
    cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()).display().to_string()
}

static HOSTS: OnceLock<Mutex<HashMap<String, Host>>> = OnceLock::new();
/// 宿主 API 的 basic-auth 密码：启动宿主时由 DOMO 生成并注入，
/// 兼容 opencode v2（`/api/*` + `opencode:<password>`）。
fn password() -> &'static str {
    static PWD: OnceLock<String> = OnceLock::new();
    PWD.get_or_init(|| {
        std::env::var("DOMO_OC_PASSWORD")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("domo-{}", uuid::Uuid::new_v4().simple()))
    })
}

fn hosts() -> &'static Mutex<HashMap<String, Host>> {
    HOSTS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn max_hosts() -> usize {
    std::env::var("DOMO_MAX_HOSTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(4)
}

/// 是否启用 HTTP 宿主模式（默认关闭：无宿主时仍走 `opencode run --standalone`）。
pub fn enabled() -> bool {
    std::env::var("DOMO_OC_HTTP").map(|v| v == "1" || v == "true").unwrap_or(false)
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn kill_group(pid: u32, signal: &str) {
    let _ = Command::new("kill")
        .args([signal, &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn free_port() -> Result<u16, String> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind port: {e}"))?;
    let port = l.local_addr().map_err(|e| format!("local_addr: {e}"))?.port();
    drop(l);
    Ok(port)
}

fn port_ready(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_millis(300),
    )
    .is_ok()
}

async fn wait_ready(port: u16, tries: usize) -> bool {
    for _ in 0..tries {
        if port_ready(port) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

/// 取得（必要时启动）该工作区的宿主，返回端口。
pub async fn ensure(cwd: &Path) -> Result<u16, String> {
    let key = key_of(cwd);
    {
        let mut map = hosts().lock().unwrap();
        if let Some(h) = map.get_mut(&key) {
            if alive(h.pid) {
                h.last_used = Instant::now();
                return Ok(h.port);
            }
            let old = map.remove(&key).unwrap();
            kill_group(old.pid, "-KILL");
        }
        if map.len() >= max_hosts() {
            return Err(format!("执行宿主已达上限（{}），回退到独立进程模式", max_hosts()));
        }
    }

    let port = free_port()?;
    let mut cmd = Command::new("opencode");
    cmd.args(["serve", "--hostname", "127.0.0.1", "--port", &port.to_string()])
        .env("OPENCODE_SERVER_PASSWORD", password())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn().map_err(|e| format!("spawn opencode serve: {e}"))?;
    let pid = child.id();

    if !wait_ready(port, 40).await {
        kill_group(pid, "-KILL");
        return Err("opencode serve 未在 10s 内就绪".into());
    }
    // 能力探测：v2 的 `/api/project` 可读即视为可用；不同版本健康端点不一致，故不依赖它
    let healthy = curl(port, "GET", "/project", None, 5).is_ok();
    if !healthy {
        kill_group(pid, "-KILL");
        return Err("opencode serve 健康检查失败（/api/project），回退独立进程模式".into());
    }

    let mut map = hosts().lock().unwrap();
    map.insert(
        key.clone(),
        Host { key, port, pid, cwd: cwd.to_path_buf(), last_used: Instant::now() },
    );
    Ok(port)
}

/// 终止并移除某工作区的宿主（工作区回收 / 切分支前调用）。
pub fn stop(cwd: &Path) {
    let h = hosts().lock().unwrap().remove(&key_of(cwd));
    if let Some(h) = h {
        kill_group(h.pid, "-TERM");
        std::thread::sleep(Duration::from_millis(300));
        kill_group(h.pid, "-KILL");
    }
}

/// 回收空闲超过 `idle` 的宿主（兜底巡检调用）。
pub fn sweep(idle: Duration) -> usize {
    let mut removed = Vec::new();
    {
        let mut map = hosts().lock().unwrap();
        let stale: Vec<String> = map
            .values()
            .filter(|h| h.last_used.elapsed() > idle || !alive(h.pid))
            .map(|h| h.key.clone())
            .collect();
        for id in stale {
            if let Some(h) = map.remove(&id) {
                removed.push(h);
            }
        }
    }
    for h in &removed {
        kill_group(h.pid, "-KILL");
    }
    removed.len()
}

pub fn stop_all() {
    let all: Vec<Host> = hosts().lock().unwrap().drain().map(|(_, v)| v).collect();
    for h in all {
        kill_group(h.pid, "-KILL");
    }
}

pub fn touch(cwd: &Path) {
    if let Some(h) = hosts().lock().unwrap().get_mut(&key_of(cwd)) {
        h.last_used = Instant::now();
    }
}

/// 简单 HTTP 调用（本地回环；`curl` 避免引入额外依赖）。
fn curl(port: u16, method: &str, path: &str, body: Option<&Value>, timeout_secs: u64) -> Result<Value, String> {
    let url = format!("http://127.0.0.1:{port}/api{path}");
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-X", method, "-H", "content-type: application/json"])
        .args(["-u", &format!("opencode:{}", password())]);
    cmd.args(["--max-time", &timeout_secs.to_string()]);
    if let Some(b) = body {
        cmd.args(["--data-binary", &b.to_string()]);
    }
    cmd.arg(&url);
    let out = cmd.output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("HTTP {method} {path} failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text).map_err(|e| format!("HTTP {method} {path} returned non-JSON: {e}"))
}

/// 创建会话，返回真实 session id。
pub fn create_session(port: u16, title: &str) -> Result<String, String> {
    let v = curl(port, "POST", "/session", Some(&json!({"title": title})), 30)?;
    // v2：{"data":{"id":...}}；兼容旧版顶层 {"id":...}
    let id = v
        .get("data")
        .and_then(|d| d.get("id"))
        .or_else(|| v.get("id"))
        .and_then(|x| x.as_str())
        .map(String::from);
    id.ok_or_else(|| format!("session response has no id: {v}"))
}

/// 发送消息并等待回复，返回（纯文本, 原始 JSON）。
pub fn send_message(port: u16, session: &str, prompt: &str, model: Option<&str>, timeout_secs: u64) -> Result<(String, Value), String> {
    let mut body = json!({ "parts": [{ "type": "text", "text": prompt }] });
    if let Some(m) = model {
        body["model"] = json!({ "providerID": m.split('/').next().unwrap_or(""), "modelID": m.split('/').nth(1).unwrap_or(m) });
    }
    let v = curl(port, "POST", &format!("/session/{session}/message"), Some(&body), timeout_secs)?;
    Ok((extract_text(&v), v))
}

/// 会话级取消。
pub fn abort_session(port: u16, session: &str) {
    let _ = curl(port, "POST", &format!("/session/{session}/abort"), Some(&json!({})), 10);
}

fn extract_text(v: &Value) -> String {
    let mut out = String::new();
    // v2：{"data":{info,parts}}；兼容旧版顶层 parts
    let v = v.get("data").unwrap_or(v);
    if let Some(parts) = v.get("parts").and_then(|p| p.as_array()) {
        for p in parts {
            if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(t);
                }
            }
        }
    }
    if out.is_empty() {
        if let Some(t) = v.get("info").and_then(|i| i.get("text")).and_then(|t| t.as_str()) {
            out.push_str(t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_port_is_bindable() {
        let p = free_port().unwrap();
        let l = std::net::TcpListener::bind(("127.0.0.1", p)).unwrap();
        drop(l);
    }

    #[test]
    fn map_insert_and_stop_is_idempotent() {
        stop(Path::new("/no/such/workspace")); // 不应 panic
        assert!(hosts().lock().unwrap().get("/no/such/workspace").is_none());
    }

    #[test]
    fn extract_text_reads_parts() {
        let v = json!({"parts":[{"type":"text","text":"a"},{"type":"tool"},{"type":"text","text":"b"}]});
        assert_eq!(extract_text(&v), "a\nb");
    }
}
