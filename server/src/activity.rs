//! Job activity derived from the job transcript. OpenCode runs write one JSON
//! event per line; other agents and mock jobs write plain text. The activity
//! view is a read-only projection: stdout/stderr storage and job state stay
//! untouched.

use std::collections::VecDeque;

use serde::Serialize;
use serde_json::Value;

/// A transcript can grow to megabytes, so a single response is bounded in both
/// item count and item size. The newest items win because a running job is
/// most useful at its current position.
pub const MAX_ITEMS: usize = 300;
pub const MAX_ITEM_CHARS: usize = 500;

pub const FORMAT_OPENCODE: &str = "opencode";
pub const FORMAT_TEXT: &str = "text";

pub const KIND_MESSAGE: &str = "message";
pub const KIND_TOOL: &str = "tool";
pub const KIND_STEP: &str = "step";
pub const KIND_LINE: &str = "line";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct JobActivity {
    pub format: String,
    pub bytes: usize,
    pub items: Vec<ActivityItem>,
    pub summary: ActivitySummary,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ActivityItem {
    pub kind: String,
    pub text: Option<String>,
    pub tool: Option<String>,
    pub target: Option<String>,
    pub status: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ActivitySummary {
    pub messages: usize,
    pub tool_calls: usize,
    pub tool_failures: usize,
    pub steps: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventKind {
    Text,
    Tool,
    Step,
}

/// Keeps only the newest items while still counting every parsed event. A long
/// transcript is therefore bounded in memory as well as in response size.
#[derive(Default)]
struct ItemBuffer {
    items: VecDeque<ActivityItem>,
    total: usize,
}

impl ItemBuffer {
    fn push(&mut self, item: ActivityItem) {
        self.total += 1;
        if self.items.len() == MAX_ITEMS {
            self.items.pop_front();
        }
        self.items.push_back(item);
    }

    fn finish(self) -> (Vec<ActivityItem>, bool) {
        (self.items.into(), self.total > MAX_ITEMS)
    }
}

/// Parses a transcript, treating non-JSONL input as line previews. `bytes`
/// counts the source bytes, not the characters after lossy decoding.
pub fn report_from_bytes(raw: &[u8]) -> JobActivity {
    report_with_bytes(&String::from_utf8_lossy(raw), raw.len())
}

#[cfg(test)]
pub fn report(raw: &str) -> JobActivity {
    report_with_bytes(raw, raw.len())
}

fn report_with_bytes(raw: &str, bytes: usize) -> JobActivity {
    let mut items = ItemBuffer::default();
    let mut summary = ActivitySummary::default();
    let mut recognized = false;

    for line in raw.lines() {
        let Some(value) = line_value(line) else {
            continue;
        };
        let Some(kind) = event_kind(&value) else {
            continue;
        };
        recognized = true;
        let part = payload(&value);
        match kind {
            EventKind::Text => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    let text = text.trim();
                    if !text.is_empty() {
                        summary.messages += 1;
                        items.push(item(KIND_MESSAGE, Some(text), None, None, None, None));
                    }
                }
            }
            EventKind::Tool => {
                summary.tool_calls += 1;
                let status = part
                    .get("state")
                    .and_then(|state| state.get("status"))
                    .or_else(|| part.get("status"))
                    .and_then(Value::as_str);
                let error = part
                    .get("state")
                    .and_then(|state| state.get("error"))
                    .or_else(|| part.get("error"))
                    .and_then(error_first_line);
                let failed =
                    error.is_some() || matches!(status, Some("error" | "failed" | "canceled"));
                if failed {
                    summary.tool_failures += 1;
                }
                let status = status
                    .map(str::to_string)
                    .or_else(|| failed.then(|| "error".to_string()));
                let tool = part
                    .get("tool")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("name").and_then(Value::as_str))
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string);
                let target = part
                    .get("state")
                    .and_then(|state| state.get("input"))
                    .or_else(|| part.get("input"))
                    .and_then(tool_target);
                items.push(item(KIND_TOOL, None, tool, target, status, error));
            }
            EventKind::Step => {
                summary.steps += 1;
                let reason = part
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty());
                items.push(item(KIND_STEP, reason, None, None, None, None));
            }
        }
    }

    if !recognized {
        return text_report(raw, bytes);
    }

    let (items, truncated) = items.finish();
    summary.truncated = truncated;
    JobActivity {
        format: FORMAT_OPENCODE.into(),
        bytes,
        items,
        summary,
    }
}

