//! Failure summaries for user-facing views. Summaries are derived from the
//! stored error text on read; the original error stays untouched.

use serde::Serialize;
use serde_json::{Map, Value};

/// A summary is a display aid: it must never be the only copy of the failure
/// detail, and it is capped so a provider payload cannot overflow a page.
pub const MAX_MESSAGE_CHARS: usize = 300;

pub const KIND_RATE_LIMIT: &str = "rate_limit";
pub const KIND_TIMEOUT: &str = "timeout";
pub const KIND_STALLED: &str = "stalled";
pub const KIND_PROCESS_EXIT: &str = "process_exit";
pub const KIND_PROTOCOL: &str = "protocol";
pub const KIND_PERMANENT: &str = "permanent";
pub const KIND_PROVIDER_ERROR: &str = "provider_error";
pub const KIND_UNKNOWN: &str = "unknown";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FailureSummary {
    pub kind: String,
    pub message: String,
    pub status: Option<i64>,
    pub retry_after_ms: Option<i64>,
}

/// Builds a summary from a stored error. Returns `None` for missing or blank
/// errors so views can omit the field instead of rendering an empty failure.
pub fn summarize(raw: Option<&str>) -> Option<FailureSummary> {
    let raw = raw?;
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(summary) = summarize_json_lines(text) {
        return Some(summary);
    }
    Some(summarize_text(text))
}

fn summarize_json_lines(text: &str) -> Option<FailureSummary> {
    let mut last = None;
    for line in text.lines() {
        let Some(fragment) = json_fragment(line) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(fragment) else {
            continue;
        };
        let mut candidates = Vec::new();
        collect_candidates(&value, "", &Context::default(), 0, &mut candidates);
        if let Some(candidate) = best_candidate(candidates) {
            last = Some(candidate.into_summary());
        }
    }
    last
}

/// JSONL lines can start with the runner's exit-code prefix, so locate the
/// first object brace instead of requiring the line to be pure JSON.
fn json_fragment(line: &str) -> Option<&str> {
    let start = line.find('{')?;
    Some(line[start..].trim())
}

#[derive(Debug, Clone, Default)]
struct Context {
    kind_hint: Option<String>,
    status: Option<i64>,
    retry_after_ms: Option<i64>,
    under_error: bool,
}

#[derive(Debug, Clone)]
struct Candidate {
    message: String,
    context: Context,
    depth: usize,
}

impl Candidate {
    fn score(&self) -> i64 {
        let mut score = 0;
        if self.context.status.is_some() {
            score += 4;
        }
        if self.context.retry_after_ms.is_some() {
            score += 3;
        }
        if self.context.kind_hint.is_some() {
            score += 2;
        }
        if self.context.under_error {
            score += 2;
        }
        score * 1000 + self.depth as i64
    }

    fn into_summary(self) -> FailureSummary {
        let kind = classify_provider(&self.context, &self.message);
        FailureSummary {
            kind: kind.to_string(),
            message: truncate_message(&self.message),
            status: self.context.status,
            retry_after_ms: self.context.retry_after_ms,
        }
    }
}

fn best_candidate(candidates: Vec<Candidate>) -> Option<Candidate> {
    candidates.into_iter().max_by_key(Candidate::score)
}

