//! Provider formats stop at the adapter boundary. Unknown evidence is useful
//! to a human, but never authorizes automatic resumption.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::AgentRun;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Completed,
    Timeout,
    Stalled,
    ProcessExit,
    ServiceInterrupted,
    UserStopped,
    LaunchError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Network,
    ProviderTransient,
    RateLimit,
    Authentication,
    Quota,
    Configuration,
    SessionUnavailable,
    ContextLimit,
    Cli,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    Supervisor,
    Structured,
    Text,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnosis {
    pub stop: StopReason,
    pub kind: ErrorKind,
    pub evidence: Evidence,
    pub status: Option<i64>,
    pub code: Option<String>,
    pub retry_after_ms: Option<i64>,
    pub message: String,
}

impl Diagnosis {
    /// Scheduling's coarse categories are separate from the detailed diagnosis.
    /// A provider quota failure is not evidence of a short-lived rate limit.
    pub fn failure_kind(&self) -> Option<crate::domain::FailureKind> {
        use crate::domain::FailureKind;
        if self.evidence != Evidence::Structured {
            return None;
        }
        match self.kind {
            ErrorKind::RateLimit => Some(FailureKind::RateLimit),
            ErrorKind::Network | ErrorKind::ProviderTransient | ErrorKind::Cli => {
                Some(FailureKind::Transient)
            }
            ErrorKind::Authentication
            | ErrorKind::Quota
            | ErrorKind::Configuration
            | ErrorKind::SessionUnavailable
            | ErrorKind::ContextLimit => Some(FailureKind::Permanent),
            ErrorKind::Unknown => None,
        }
    }
    pub fn stopped(stop: StopReason) -> Self {
        Self {
            stop,
            kind: ErrorKind::Unknown,
            evidence: Evidence::Supervisor,
            status: None,
            code: None,
            retry_after_ms: None,
            message: String::new(),
        }
    }
    pub fn automatic(&self) -> bool {
        (self.stop == StopReason::Timeout
            && matches!(
                self.kind,
                ErrorKind::Unknown | ErrorKind::Network | ErrorKind::ProviderTransient
            ))
            || (self.stop == StopReason::ProcessExit
                && self.evidence == Evidence::Structured
                && matches!(self.kind, ErrorKind::Network | ErrorKind::ProviderTransient))
    }
    pub fn session_unavailable(&self) -> bool {
        self.evidence == Evidence::Structured
            && matches!(
                self.kind,
                ErrorKind::SessionUnavailable | ErrorKind::ContextLimit
            )
    }
}

#[derive(Clone, Copy)]
pub(super) enum Protocol {
    Generic,
    Codex,
    Workbuddy,
    Opencode,
}

fn error_event(protocol: Protocol, v: &Value) -> bool {
    match protocol {
        Protocol::Codex => matches!(v["type"].as_str(), Some("error" | "turn.failed")),
        Protocol::Workbuddy => {
            v["type"] == "error" || (v["type"] == "result" && v["is_error"] == true)
        }
        Protocol::Opencode => v["type"] == "error" || v["type"] == "error.event",
        Protocol::Generic => {
            v["type"] == "error" || (v["type"].is_null() && v.get("error").is_some())
        }
    }
}

fn field<'a>(v: &'a Value, names: &[&str]) -> Option<&'a Value> {
    for name in names {
        if let Some(value) = v.get(name) {
            return Some(value);
        }
    }
    for name in ["error", "data", "cause", "response", "body"] {
        if let Some(child) = v.get(name) {
            if let Some(value) = field(child, names) {
                return Some(value);
            }
        }
    }
    None
}

fn classify(text: &str, status: Option<i64>) -> ErrorKind {
    let text = text.to_lowercase();
    if [
        "session_not_found",
        "session not found",
        "invalid_session",
        "invalid session",
        "session_expired",
        "session expired",
        "thread_not_found",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        ErrorKind::SessionUnavailable
    } else if [
        "context_length_exceeded",
        "context window exceeded",
        "context_limit",
        "maximum context length",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        ErrorKind::ContextLimit
    } else if [
        "insufficient_quota",
        "insufficient quota",
        "insufficient balance",
        "quota_exceeded",
        "billing",
        "credit balance",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        ErrorKind::Quota
    } else if matches!(status, Some(401 | 403))
        || [
            "unauthorized",
            "invalid api key",
            "invalid_api_key",
            "authentication",
        ]
        .iter()
        .any(|s| text.contains(s))
    {
        ErrorKind::Authentication
    } else if status == Some(429)
        || text.contains("rate_limit")
        || text.contains("too many requests")
    {
        ErrorKind::RateLimit
    } else if [
        "model_not_found",
        "invalid model",
        "invalid argument",
        "configuration_error",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        ErrorKind::Configuration
    } else if ["cli_error", "internal_cli_error"]
        .iter()
        .any(|s| text.contains(s))
    {
        ErrorKind::Cli
    } else if matches!(status, Some(500 | 502..=504)) {
        ErrorKind::ProviderTransient
    } else if [
        "connection reset",
        "connection timed out",
        "network unreachable",
        "socket hang up",
        "network_error",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        ErrorKind::Network
    } else {
        ErrorKind::Unknown
    }
}