fn text_report(raw: &str, bytes: usize) -> JobActivity {
    let mut buffer = ItemBuffer::default();
    for line in raw
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
    {
        buffer.push(ActivityItem {
            kind: KIND_LINE.into(),
            text: Some(truncate_chars(line, MAX_ITEM_CHARS)),
            tool: None,
            target: None,
            status: None,
            error: None,
        });
    }
    let (items, truncated) = buffer.finish();
    JobActivity {
        format: FORMAT_TEXT.into(),
        bytes,
        items,
        summary: ActivitySummary {
            truncated,
            ..ActivitySummary::default()
        },
    }
}

fn item(
    kind: &str,
    text: Option<&str>,
    tool: Option<String>,
    target: Option<String>,
    status: Option<String>,
    error: Option<String>,
) -> ActivityItem {
    ActivityItem {
        kind: kind.into(),
        text: text.map(|value| truncate_chars(value, MAX_ITEM_CHARS)),
        tool: tool.map(|value| truncate_chars(&value, MAX_ITEM_CHARS)),
        target: target.map(|value| truncate_chars(&value, MAX_ITEM_CHARS)),
        status: status.map(|value| truncate_chars(&value, MAX_ITEM_CHARS)),
        error: error.map(|value| truncate_chars(&value, MAX_ITEM_CHARS)),
    }
}

/// JSONL lines can carry a prefix from the runner, so start at the first brace
/// instead of requiring a pure JSON line.
fn line_value(line: &str) -> Option<Value> {
    let start = line.find('{')?;
    serde_json::from_str(line[start..].trim()).ok()
}

fn event_kind(value: &Value) -> Option<EventKind> {
    let explicit = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(kind) = kind_from_type(explicit) {
        return Some(kind);
    }
    let part_type = payload(value)
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    kind_from_type(part_type)
}

fn kind_from_type(value: &str) -> Option<EventKind> {
    match value {
        "text" => Some(EventKind::Text),
        "tool_use" | "tool" => Some(EventKind::Tool),
        "step_finish" | "step-finish" => Some(EventKind::Step),
        _ => None,
    }
}

fn payload(value: &Value) -> &Value {
    value
        .get("part")
        .or_else(|| value.get("properties").and_then(|props| props.get("part")))
        .filter(|part| part.is_object())
        .unwrap_or(value)
}

/// A tool target is the path it touched or the first line of its command.
/// Other input is omitted so the timeline stays a summary, not a payload dump.
fn tool_target(input: &Value) -> Option<String> {
    if let Some(text) = input.as_str() {
        return non_empty(first_line(text));
    }
    let map = input.as_object()?;
    for key in ["command", "cmd"] {
        if let Some(value) = map.get(key).and_then(Value::as_str) {
            if let Some(text) = non_empty(first_line(value)) {
                return Some(text);
            }
        }
    }
    for key in [
        "filePath",
        "file_path",
        "path",
        "file",
        "filename",
        "pattern",
        "query",
        "url",
        "description",
    ] {
        if let Some(value) = map.get(key).and_then(Value::as_str) {
            if let Some(text) = non_empty(first_line(value)) {
                return Some(text);
            }
        }
    }
    None
}

fn error_first_line(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => non_empty(first_line(text)),
        Value::Object(map) => {
            for key in ["message", "error", "reason", "data", "cause"] {
                if let Some(found) = map.get(key).and_then(error_first_line) {
                    return Some(found);
                }
            }
            map.get("name").and_then(error_first_line)
        }
        Value::Null => None,
        other => non_empty(first_line(&other.to_string())),
    }
}