fn collect_candidates(
    value: &Value,
    path: &str,
    inherited: &Context,
    depth: usize,
    out: &mut Vec<Candidate>,
) {
    match value {
        Value::Object(map) => {
            let mut context = inherited.clone();
            if let Some(hint) = string_field(map, &["type", "name", "code", "error_type"]) {
                context.kind_hint = Some(hint);
            }
            if let Some(status) = integer_field(
                map,
                &[
                    "status",
                    "statusCode",
                    "status_code",
                    "http_status",
                    "httpStatus",
                ],
            ) {
                context.status = Some(status);
            }
            if let Some(retry) = integer_field(
                map,
                &[
                    "retryAfterMs",
                    "retry_after_ms",
                    "retry-after-ms",
                    "retry_after",
                    "retryAfter",
                ],
            ) {
                context.retry_after_ms = Some(retry);
            }
            let is_error_path = path.contains("error")
                || path.contains("response")
                || path.contains("body")
                || path.contains("cause");
            context.under_error |= is_error_path;

            if let Some(message) = message_field(map) {
                if context.under_error
                    || context.status.is_some()
                    || context.retry_after_ms.is_some()
                    || context
                        .kind_hint
                        .as_deref()
                        .is_some_and(looks_like_error_hint)
                {
                    out.push(Candidate {
                        message: message.to_string(),
                        context: context.clone(),
                        depth,
                    });
                }
            }

            for (key, child) in map {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match child {
                    Value::String(body) => {
                        if !is_message_key(key) {
                            let trimmed = body.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                                collect_candidates(&parsed, &child_path, &context, depth + 1, out);
                            } else if depth > 0
                                && is_body_key(key)
                                && (context.under_error || child_path.contains("body"))
                            {
                                out.push(Candidate {
                                    message: trimmed.to_string(),
                                    context: context.clone(),
                                    depth: depth + 1,
                                });
                            }
                        }
                    }
                    Value::Object(_) | Value::Array(_) => {
                        collect_candidates(child, &child_path, &context, depth + 1, out);
                    }
                    _ => {}
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_candidates(
                    child,
                    &format!("{path}[{index}]"),
                    inherited,
                    depth + 1,
                    out,
                );
            }
        }
        _ => {}
    }
}

fn is_message_key(key: &str) -> bool {
    matches!(
        key,
        "message" | "detail" | "details" | "reason" | "error_description" | "description"
    )
}

fn is_body_key(key: &str) -> bool {
    matches!(key, "body" | "responseBody" | "response_body" | "response")
}

fn message_field(map: &Map<String, Value>) -> Option<&str> {
    for key in [
        "message",
        "detail",
        "details",
        "reason",
        "error_description",
        "description",
    ] {
        if let Some(text) = map.get(key).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn string_field(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        match map.get(*key) {
            Some(Value::String(text)) if !text.trim().is_empty() => {
                return Some(text.trim().to_string())
            }
            Some(Value::Number(number)) => return Some(number.to_string()),
            _ => {}
        }
    }
    None
}

fn integer_field(map: &Map<String, Value>, keys: &[&str]) -> Option<i64> {
    for key in keys {
        let Some(value) = map.get(*key) else {
            continue;
        };
        match value {
            Value::Number(number) => {
                if let Some(value) = number.as_i64() {
                    return Some(value);
                }
                if let Some(value) = number.as_f64() {
                    return Some(value.round() as i64);
                }
            }
            Value::String(text) => {
                if let Ok(value) = text.trim().parse::<i64>() {
                    return Some(value);
                }
            }
            _ => {}
        }
    }
    None
}

fn looks_like_error_hint(hint: &str) -> bool {
    let lowered = hint.to_lowercase();
    [
        "error",
        "provider",
        "quota",
        "rate",
        "limit",
        "exceed",
        "insufficient",
        "unavailable",
        "overloaded",
        "timeout",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

fn classify_provider(context: &Context, message: &str) -> &'static str {
    let hint = context.kind_hint.as_deref().unwrap_or_default();
    let lowered = format!("{} {}", hint, message).to_lowercase();
    if context.status == Some(429)
        || [
            "quota",
            "rate limit",
            "ratelimit",
            "rate_limit",
            "too many requests",
            "usage limit",
            "limit reached",
            "resource_exhausted",
            "insufficient_quota",
            "429",
            "限流",
            "请求过多",
            "额度",
        ]
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        return KIND_RATE_LIMIT;
    }
    if matches!(context.status, Some(401 | 403))
        || [
            "unauthorized",
            "forbidden",
            "invalid api key",
            "invalid_request",
        ]
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        return KIND_PERMANENT;
    }
    KIND_PROVIDER_ERROR
}

fn summarize_text(text: &str) -> FailureSummary {
    let lowered = text.to_lowercase();
    let kind = if lowered.contains("作业超时") || lowered.contains("timed out") {
        KIND_TIMEOUT
    } else if lowered.contains("卡死") || lowered.contains("stalled") {
        KIND_STALLED
    } else if lowered.contains("进程退出码") {
        KIND_PROCESS_EXIT
    } else if lowered.contains("结果协议不合法") || lowered.contains("没有写 result.json")
    {
        KIND_PROTOCOL
    } else if lowered.contains("永久错误")
        || lowered.contains("permanent")
        || lowered.contains("配置无效")
    {
        KIND_PERMANENT
    } else {
        KIND_UNKNOWN
    };
    let message = match kind {
        KIND_PROCESS_EXIT => process_exit_message(text),
        _ => collapse_whitespace(text),
    };
    FailureSummary {
        kind: kind.to_string(),
        message: truncate_message(&message),
        status: None,
        retry_after_ms: None,
    }
}

