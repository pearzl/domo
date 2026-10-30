//! Job activity derived from the job transcript. OpenCode, Codex and WorkBuddy
//! runs write one JSON event per line; mock jobs and other agents write plain
//! text. A transcript is classified by the first event that names a known
//! format, so only that format's mapping applies to the whole output. The
//! activity view is a read-only projection: stdout/stderr storage and job
//! state stay untouched.

use std::collections::{HashMap, VecDeque};

use serde::Serialize;
use serde_json::Value;

/// A transcript can grow to megabytes, so a single response is bounded in both
/// item count and item size. The newest items win because a running job is
/// most useful at its current position.
pub const MAX_ITEMS: usize = 300;
pub const MAX_ITEM_CHARS: usize = 500;

pub const FORMAT_OPENCODE: &str = "opencode";
pub const FORMAT_CODEX: &str = "codex";
pub const FORMAT_WORKBUDDY: &str = "workbuddy";
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Opencode,
    Codex,
    Workbuddy,
}

impl Format {
    fn name(self) -> &'static str {
        match self {
            Format::Opencode => FORMAT_OPENCODE,
            Format::Codex => FORMAT_CODEX,
            Format::Workbuddy => FORMAT_WORKBUDDY,
        }
    }
}

/// Keeps only the newest items while still counting every parsed event. A long
/// transcript is therefore bounded in memory as well as in response size.
#[derive(Default)]
struct ItemBuffer {
    items: VecDeque<ActivityItem>,
    total: usize,
}

impl ItemBuffer {
    /// Returns the ordinal of the buffered item so a later event can update it
    /// in place; the ordinal stays valid even after older items are dropped.
    fn push(&mut self, item: ActivityItem) -> usize {
        let ordinal = self.total;
        self.total += 1;
        if self.items.len() == MAX_ITEMS {
            self.items.pop_front();
        }
        self.items.push_back(item);
        ordinal
    }

    fn update(&mut self, ordinal: usize, edit: impl FnOnce(&mut ActivityItem)) {
        let first = self.total - self.items.len();
        if ordinal < first {
            return;
        }
        if let Some(item) = self.items.get_mut(ordinal - first) {
            edit(item);
        }
    }

    fn finish(self) -> (Vec<ActivityItem>, bool) {
        (self.items.into(), self.total > MAX_ITEMS)
    }
}

/// Accumulates the projection of one transcript: the bounded item buffer plus
/// the counters that cover every parsed event, including dropped items.
#[derive(Default)]
struct ParseState {
    items: ItemBuffer,
    summary: ActivitySummary,
}

impl ParseState {
    fn message(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.summary.messages += 1;
        self.items
            .push(item(KIND_MESSAGE, Some(text), None, None, None, None));
    }

    fn line(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.items
            .push(item(KIND_LINE, Some(text), None, None, None, None));
    }

    fn step(&mut self, text: Option<&str>) {
        self.summary.steps += 1;
        self.items
            .push(item(KIND_STEP, text, None, None, None, None));
    }

