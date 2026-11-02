//! Process supervision shared by agent kinds that run an external CLI.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use super::adapter::{AgentRun, AgentSpec, JobProgress};

const OUTPUT_CAP: usize = 2 * 1024 * 1024;

pub(super) async fn run_process(
    program: &str,
    args: &[String],
    spec: &AgentSpec,
    timeout: Duration,
    stdin: Option<Vec<u8>>,
) -> Result<AgentRun> {
    let mut command = crate::platform::command(program, args);
    command
        .current_dir(&spec.cwd)
        // CLIs may resolve their workspace from PWD; keep it aligned with cwd.
        .env("PWD", &spec.cwd)
        .env_remove("OLDPWD")
        .env("DOMO_JOB_ID", &spec.job_id)
        .env("DOMO_JOB_ROLE", &spec.role)
        .env("DOMO_JOB_DIR", &spec.job_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("启动 Agent 进程失败: {program}"))?;
    let pid = child.id();
    if let Some(pid) = pid {
        // Only record a pid that can be checked later. Without a start time a
        // recycled pid could make recovery kill an unrelated process.
        if let Some(start) = process_start_time(pid) {
            let _ = std::fs::write(spec.job_dir.join("pid"), format!("{pid}:{start}"));
        }
    }
    if let Some(input) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            tokio::spawn(async move {
                let _ = pipe.write_all(&input).await;
                let _ = pipe.shutdown().await;
            });
        }
    }
    let stdout = child.stdout.take().expect("stdout pipe");
    let stderr = child.stderr.take().expect("stderr pipe");
    let stdout_log = spec.job_dir.join("stdout.log");
    let stderr_log = spec.job_dir.join("stderr.log");
    let stdout_task = tokio::spawn(read_capped(stdout, spec.progress.clone(), true, stdout_log));
    let stderr_task = tokio::spawn(read_capped(
        stderr,
        spec.progress.clone(),
        false,
        stderr_log,
    ));

    let mut timed_out = false;
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => result?,
        Err(_) => {
            timed_out = true;
            if let Some(pid) = pid {
                kill_process_group(pid);
            }
            let _ = child.kill().await;
            child.wait().await?
        }
    };
    let stdout = stdout_task.await.unwrap_or_default();
    let stderr = stderr_task.await.unwrap_or_default();
    Ok(AgentRun {
        stdout,
        stderr,
        exit_code: status.code().map(|code| code as i64),
        timed_out,
        stalled: false,
        session_id: None,
        usage: None,
    })
}