pub(super) fn diagnose(protocol: Protocol, run: &AgentRun) -> Diagnosis {
    let stop = if run.stalled {
        StopReason::Stalled
    } else if run.timed_out {
        StopReason::Timeout
    } else if run.exit_code == Some(0) {
        StopReason::Completed
    } else {
        StopReason::ProcessExit
    };
    let mut diagnostic = Diagnosis::stopped(stop);
    if !matches!(stop, StopReason::Timeout | StopReason::Stalled) {
        diagnostic.evidence = Evidence::Unknown;
    }
    // A CLI may recover intermediate provider errors and complete successfully.
    if stop == StopReason::Completed {
        return diagnostic;
    }
    for v in run
        .stdout
        .lines()
        .chain(run.stderr.lines())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        if !error_event(protocol, &v) {
            continue;
        }
        let status = field(&v, &["statusCode", "status_code", "http_status", "status"])
            .and_then(|s| s.as_i64().or_else(|| s.as_str()?.parse().ok()));
        let code = field(&v, &["code", "error_type"])
            .and_then(Value::as_str)
            .map(str::to_string);
        let text = v.to_string();
        diagnostic = Diagnosis {
            stop,
            kind: classify(&text, status),
            evidence: Evidence::Structured,
            status,
            code,
            retry_after_ms: field(&v, &["retry_after_ms", "retryAfterMs"]).and_then(Value::as_i64),
            message: crate::failure::error_detail(&text, 1)
                .chars()
                .take(300)
                .collect(),
        };
    }
    if stop == StopReason::ProcessExit
        && diagnostic.evidence != Evidence::Structured
        && !run.stderr.trim().is_empty()
    {
        diagnostic.kind = classify(&run.stderr, None);
        diagnostic.evidence = Evidence::Text;
        diagnostic.message = crate::failure::error_detail(&run.stderr, 3)
            .chars()
            .take(300)
            .collect();
    }
    diagnostic
}

pub(super) fn session(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| {
            ["sessionID", "session_id", "thread_id"]
                .iter()
                .find_map(|key| {
                    v.get(key)
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_string)
                })
        })
        .next_back()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(stdout: &str, stderr: &str, exit: i64) -> AgentRun {
        AgentRun {
            stdout: stdout.into(),
            stderr: stderr.into(),
            exit_code: Some(exit),
            timed_out: false,
            stalled: false,
            session_id: None,
            usage: None,
        }
    }
    #[test]
    fn protocols_normalize_errors_without_promoting_tool_output() {
        for (protocol, text) in [
            (
                Protocol::Codex,
                r#"{"type":"turn.failed","error":{"status":503,"message":"unavailable"}}"#,
            ),
            (
                Protocol::Workbuddy,
                r#"{"type":"result","is_error":true,"error":{"status_code":503}}"#,
            ),
            (
                Protocol::Opencode,
                r#"{"type":"error","error":{"data":{"statusCode":503}}}"#,
            ),
            (Protocol::Generic, r#"{"error":{"status":503}}"#),
        ] {
            let d = diagnose(protocol, &run(text, "", 1));
            assert_eq!(d.kind, ErrorKind::ProviderTransient);
            assert!(d.automatic());
            assert!(!diagnose(protocol, &run(text, "", 0)).automatic());
            assert!(!diagnose(
                protocol,
                &run(
                    r#"{"type":"tool","result":"HTTP 503 invalid session"}"#,
                    "",
                    1
                )
            )
            .automatic());
        }
    }
    #[test]
    fn ambiguity_stays_manual_and_structured_session_errors_are_terminal() {
        let unknown = diagnose(Protocol::Generic, &run("", "HTTP 503 connection reset", 1));
        assert!(!unknown.automatic());
        assert_eq!(unknown.evidence, Evidence::Text);
        for (code, kind) in [
            ("invalid_api_key", ErrorKind::Authentication),
            ("insufficient_quota", ErrorKind::Quota),
            ("rate_limit", ErrorKind::RateLimit),
            ("model_not_found", ErrorKind::Configuration),
            ("session_not_found", ErrorKind::SessionUnavailable),
            ("context_length_exceeded", ErrorKind::ContextLimit),
        ] {
            let text = format!(r#"{{"type":"error","error":{{"code":"{code}"}}}}"#);
            let d = diagnose(Protocol::Generic, &run(&text, "", 1));
            assert_eq!(d.kind, kind);
            assert!(!d.automatic());
            assert_eq!(
                d.session_unavailable(),
                matches!(
                    kind,
                    ErrorKind::SessionUnavailable | ErrorKind::ContextLimit
                )
            );
        }
        assert!(!diagnose(Protocol::Generic, &run("", "invalid session", 1)).session_unavailable());
    }
}