    fn tool(
        &mut self,
        tool: Option<String>,
        target: Option<String>,
        status: Option<String>,
        error: Option<String>,
    ) -> usize {
        self.summary.tool_calls += 1;
        self.items
            .push(item(KIND_TOOL, None, tool, target, status, error))
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
    let Some(format) = detect_format(raw) else {
        return text_report(raw, bytes);
    };
    let mut state = ParseState::default();
    match format {
        Format::Opencode => parse_opencode(raw, &mut state),
        Format::Codex => parse_codex(raw, &mut state),
        Format::Workbuddy => parse_workbuddy(raw, &mut state),
    }

    let (items, truncated) = state.items.finish();
    let summary = ActivitySummary {
        truncated,
        ..state.summary
    };
    JobActivity {
        format: format.name().into(),
        bytes,
        items,
        summary,
    }
}

/// JSONL output can mix harmless prefix lines with events, so the format is
/// taken from the first line that names a known protocol and then applied to
/// the whole transcript.
fn detect_format(raw: &str) -> Option<Format> {
    raw.lines()
        .filter_map(line_value)
        .find_map(|value| format_of(&value))
}

fn format_of(value: &Value) -> Option<Format> {
    let event = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(
        event,
        "thread.started" | "turn.started" | "turn.completed" | "turn.failed"
    ) {
        return Some(Format::Codex);
    }
    if matches!(event, "item.started" | "item.completed") && item_type(value).is_some() {
        return Some(Format::Codex);
    }
    if is_workbuddy_event(value, event) {
        return Some(Format::Workbuddy);
    }
    if opencode_kind(event).is_some() {
        return Some(Format::Opencode);
    }
    let part_type = payload(value)
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    opencode_kind(part_type).map(|_| Format::Opencode)
}

fn is_workbuddy_event(value: &Value, event: &str) -> bool {
    match event {
        // WorkBuddy wraps content blocks in `message`; Codex and OpenCode never
        // use these event names.
        "assistant" | "user" => value.get("message").is_some(),
        "result" => {
            value.get("subtype").is_some()
                || value.get("usage").is_some()
                || value.get("total_cost_usd").is_some()
        }
        "system" => matches!(
            value.get("subtype").and_then(Value::as_str),
            Some("init" | "status")
        ),
        _ => false,
    }
}

fn item_type(value: &Value) -> Option<&str> {
    value
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
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

fn opencode_kind(value: &str) -> Option<EventKind> {
    match value {
        "text" => Some(EventKind::Text),
        "tool_use" | "tool" => Some(EventKind::Tool),
        "step_finish" | "step-finish" => Some(EventKind::Step),
        _ => None,
    }
}

fn parse_opencode(raw: &str, state: &mut ParseState) {
    for value in raw.lines().filter_map(line_value) {
        let explicit = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let part = payload(&value);
        let part_type = part.get("type").and_then(Value::as_str).unwrap_or_default();
        let Some(kind) = opencode_kind(explicit).or_else(|| opencode_kind(part_type)) else {
            continue;
        };
        match kind {
            EventKind::Text => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    state.message(text);
                }
            }
            EventKind::Tool => {
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
                    state.summary.tool_failures += 1;
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
                state.tool(tool, target, status, error);
            }
            EventKind::Step => {
                let reason = part
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty());
                state.step(reason);
            }
        }
    }
}

/// Codex emits `item.started` and `item.completed` for the same item id. The
/// started event opens the tool entry (its exit code is still null) and the
/// completed event folds the result in, so one command stays one entry and a
/// failure is counted once. `agent_message` text is read from completion only.
fn parse_codex(raw: &str, state: &mut ParseState) {
    let mut tools: HashMap<String, usize> = HashMap::new();
    for value in raw.lines().filter_map(line_value) {
        let event = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match event {
            "item.started" | "item.completed" => {
                if let Some(item) = value.get("item") {
                    codex_item(state, &mut tools, item, event == "item.completed");
                }
            }
            "turn.completed" => state.step(None),
            "error" => state.line(&event_message(value.get("message").unwrap_or(&Value::Null))),
            "turn.failed" => state.line(
                &value
                    .get("error")
                    .map(event_message)
                    .unwrap_or_else(|| "turn failed".to_string()),
            ),
            _ => {}
        }
    }
}

fn codex_item(
    state: &mut ParseState,
    tools: &mut HashMap<String, usize>,
    item: &Value,
    completed: bool,
) {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
    let id = item.get("id").and_then(Value::as_str);
    match item_type {
        "agent_message" => {
            if completed {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    state.message(text);
                }
            }
        }
        "command_execution" | "file_change" | "mcp_tool_call" | "web_search" => {
            let tool = codex_tool_name(item_type, item);
            let target = codex_tool_target(item_type, item);
            let failed = completed && codex_tool_failed(item);
            if failed {
                state.summary.tool_failures += 1;
            }
            let status = if !completed {
                "in_progress"
            } else if failed {
                "error"
            } else {
                "completed"
            };
            let error = if failed {
                item.get("aggregated_output")
                    .and_then(error_first_line)
                    .or_else(|| item.get("error").and_then(error_first_line))
            } else {
                None
            };
            match id.and_then(|id| tools.get(id)).copied() {
                Some(ordinal) => state.items.update(ordinal, |entry| {
                    entry.status = Some(status.to_string());
                    entry.error = error.map(|text| truncate_chars(&text, MAX_ITEM_CHARS));
                }),
                None => {
                    let ordinal = state.tool(tool, target, Some(status.to_string()), error);
                    if let Some(id) = id {
                        tools.insert(id.to_string(), ordinal);
                    }
                }
            }
        }
        "error" => state.line(&event_message(item.get("message").unwrap_or(&Value::Null))),
        _ => {
            // Unknown item types are not guessed; the raw item is shown as a line.
            if completed {
                state.line(&item.to_string());
            }
        }
    }
}