async fn read_capped<R: AsyncRead + Unpin>(
    mut reader: R,
    progress: JobProgress,
    stdout: bool,
    log_path: PathBuf,
) -> String {
    let mut output = Vec::new();
    let mut written = 0usize;
    let mut buffer = [0u8; 8192];
    let mut log = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .await
        .ok();
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(n) => {
                if output.len() < OUTPUT_CAP {
                    let take = n.min(OUTPUT_CAP - output.len());
                    output.extend_from_slice(&buffer[..take]);
                }
                if let Some(log) = &mut log {
                    if written < OUTPUT_CAP {
                        let take = n.min(OUTPUT_CAP - written);
                        let _ = log.write_all(&buffer[..take]).await;
                        written += take;
                    }
                }
                if stdout {
                    progress.record_stdout(n);
                } else {
                    progress.record_stderr(n);
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&output).to_string()
}

pub fn kill_process_group(pid: u32) -> bool {
    #[cfg(unix)]
    unsafe {
        if libc::kill(-(pid as i32), libc::SIGKILL) == 0 {
            return true;
        }
        libc::kill(pid as i32, libc::SIGKILL) == 0
    }
    #[cfg(windows)]
    {
        // `child.kill()` only terminates the direct child. Agent CLIs spawn
        // their own tools, so the whole process tree has to be removed.
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// True while the process recorded for a job is still the same process.
pub fn job_process_alive(job_dir: &Path) -> bool {
    recorded_job_pid(job_dir).is_some_and(process_running)
}

/// Kills the process recorded for a job. Returns true only when the pid file
/// matched and the process was actually terminated, which lets the runner tell
/// a watchdog kill from a process that had already exited on its own.
pub fn kill_job_dir(job_dir: &Path) -> bool {
    let Some(pid) = recorded_job_pid(job_dir) else {
        return false;
    };
    kill_process_group(pid)
}

/// Reads the pid file and rejects a recycled pid by comparing the recorded
/// process start time.
fn recorded_job_pid(job_dir: &Path) -> Option<u32> {
    let path = job_dir.join("pid");
    let raw = std::fs::read_to_string(path).ok()?;
    let (pid_text, recorded_start) = match raw.trim().split_once(':') {
        Some((pid, start)) => (pid, Some(start)),
        None => (raw.trim(), None),
    };
    let pid = pid_text.parse::<u32>().ok()?;
    if let Some(recorded) = recorded_start {
        if !recorded.is_empty() && process_start_time(pid).as_deref() != Some(recorded) {
            return None;
        }
    }
    Some(pid)
}

#[cfg(not(windows))]
pub(super) fn process_start_time(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The executable name may contain spaces; fields after the closing parenthesis
    // are stable. Start time is field 22 overall, index 19 after the state field.
    let rest = stat.rsplit_once(") ").map(|(_, rest)| rest)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    fields.get(19).map(|value| value.to_string())
}

#[cfg(not(windows))]
fn process_running(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, rest)) = stat.rsplit_once(") ") else {
        return false;
    };
    !rest.starts_with('Z')
}

#[cfg(windows)]
pub(super) fn process_start_time(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        let ticks = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
        Some(ticks.to_string())
    }
}

#[cfg(windows)]
fn process_running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const STILL_ACTIVE: u32 = 259;

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::models::JOB_ROLE_EXECUTOR;

    #[cfg(windows)]
    #[test]
    fn windows_process_start_time_is_available() {
        let pid = std::process::id();
        assert!(process_start_time(pid).is_some());
    }

    #[test]
    fn kill_job_dir_ignores_a_reused_pid() {
        let dir = std::env::temp_dir().join(format!("domo-kill-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pid"),
            format!("{}:not-the-recorded-start-time", std::process::id()),
        )
        .unwrap();
        assert!(!kill_job_dir(&dir));
        assert!(!job_process_alive(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kill_job_dir_kills_a_verified_process() {
        let dir = std::env::temp_dir().join(format!("domo-kill-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping", "-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        #[cfg(unix)]
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let start = process_start_time(pid).expect("child start time");
        std::fs::write(dir.join("pid"), format!("{pid}:{start}")).unwrap();

        assert!(job_process_alive(&dir));
        assert!(kill_job_dir(&dir));
        let _ = child.wait();
        assert!(!job_process_alive(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawns_with_cwd_and_pwd_aligned() {
        let dir = std::env::temp_dir().join(format!("domo-pwd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = std::fs::canonicalize(&dir).unwrap();
        let spec = AgentSpec {
            job_id: "job_test".into(),
            role: JOB_ROLE_EXECUTOR.into(),
            cwd: cwd.clone(),
            job_dir: cwd.clone(),
            prompt: String::new(),
            original_prompt: String::new(),
            session_id: Some(crate::opencode::session_id("job_test")),
            continuation: false,
            progress: JobProgress::new(),
        };
        let args = vec![
            "-c".to_string(),
            "printf '%s\\n%s\\n' \"$PWD\" \"$(pwd)\"".to_string(),
        ];
        let run = run_process("sh", &args, &spec, Duration::from_secs(10), None)
            .await
            .unwrap();
        let expected = cwd.to_string_lossy();
        let mut lines = run.stdout.lines();
        assert_eq!(lines.next(), Some(expected.as_ref()));
        assert_eq!(lines.next(), Some(expected.as_ref()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