fn first_line(value: &str) -> &str {
    value.lines().next().unwrap_or_default().trim()
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn truncate_chars(value: &str, max: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut truncated: String = value.chars().take(max.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_opencode_jsonl_events() {
        let raw = concat!(
            r#"{"type":"text","timestamp":1,"sessionID":"ses_1","part":{"type":"text","text":"我会先读取入口文件。"}}"#,
            "\n",
            r#"{"type":"tool_use","timestamp":2,"sessionID":"ses_1","part":{"type":"tool","tool":"read","state":{"status":"completed","input":{"filePath":"src/main.rs"},"output":"fn main() {}"}}}"#,
            "\n",
            r#"{"type":"tool_use","timestamp":3,"sessionID":"ses_1","part":{"type":"tool","tool":"bash","state":{"status":"error","input":{"command":"cargo test --all\ncd .."},"error":"error[E0308]: mismatched types\n --> src/main.rs:1:1"}}}"#,
            "\n",
            r#"{"type":"step_finish","timestamp":4,"sessionID":"ses_1","part":{"type":"step-finish","reason":"stop"}}"#,
            "\n",
            "runner prefix {",
            "\n",
            r#"{"type":"tool_use","part":{"type":"tool","tool":"edit","state":{"status":"completed","input":{"path":"src/lib.rs"}}}}"#,
        );

        let activity = report(raw);
        assert_eq!(activity.format, FORMAT_OPENCODE);
        assert_eq!(activity.bytes, raw.len());
        assert_eq!(activity.items.len(), 5);
        assert_eq!(
            activity.summary,
            ActivitySummary {
                messages: 1,
                tool_calls: 3,
                tool_failures: 1,
                steps: 1,
                truncated: false,
            }
        );

        let message = &activity.items[0];
        assert_eq!(message.kind, KIND_MESSAGE);
        assert_eq!(message.text.as_deref(), Some("我会先读取入口文件。"));

        let read = &activity.items[1];
        assert_eq!(read.kind, KIND_TOOL);
        assert_eq!(read.tool.as_deref(), Some("read"));
        assert_eq!(read.target.as_deref(), Some("src/main.rs"));
        assert_eq!(read.status.as_deref(), Some("completed"));
        assert_eq!(read.error, None);

        let bash = &activity.items[2];
        assert_eq!(bash.tool.as_deref(), Some("bash"));
        assert_eq!(bash.target.as_deref(), Some("cargo test --all"));
        assert_eq!(bash.status.as_deref(), Some("error"));
        assert_eq!(
            bash.error.as_deref(),
            Some("error[E0308]: mismatched types")
        );

        let step = &activity.items[3];
        assert_eq!(step.kind, KIND_STEP);
        assert_eq!(step.text.as_deref(), Some("stop"));

        assert_eq!(activity.items.last().unwrap().tool.as_deref(), Some("edit"));
        assert_eq!(
            activity.items.last().unwrap().target.as_deref(),
            Some("src/lib.rs")
        );
    }

    #[test]
    fn falls_back_to_text_lines() {
        let raw = "mock job job_1 streaming\nmock job job_1 segment 1\n\n结束\n";
        let activity = report(raw);
        assert_eq!(activity.format, FORMAT_TEXT);
        assert_eq!(activity.bytes, raw.len());
        assert_eq!(activity.items.len(), 3);
        assert!(activity.items.iter().all(|item| item.kind == KIND_LINE));
        assert_eq!(
            activity.items[0].text.as_deref(),
            Some("mock job job_1 streaming")
        );
        assert_eq!(activity.summary.messages, 0);
        assert_eq!(activity.summary.tool_calls, 0);
        assert!(!activity.summary.truncated);
    }

    #[test]
    fn caps_items_and_item_size() {
        let mut raw = String::new();
        for index in 0..(MAX_ITEMS + 5) {
            let event = serde_json::json!({
                "type": "text",
                "part": {
                    "type": "text",
                    "text": format!("message-{index}-{}", "x".repeat(800)),
                },
            });
            raw.push_str(&event.to_string());
            raw.push('\n');
        }
        let activity = report(&raw);
        assert_eq!(activity.items.len(), MAX_ITEMS);
        assert!(activity.summary.truncated);
        assert_eq!(activity.summary.messages, MAX_ITEMS + 5);
        assert!(activity
            .items
            .first()
            .unwrap()
            .text
            .as_deref()
            .unwrap()
            .starts_with("message-5-"));
        for item in &activity.items {
            assert!(item.text.as_deref().unwrap().chars().count() <= MAX_ITEM_CHARS);
        }
    }

    #[test]
    fn caps_text_preview_lines() {
        let mut raw = String::new();
        for index in 0..(MAX_ITEMS + 2) {
            raw.push_str(&format!("line-{index}-{}\n", "y".repeat(700)));
        }
        let activity = report(&raw);
        assert_eq!(activity.format, FORMAT_TEXT);
        assert_eq!(activity.items.len(), MAX_ITEMS);
        assert!(activity.summary.truncated);
        assert!(activity
            .items
            .first()
            .unwrap()
            .text
            .as_deref()
            .unwrap()
            .starts_with("line-2-"));
        for item in &activity.items {
            assert!(item.text.as_deref().unwrap().chars().count() <= MAX_ITEM_CHARS);
        }
    }

    #[test]
    fn nested_part_and_error_objects_are_supported() {
        let raw = concat!(
            r#"{"type":"message.part.updated","properties":{"part":{"type":"tool","tool":"task","state":{"status":"failed","input":{"description":"整理报告"},"error":{"message":"子任务失败\n堆栈"}}}}}"#,
            "\n",
            r#"{"part":{"type":"text","text":"继续处理"}}"#,
        );
        let activity = report(raw);
        assert_eq!(activity.format, FORMAT_OPENCODE);
        assert_eq!(activity.items.len(), 2);
        assert_eq!(activity.summary.tool_failures, 1);
        assert_eq!(activity.items[0].status.as_deref(), Some("failed"));
        assert_eq!(activity.items[0].target.as_deref(), Some("整理报告"));
        assert_eq!(activity.items[0].error.as_deref(), Some("子任务失败"));
        assert_eq!(activity.items[1].text.as_deref(), Some("继续处理"));
    }
}