fn process_exit_message(text: &str) -> String {
    if let Some(position) = text.find("进程退出码") {
        if let Some(colon) = text[position..].find(':') {
            let detail = collapse_whitespace(&text[position + colon + 1..]);
            if !detail.is_empty() {
                return detail;
            }
        }
        let code = text[position + "进程退出码".len()..]
            .split(':')
            .next()
            .unwrap_or_default()
            .trim();
        if !code.is_empty() {
            return format!("Agent 进程退出（退出码 {code}）");
        }
    }
    collapse_whitespace(text)
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_message(text: &str) -> String {
    let collapsed = collapse_whitespace(text);
    if collapsed.chars().count() <= MAX_MESSAGE_CHARS {
        return collapsed;
    }
    let mut truncated: String = collapsed.chars().take(MAX_MESSAGE_CHARS - 1).collect();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_quota_jsonl() {
        let raw = "Agent 进程退出码 Some(1): {\"type\":\"error\",\"error\":{\"type\":\"provider.quota\",\"message\":\"Free usage limit reached\",\"status\":429,\"retryAfterMs\":60000}}";
        let summary = summarize(Some(raw)).expect("quota error is summarized");
        assert_eq!(summary.kind, KIND_RATE_LIMIT);
        assert_eq!(summary.message, "Free usage limit reached");
        assert_eq!(summary.status, Some(429));
        assert_eq!(summary.retry_after_ms, Some(60000));
    }

    #[test]
    fn parses_nested_response_body() {
        let raw = "Agent 进程退出码 Some(1): {\"type\":\"error\",\"error\":{\"name\":\"AI_APICallError\",\"message\":\"Provider request failed\",\"statusCode\":429,\"response\":{\"body\":{\"error\":{\"type\":\"provider.quota\",\"message\":\"Free usage limit reached\"}}}}}";
        let summary = summarize(Some(raw)).expect("nested body is summarized");
        assert_eq!(summary.kind, KIND_RATE_LIMIT);
        assert_eq!(summary.message, "Free usage limit reached");
        assert_eq!(summary.status, Some(429));
    }

    #[test]
    fn parses_plain_text_timeout() {
        let summary = summarize(Some("Agent 作业超时")).expect("timeout text is summarized");
        assert_eq!(summary.kind, KIND_TIMEOUT);
        assert_eq!(summary.message, "Agent 作业超时");
        assert_eq!(summary.status, None);
        assert_eq!(summary.retry_after_ms, None);
    }

    #[test]
    fn keeps_non_json_errors_readable() {
        let raw = "结果协议不合法: expected value at line 1 column 1\n补充说明";
        let summary = summarize(Some(raw)).expect("plain error is summarized");
        assert_eq!(summary.kind, KIND_PROTOCOL);
        assert!(summary.message.contains("结果协议不合法"));
        assert!(summary.message.contains("补充说明"));
        assert_eq!(summary.status, None);
    }

    #[test]
    fn selects_the_last_provider_error_in_jsonl() {
        let raw = "{\"type\":\"error\",\"error\":{\"type\":\"provider.error\",\"message\":\"first\"}}\n{\"type\":\"error\",\"error\":{\"type\":\"provider.quota\",\"message\":\"second\",\"status\":429}}";
        let summary = summarize(Some(raw)).expect("last error wins");
        assert_eq!(summary.message, "second");
        assert_eq!(summary.kind, KIND_RATE_LIMIT);
    }

    #[test]
    fn caps_message_length_and_ignores_blank_errors() {
        let raw = "x".repeat(MAX_MESSAGE_CHARS + 50);
        let summary = summarize(Some(&raw)).expect("long text is summarized");
        assert_eq!(summary.message.chars().count(), MAX_MESSAGE_CHARS);
        assert!(summary.message.ends_with('…'));
        assert!(summarize(Some("   ")).is_none());
        assert!(summarize(None).is_none());
    }
}