fn codex_tool_name(item_type: &str, item: &Value) -> Option<String> {
    let name = match item_type {
        "file_change" => "file_change",
        "mcp_tool_call" => item
            .get("tool_name")
            .or_else(|| item.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("mcp_tool_call"),
        "web_search" => "web_search",
        _ => "exec_command",
    };
    non_empty(name)
}

fn codex_tool_target(item_type: &str, item: &Value) -> Option<String> {
    match item_type {
        "command_execution" => item.get("command").and_then(error_first_line),
        "file_change" => item
            .get("changes")
            .and_then(Value::as_array)
            .and_then(|changes| {
                changes
                    .iter()
                    .find_map(|change| change.get("path").and_then(error_first_line))
            }),
        "mcp_tool_call" => item.get("arguments").and_then(tool_target),
        "web_search" => item
            .get("query")
            .and_then(error_first_line)
            .or_else(|| item.get("url").and_then(error_first_line)),
        _ => None,
    }
}

/// A command item fails when its exit code is non-zero or its status is
/// `failed`/`error`. `item.started` always reports a null exit code, so only
/// completed items are judged.
fn codex_tool_failed(item: &Value) -> bool {
    if item
        .get("exit_code")
        .and_then(Value::as_i64)
        .is_some_and(|code| code != 0)
    {
        return true;
    }
    matches!(
        item.get("status").and_then(Value::as_str),
        Some("failed" | "error" | "canceled")
    )
}

/// WorkBuddy streams content blocks: assistant events announce text and tool
/// uses, user events return tool results keyed by `tool_use_id`. Results are
/// applied to the announced tool entry, so a failure is counted once.
fn parse_workbuddy(raw: &str, state: &mut ParseState) {
    let mut tools: HashMap<String, usize> = HashMap::new();
    for value in raw.lines().filter_map(line_value) {
        let event = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match event {
            "assistant" | "user" => {}
            // `result` carries session totals only; usage is read by the runner.
            _ => continue,
        }
        for block in message_blocks(&value) {
            match block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "text" => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        state.message(text);
                    }
                }
                "thinking" => {
                    let thinking = block
                        .get("thinking")
                        .or_else(|| block.get("text"))
                        .and_then(error_first_line);
                    state.step(thinking.as_deref());
                }
                "tool_use" => {
                    let tool = block
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_string);
                    let target = block.get("input").and_then(tool_target);
                    let ordinal = state.tool(tool, target, Some("pending".to_string()), None);
                    if let Some(id) = block.get("id").and_then(Value::as_str) {
                        tools.insert(id.to_string(), ordinal);
                    }
                }
                "tool_result" => {
                    let failed = tool_result_failed(block);
                    if failed {
                        state.summary.tool_failures += 1;
                    }
                    let status = if failed { "error" } else { "completed" };
                    let error = failed.then(|| tool_result_error(block)).flatten();
                    let ordinal = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .and_then(|id| tools.get(id))
                        .copied();
                    match ordinal {
                        Some(ordinal) => state.items.update(ordinal, |entry| {
                            entry.status = Some(status.to_string());
                            entry.error = error.map(|text| truncate_chars(&text, MAX_ITEM_CHARS));
                        }),
                        // A result without a matching tool_use still shows up,
                        // so a failure is never silent.
                        None => {
                            state.tool(None, None, Some(status.to_string()), error);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn message_blocks(value: &Value) -> Vec<&Value> {
    value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| blocks.iter().collect())
        .unwrap_or_default()
}

/// `tool_result.is_error` misses shell failures, so the raw response and the
/// rendered exit code decide whether the tool failed.
fn tool_result_failed(block: &Value) -> bool {
    if block.get("is_error").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    if let Some(raw) = block.get("_meta").and_then(|meta| meta.get("rawResponse")) {
        if raw.get("is_error").and_then(Value::as_bool) == Some(true) {
            return true;
        }
        if raw
            .get("exitCode")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
        {
            return true;
        }
    }
    tool_result_text(block)
        .as_deref()
        .and_then(render_exit_code)
        .is_some_and(|code| code != 0)
}

/// The failure text prefers the raw response error and falls back to the first
/// stderr line the CLI rendered into the result content.
fn tool_result_error(block: &Value) -> Option<String> {
    if let Some(error) = block
        .get("_meta")
        .and_then(|meta| meta.get("rawResponse"))
        .and_then(|raw| raw.get("error"))
        .and_then(error_first_line)
    {
        return Some(error);
    }
    let text = tool_result_text(block)?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(stderr) = line.strip_prefix("Stderr:") {
            let stderr = stderr.trim();
            if !stderr.is_empty() && stderr != "(empty)" {
                return non_empty(stderr);
            }
        }
    }
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn tool_result_text(block: &Value) -> Option<String> {
    match block.get("content")? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let text: Vec<&str> = blocks
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect();
            (!text.is_empty()).then(|| text.join("\n"))
        }
        _ => None,
    }
}

fn render_exit_code(text: &str) -> Option<i64> {
    let start = text.find("Exit Code:")? + "Exit Code:".len();
    let digits: String = text[start..]
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '-' || *ch == '+')
        .collect();
    digits.parse().ok()
}

/// Reads the text of a message-like JSON value; nested objects are probed for
/// the usual text keys before falling back to their serialized form.
fn event_message(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        Value::Object(map) => {
            for key in ["message", "text", "error", "reason"] {
                if let Some(found) = map.get(key) {
                    let text = event_message(found);
                    if !text.trim().is_empty() {
                        return text;
                    }
                }
            }
            value.to_string()
        }
        other => other.to_string(),
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

    fn codex_replay() -> String {
        concat!(
            r#"{"type":"thread.started","thread_id":"<thread-id>"}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_0","type":"error","message":"Model metadata for `qiniu/conatus-free` not found."}}"#,
            "\n",
            r#"{"type":"turn.started"}"#,
            "\n",
            r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/bash -lc 'printf hello > note.txt; cat missing.txt'","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/bash -lc 'printf hello > note.txt; cat missing.txt'","aggregated_output":"cat: missing.txt: No such file or directory\n","exit_code":1,"status":"failed"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_2","type":"command_execution","command":"cat result.json","aggregated_output":"{}","exit_code":0,"status":"completed"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"`cat missing.txt` failed as expected."}}"#,
            "\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":21349,"output_tokens":130}}"#,
            "\n",
        )
        .to_string()
    }

    #[test]
    fn parses_codex_jsonl_events() {
        let raw = codex_replay();
        let activity = report(&raw);
        assert_eq!(activity.format, FORMAT_CODEX);
        assert_eq!(activity.bytes, raw.len());
        assert_eq!(
            activity.summary,
            ActivitySummary {
                messages: 1,
                tool_calls: 2,
                tool_failures: 1,
                steps: 1,
                truncated: false,
            }
        );

        let warning = &activity.items[0];
        assert_eq!(warning.kind, KIND_LINE);
        assert_eq!(
            warning.text.as_deref(),
            Some("Model metadata for `qiniu/conatus-free` not found.")
        );

        let failed = &activity.items[1];
        assert_eq!(failed.kind, KIND_TOOL);
        assert_eq!(failed.tool.as_deref(), Some("exec_command"));
        assert_eq!(
            failed.target.as_deref(),
            Some("/bin/bash -lc 'printf hello > note.txt; cat missing.txt'")
        );
        assert_eq!(failed.status.as_deref(), Some("error"));
        assert_eq!(
            failed.error.as_deref(),
            Some("cat: missing.txt: No such file or directory")
        );

        let completed = &activity.items[2];
        assert_eq!(completed.tool.as_deref(), Some("exec_command"));
        assert_eq!(completed.status.as_deref(), Some("completed"));
        assert_eq!(completed.error, None);

        let message = &activity.items[3];
        assert_eq!(message.kind, KIND_MESSAGE);
        assert_eq!(
            message.text.as_deref(),
            Some("`cat missing.txt` failed as expected.")
        );

        assert_eq!(activity.items[4].kind, KIND_STEP);
    }

    #[test]
    fn codex_failure_is_judged_by_exit_code_even_without_status() {
        let raw = concat!(
            r#"{"type":"thread.started","thread_id":"t"}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"false","aggregated_output":"boom","exit_code":2}}"#,
            "\n",
            r#"{"type":"error","message":"Reconnecting... 1/5"}"#,
            "\n",
            r#"{"type":"turn.failed","error":{"message":"unexpected status 404 Not Found"}}"#,
            "\n",
        );
        let activity = report(raw);
        assert_eq!(activity.format, FORMAT_CODEX);
        assert_eq!(activity.summary.tool_calls, 1);
        assert_eq!(activity.summary.tool_failures, 1);
        assert_eq!(activity.summary.steps, 0);
        assert_eq!(activity.items[0].status.as_deref(), Some("error"));
        assert_eq!(activity.items[0].error.as_deref(), Some("boom"));
        assert_eq!(
            activity.items[1].text.as_deref(),
            Some("Reconnecting... 1/5")
        );
        assert_eq!(
            activity.items[2].text.as_deref(),
            Some("unexpected status 404 Not Found")
        );
    }

    #[test]
    fn codex_non_command_items_become_tools() {
        let raw = concat!(
            r#"{"type":"thread.started","thread_id":"t"}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_1","type":"file_change","changes":[{"path":"src/lib.rs","kind":"update"}],"status":"completed"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_2","type":"mcp_tool_call","server":"docs","tool_name":"search","arguments":{"query":"activity"},"status":"completed"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_3","type":"web_search","query":"codex jsonl"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_4","type":"reasoning","text":"内部推理"}}"#,
            "\n",
        );
        let activity = report(raw);
        assert_eq!(activity.format, FORMAT_CODEX);
        assert_eq!(activity.summary.tool_calls, 3);
        assert_eq!(activity.summary.tool_failures, 0);
        assert_eq!(activity.items[0].tool.as_deref(), Some("file_change"));
        assert_eq!(activity.items[0].target.as_deref(), Some("src/lib.rs"));
        assert_eq!(activity.items[1].tool.as_deref(), Some("search"));
        assert_eq!(activity.items[1].target.as_deref(), Some("activity"));
        assert_eq!(activity.items[2].tool.as_deref(), Some("web_search"));
        assert_eq!(activity.items[2].target.as_deref(), Some("codex jsonl"));
        assert_eq!(activity.items[3].kind, KIND_LINE);
        assert!(activity.items[3]
            .text
            .as_deref()
            .unwrap()
            .contains("reasoning"));
    }

    fn workbuddy_replay() -> String {
        concat!(
            r#"{"type":"system","subtype":"init","session_id":"s","model":"hy3","permissionMode":"bypassPermissions"}"#,
            "\n",
            r#"{"type":"system","subtype":"status","session_id":"s","status":null}"#,
            "\n",
            r#"{"type":"assistant","session_id":"s","message":{"role":"assistant","content":[{"type":"tool_use","id":"tool-1","name":"Read","input":{"file_path":"task.txt"}}]}}"#,
            "\n",
            r#"{"type":"assistant","session_id":"s","message":{"role":"assistant","content":[{"type":"tool_use","id":"tool-2","name":"Bash","input":{"command":"cat missing.txt","description":"read missing file"}}]}}"#,
            "\n",
            r#"{"type":"user","session_id":"s","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":[{"type":"text","text":"alpha"}],"is_error":false}]}}"#,
            "\n",
            r#"{"type":"user","session_id":"s","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-2","content":[{"type":"text","text":"Stdout: (empty)\nStderr: cat: missing.txt: No such file or directory\n\nExit Code: 1\nSignal: (none)"}],"is_error":false}]}}"#,
            "\n",
            r#"{"type":"assistant","session_id":"s","message":{"role":"assistant","content":[{"type":"text","text":"task.txt contained alpha."}]}}"#,
            "\n",
            r#"{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0,"usage":{"input_tokens":73243,"output_tokens":274}}"#,
            "\n",
        )
        .to_string()
    }

    #[test]
    fn parses_workbuddy_jsonl_events() {
        let raw = workbuddy_replay();
        let activity = report(&raw);
        assert_eq!(activity.format, FORMAT_WORKBUDDY);
        assert_eq!(activity.bytes, raw.len());
        assert_eq!(
            activity.summary,
            ActivitySummary {
                messages: 1,
                tool_calls: 2,
                tool_failures: 1,
                steps: 0,
                truncated: false,
            }
        );
        assert_eq!(activity.items.len(), 3);

        let read = &activity.items[0];
        assert_eq!(read.kind, KIND_TOOL);
        assert_eq!(read.tool.as_deref(), Some("Read"));
        assert_eq!(read.target.as_deref(), Some("task.txt"));
        assert_eq!(read.status.as_deref(), Some("completed"));

        let bash = &activity.items[1];
        assert_eq!(bash.tool.as_deref(), Some("Bash"));
        assert_eq!(bash.target.as_deref(), Some("cat missing.txt"));
        assert_eq!(bash.status.as_deref(), Some("error"));
        assert_eq!(
            bash.error.as_deref(),
            Some("cat: missing.txt: No such file or directory")
        );

        let message = &activity.items[2];
        assert_eq!(message.kind, KIND_MESSAGE);
        assert_eq!(message.text.as_deref(), Some("task.txt contained alpha."));
    }

    #[test]
    fn workbuddy_tool_result_carries_error_status_independently() {
        let raw = concat!(
            r#"{"type":"system","subtype":"init","session_id":"s","tools":["Bash"]}"#,
            "\n",
            r#"{"type":"assistant","session_id":"s","message":{"role":"assistant","content":[{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"false"}}]}}"#,
            "\n",
            r#"{"type":"user","session_id":"s","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"permission denied","is_error":true}]}}"#,
            "\n",
            r#"{"type":"assistant","session_id":"s","message":{"role":"assistant","content":[{"type":"thinking","thinking":"需要重试\n下一行"}]}}"#,
            "\n",
            r#"{"type":"result","subtype":"success","is_error":false}"#,
            "\n",
        );
        let activity = report(raw);
        assert_eq!(activity.format, FORMAT_WORKBUDDY);
        assert_eq!(activity.summary.tool_calls, 1);
        assert_eq!(activity.summary.tool_failures, 1);
        assert_eq!(activity.summary.steps, 1);
        assert_eq!(activity.summary.messages, 0);
        assert_eq!(activity.items[0].kind, KIND_TOOL);
        assert_eq!(activity.items[0].status.as_deref(), Some("error"));
        assert_eq!(
            activity.items[0].error.as_deref(),
            Some("permission denied")
        );
        assert_eq!(activity.items[1].kind, KIND_STEP);
        assert_eq!(activity.items[1].text.as_deref(), Some("需要重试"));
    }

    #[test]
    fn workbuddy_result_event_adds_no_activity() {
        let raw = concat!(
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            "\n",
            r#"{"type":"assistant","session_id":"s","message":{"role":"assistant","content":[{"type":"text","text":"400 model service info not found"}]}}"#,
            "\n",
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["400 model service info not found\n可用模型列表"]}"#,
            "\n",
        );
        let activity = report(raw);
        assert_eq!(activity.format, FORMAT_WORKBUDDY);
        assert_eq!(activity.summary.messages, 1);
        assert_eq!(activity.items.len(), 1);
        assert_eq!(
            activity.items[0].text.as_deref(),
            Some("400 model service info not found")
        );
    }

    #[test]
    fn formats_are_classified_by_the_first_known_event() {
        let raw = concat!(
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            "\n",
            "Reading additional input from stdin...\n",
            r#"{"type":"assistant","session_id":"s","message":{"role":"assistant","content":[{"type":"text","text":"完成"}]}}"#,
            "\n",
            r#"{"type":"result","subtype":"success","is_error":false}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"另一种格式"}}"#,
            "\n",
        );
        let activity = report(raw);
        assert_eq!(activity.format, FORMAT_WORKBUDDY);
        assert_eq!(activity.summary.messages, 1);
        assert_eq!(activity.items.len(), 1);
        assert_eq!(activity.items[0].text.as_deref(), Some("完成"));

        let stderr_only = "Reading additional input from stdin...\n";
        assert_eq!(report(stderr_only).format, FORMAT_TEXT);

        let empty = report("");
        assert_eq!(empty.format, FORMAT_TEXT);
        assert!(empty.items.is_empty());
    }
}
