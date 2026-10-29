//! Cost and usage accounting, plus the availability gates used by scheduling.
//!
//! Usage comes from the result protocol when an agent provides it, or from the
//! OpenCode session ledger for OpenCode jobs. Token and cost values that remain
//! unavailable stay `None`; they are never coerced to zero, so aggregation can
//! count unknown jobs separately and usage limits only consume known values.
//! Cache read, cache write, and reasoning tokens are stored as details of the
//! total input and output counts. Availability windows are evaluated in the
//! server's local timezone on every scheduling pass, which makes a new day or
//! month restore an agent without an explicit enable action.

use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Datelike, Local, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::domain::extract_json;

pub const METRIC_TOKENS: &str = "tokens";
pub const METRIC_INPUT_TOKENS: &str = "input_tokens";
pub const METRIC_OUTPUT_TOKENS: &str = "output_tokens";
pub const METRIC_COST: &str = "cost";

pub const WINDOW_DAY: &str = "day";
pub const WINDOW_MONTH: &str = "month";

pub const UNKNOWN_AGENT_KEY: &str = "unknown";
pub const UNKNOWN_AGENT_LABEL: &str = "未记录";

/// A usage snapshot from a result payload or an OpenCode session ledger.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

impl UsageRecord {
    pub fn from_result(raw: &str) -> Option<Self> {
        let value = extract_json(raw)?;
        value.get("usage").and_then(Self::from_value)
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let record = Self {
            input_tokens: token_value(object.get("input_tokens")),
            output_tokens: token_value(object.get("output_tokens")),
            cache_read_tokens: token_value(object.get("cache_read_tokens")),
            cache_write_tokens: token_value(object.get("cache_write_tokens")),
            reasoning_tokens: token_value(object.get("reasoning_tokens")),
            cost: cost_value(object.get("cost")),
            duration_ms: token_value(object.get("duration_ms")),
        };
        if record.input_tokens.is_none()
            && record.output_tokens.is_none()
            && record.cache_read_tokens.is_none()
            && record.cache_write_tokens.is_none()
            && record.reasoning_tokens.is_none()
            && record.cost.is_none()
            && record.duration_ms.is_none()
        {
            return None;
        }
        Some(record)
    }

    /// Usage is "known" when at least one token or cost value is present.
    /// Duration alone does not make the accounting complete.
    pub fn known(&self) -> bool {
        self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cache_read_tokens.is_some()
            || self.cache_write_tokens.is_some()
            || self.reasoning_tokens.is_some()
            || self.cost.is_some()
    }
}

fn token_value(value: Option<&Value>) -> Option<i64> {
    let number = numeric_value(value?)?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    Some(number.round() as i64)
}

fn cost_value(value: Option<&Value>) -> Option<f64> {
    let number = numeric_value(value?)?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    Some(number)
}

fn numeric_value(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Duration from the job's persisted start and end timestamps.
pub fn elapsed_ms(started_at: Option<&str>, finished_at: &str) -> Option<i64> {
    let start = DateTime::parse_from_rfc3339(started_at?).ok()?;
    let end = DateTime::parse_from_rfc3339(finished_at).ok()?;
    let millis = (end - start).num_milliseconds();
    (millis >= 0).then_some(millis)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UsageJobRow {
    pub project_id: String,
    pub agent_id: Option<String>,
    pub role: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub cost: Option<f64>,
    pub duration_ms: Option<i64>,
    pub usage_known: i64,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
}

pub async fn load_jobs(pool: &SqlitePool, project_id: Option<&str>) -> Result<Vec<UsageJobRow>> {
    const COLUMNS: &str = "SELECT project_id, agent_id, role, input_tokens, output_tokens, \
                          cache_read_tokens, cache_write_tokens, reasoning_tokens, cost, \
                          duration_ms, usage_known, started_at, finished_at, created_at FROM jobs";
    let rows = match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, UsageJobRow>(&format!("{COLUMNS} WHERE project_id = ?"))
                .bind(project_id)
                .fetch_all(pool)
                .await?
        }
        None => {
            sqlx::query_as::<_, UsageJobRow>(COLUMNS)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageTotals {
    pub jobs: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub reasoning_tokens: i64,
    pub cost: f64,
    pub duration_ms: i64,
    /// Jobs with a recorded token or cost value; `jobs = known_jobs + unknown_jobs`.
    pub known_jobs: i64,
    pub unknown_jobs: i64,
}

impl UsageTotals {
    fn add(&mut self, row: &UsageJobRow) {
        self.jobs += 1;
        self.input_tokens += row.input_tokens.unwrap_or(0);
        self.output_tokens += row.output_tokens.unwrap_or(0);
        self.cache_read_tokens += row.cache_read_tokens.unwrap_or(0);
        self.cache_write_tokens += row.cache_write_tokens.unwrap_or(0);
        self.reasoning_tokens += row.reasoning_tokens.unwrap_or(0);
        self.cost += row.cost.unwrap_or(0.0);
        self.duration_ms += row_duration(row);
        if row.usage_known == 0 {
            self.unknown_jobs += 1;
        } else {
            self.known_jobs += 1;
        }
    }
}

fn row_duration(row: &UsageJobRow) -> i64 {
    row.duration_ms.unwrap_or_else(|| {
        row.finished_at
            .as_deref()
            .and_then(|finished| elapsed_ms(row.started_at.as_deref(), finished))
            .unwrap_or(0)
    })
}

fn row_time(row: &UsageJobRow) -> Option<DateTime<Utc>> {
    row.started_at
        .as_deref()
        .or(Some(row.created_at.as_str()))
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|value| value.with_timezone(&Utc))
}

#[derive(Debug, Clone, Default)]
pub struct UsageFilter {
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
    pub role: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

impl UsageFilter {
    fn matches(&self, row: &UsageJobRow) -> bool {
        if let Some(project_id) = &self.project_id {
            if &row.project_id != project_id {
                return false;
            }
        }
        if let Some(agent_id) = &self.agent_id {
            if row.agent_id.as_deref() != Some(agent_id.as_str()) {
                return false;
            }
        }
        if let Some(role) = &self.role {
            if &row.role != role {
                return false;
            }
        }
        if self.from.is_some() || self.to.is_some() {
            let Some(time) = row_time(row) else {
                return false;
            };
            if self.from.as_ref().is_some_and(|from| time < *from) {
                return false;
            }
            if self.to.as_ref().is_some_and(|to| time > *to) {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageGroupBy {
    Project,
    Agent,
    Role,
    None,
}

impl UsageGroupBy {
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(GROUP_PROJECT)
        {
            GROUP_PROJECT | "projects" => Ok(Self::Project),
            GROUP_AGENT | "agents" => Ok(Self::Agent),
            GROUP_ROLE | "roles" => Ok(Self::Role),
            "none" | "total" | "all" => Ok(Self::None),
            _ => Err("group_by 必须是 project、agent、role 或 none".into()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => GROUP_PROJECT,
            Self::Agent => GROUP_AGENT,
            Self::Role => GROUP_ROLE,
            Self::None => "none",
        }
    }
}

pub const GROUP_PROJECT: &str = "project";
pub const GROUP_AGENT: &str = "agent";
pub const GROUP_ROLE: &str = "role";

#[derive(Debug, Clone, Serialize)]
pub struct UsageGroup {
    pub key: String,
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub role: Option<String>,
    #[serde(flatten)]
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageFilterView {
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
    pub role: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

impl From<&UsageFilter> for UsageFilterView {
    fn from(filter: &UsageFilter) -> Self {
        Self {
            project_id: filter.project_id.clone(),
            agent_id: filter.agent_id.clone(),
            role: filter.role.clone(),
            from: filter.from.as_ref().map(|value| value.to_rfc3339()),
            to: filter.to.as_ref().map(|value| value.to_rfc3339()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageReport {
    pub group_by: String,
    pub filters: UsageFilterView,
    pub totals: UsageTotals,
    pub groups: Vec<UsageGroup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectUsage {
    pub totals: UsageTotals,
    pub by_agent: Vec<UsageGroup>,
    pub by_role: Vec<UsageGroup>,
}

/// Aggregates matching jobs into totals and one bucket per requested dimension.
pub fn summarize(
    rows: &[UsageJobRow],
    filter: &UsageFilter,
    group_by: UsageGroupBy,
    agent_names: &HashMap<String, String>,
) -> UsageReport {
    let mut totals = UsageTotals::default();
    let mut groups: Vec<UsageGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for row in rows.iter().filter(|row| filter.matches(row)) {
        totals.add(row);
        let (key, project_id, agent_id, agent_name, role) = match group_by {
            UsageGroupBy::Project => (
                row.project_id.clone(),
                Some(row.project_id.clone()),
                None,
                None,
                None,
            ),
            UsageGroupBy::Agent => match &row.agent_id {
                Some(agent_id) => {
                    let name = agent_names.get(agent_id).cloned();
                    (agent_id.clone(), None, Some(agent_id.clone()), name, None)
                }
                None => (
                    UNKNOWN_AGENT_KEY.to_string(),
                    None,
                    None,
                    Some(UNKNOWN_AGENT_LABEL.to_string()),
                    None,
                ),
            },
            UsageGroupBy::Role => (row.role.clone(), None, None, None, Some(row.role.clone())),
            UsageGroupBy::None => ("all".to_string(), None, None, None, None),
        };
        let position = match index.get(&key) {
            Some(position) => *position,
            None => {
                let position = groups.len();
                index.insert(key.clone(), position);
                groups.push(UsageGroup {
                    key,
                    project_id,
                    agent_id,
                    agent_name,
                    role,
                    totals: UsageTotals::default(),
                });
                position
            }
        };
        groups[position].totals.add(row);
    }
    groups.sort_by(|left, right| left.key.cmp(&right.key));
    UsageReport {
        group_by: group_by.as_str().to_string(),
        filters: UsageFilterView::from(filter),
        totals,
        groups,
    }
}

pub fn project_usage(
    rows: &[UsageJobRow],
    project_id: &str,
    agent_names: &HashMap<String, String>,
) -> ProjectUsage {
    let filter = UsageFilter {
        project_id: Some(project_id.to_string()),
        ..UsageFilter::default()
    };
    let by_agent = summarize(rows, &filter, UsageGroupBy::Agent, agent_names);
    let by_role = summarize(rows, &filter, UsageGroupBy::Role, agent_names);
    ProjectUsage {
        totals: by_agent.totals,
        by_agent: by_agent.groups,
        by_role: by_role.groups,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvailabilitySlot {
    /// 1 = Monday, 7 = Sunday.
    pub weekday: u32,
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AvailabilityConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub slots: Vec<AvailabilitySlot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AvailabilityStatus {
    pub available: bool,
    pub reason: Option<String>,
    pub next_start: Option<String>,
}

pub fn parse_availability(raw: Option<&str>) -> Option<AvailabilityConfig> {
    serde_json::from_str(raw?).ok()
}

pub fn validate_availability(config: &AvailabilityConfig) -> Result<(), String> {
    if config.slots.len() > 50 {
        return Err("可用时段最多支持 50 条".into());
    }
    for slot in &config.slots {
        if !(1..=7).contains(&slot.weekday) {
            return Err("可用时段的 weekday 必须是 1（周一）到 7（周日）".into());
        }
        let start = parse_clock(&slot.start)
            .ok_or_else(|| format!("可用时段开始时间不合法: {}", slot.start))?;
        let end = parse_clock(&slot.end)
            .ok_or_else(|| format!("可用时段结束时间不合法: {}", slot.end))?;
        if start >= end {
            return Err("可用时段的开始时间必须早于结束时间".into());
        }
    }
    Ok(())
}

pub fn availability_status(
    config: Option<&AvailabilityConfig>,
    at: DateTime<Local>,
) -> AvailabilityStatus {
    let Some(config) = config else {
        return available();
    };
    if !config.enabled {
        return available();
    }
    let weekday = at.weekday().number_from_monday();
    let time = at.time();
    let in_slot = config.slots.iter().any(|slot| {
        slot.weekday == weekday
            && match (parse_clock(&slot.start), parse_clock(&slot.end)) {
                (Some(start), Some(end)) => time >= start && time < end,
                _ => false,
            }
    });
    if in_slot {
        return available();
    }
    let next_start = next_start(config, at);
    let current = format!("{} {}", weekday_label(weekday), time.format("%H:%M"));
    let reason = match &next_start {
        Some(next) => format!("不在可用时段（当前 {current}，下次可用：{next}）"),
        None => format!("不在可用时段（当前 {current}），没有后续可用时段"),
    };
    AvailabilityStatus {
        available: false,
        reason: Some(reason),
        next_start,
    }
}

fn available() -> AvailabilityStatus {
    AvailabilityStatus {
        available: true,
        reason: None,
        next_start: None,
    }
}

fn parse_clock(text: &str) -> Option<NaiveTime> {
    let text = text.trim();
    NaiveTime::parse_from_str(text, "%H:%M")
        .ok()
        .or_else(|| NaiveTime::parse_from_str(text, "%H:%M:%S").ok())
}

fn next_start(config: &AvailabilityConfig, at: DateTime<Local>) -> Option<String> {
    for offset in 0..=7_i64 {
        let date = at.date_naive() + chrono::Duration::days(offset);
        let weekday = date.weekday().number_from_monday();
        let mut best: Option<NaiveTime> = None;
        for slot in &config.slots {
            if slot.weekday != weekday {
                continue;
            }
            let Some(start) = parse_clock(&slot.start) else {
                continue;
            };
            if offset == 0 && start <= at.time() {
                continue;
            }
            best = Some(best.map_or(start, |current| current.min(start)));
        }
        if let Some(best) = best {
            let prefix = if offset == 0 {
                "今天"
            } else {
                weekday_label(weekday)
            };
            return Some(format!("{prefix} {}", best.format("%H:%M")));
        }
    }
    None
}

pub fn weekday_label(weekday: u32) -> &'static str {
    match weekday {
        1 => "周一",
        2 => "周二",
        3 => "周三",
        4 => "周四",
        5 => "周五",
        6 => "周六",
        _ => "周日",
    }
}

fn default_metric() -> String {
    METRIC_TOKENS.to_string()
}

fn default_window() -> String {
    WINDOW_DAY.to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageLimitConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_metric")]
    pub metric: String,
    #[serde(default = "default_window")]
    pub window: String,
    #[serde(default)]
    pub limit: f64,
}

impl Default for UsageLimitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            metric: default_metric(),
            window: default_window(),
            limit: 0.0,
        }
    }
}

pub fn parse_usage_limit(raw: Option<&str>) -> Option<UsageLimitConfig> {
    serde_json::from_str(raw?).ok()
}

pub fn validate_usage_limit(config: &UsageLimitConfig) -> Result<(), String> {
    if !matches!(
        config.metric.as_str(),
        METRIC_TOKENS | METRIC_INPUT_TOKENS | METRIC_OUTPUT_TOKENS | METRIC_COST
    ) {
        return Err("用量限额 metric 必须是 tokens、input_tokens、output_tokens 或 cost".into());
    }
    if !matches!(config.window.as_str(), WINDOW_DAY | WINDOW_MONTH) {
        return Err("用量限额 window 必须是 day 或 month".into());
    }
    if !config.limit.is_finite() || config.limit < 0.0 {
        return Err("用量限额必须是大于等于 0 的数值".into());
    }
    if config.enabled && config.limit <= 0.0 {
        return Err("启用用量限额时限额必须大于 0".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageWindow {
    pub metric: String,
    pub window: String,
    pub window_start: String,
    pub window_end: String,
    pub used: f64,
    pub limit: f64,
    pub exceeded: bool,
}

impl UsageWindow {
    pub fn exceeded_reason(&self) -> String {
        format!(
            "已达到用量限额（{}{}）：已用 {} / 限额 {}，窗口 {} 至 {} 重置",
            self.window_label(),
            self.metric_label(),
            format_amount(self.used),
            format_amount(self.limit),
            self.window_start,
            self.window_end
        )
    }

    fn window_label(&self) -> &'static str {
        if self.window == WINDOW_MONTH {
            "月度"
        } else {
            "每日"
        }
    }

    fn metric_label(&self) -> &'static str {
        match self.metric.as_str() {
            METRIC_COST => "费用",
            METRIC_INPUT_TOKENS => "输入 token",
            METRIC_OUTPUT_TOKENS => "输出 token",
            _ => " token",
        }
    }
}

fn format_amount(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.4}")
    }
}

pub fn limit_exceeded(config: &UsageLimitConfig, used: f64) -> bool {
    config.enabled && config.limit > 0.0 && used >= config.limit
}

pub fn window_bounds(
    config: &UsageLimitConfig,
    at: DateTime<Local>,
    reset_at: Option<DateTime<Utc>>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let date = at.date_naive();
    let (start_naive, end_naive) = if config.window == WINDOW_MONTH {
        let start = NaiveDate::from_ymd_opt(date.year(), date.month(), 1)
            .unwrap_or_else(|| date.and_hms_opt(0, 0, 0).map(|v| v.date()).unwrap_or(date));
        let (year, month) = if date.month() == 12 {
            (date.year() + 1, 1)
        } else {
            (date.year(), date.month() + 1)
        };
        let end = NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(start);
        (
            start.and_hms_opt(0, 0, 0).unwrap_or_default(),
            end.and_hms_opt(0, 0, 0).unwrap_or_default(),
        )
    } else {
        let start = date.and_hms_opt(0, 0, 0).unwrap_or_default();
        let end = date
            .succ_opt()
            .and_then(|next| next.and_hms_opt(0, 0, 0))
            .unwrap_or_default();
        (start, end)
    };
    let mut start = local_to_utc(start_naive);
    let end = local_to_utc(end_naive);
    if let Some(reset_at) = reset_at {
        if reset_at > start && reset_at < end {
            start = reset_at;
        }
    }
    (start, end)
}

fn local_to_utc(value: chrono::NaiveDateTime) -> DateTime<Utc> {
    match Local.from_local_datetime(&value) {
        LocalResult::Single(time) => time.with_timezone(&Utc),
        LocalResult::Ambiguous(earliest, _) => earliest.with_timezone(&Utc),
        LocalResult::None => DateTime::<Utc>::from_naive_utc_and_offset(value, Utc),
    }
}

fn metric_value(metric: &str, row: &UsageJobRow) -> f64 {
    match metric {
        METRIC_COST => row.cost.unwrap_or(0.0),
        METRIC_INPUT_TOKENS => row.input_tokens.unwrap_or(0) as f64,
        METRIC_OUTPUT_TOKENS => row.output_tokens.unwrap_or(0) as f64,
        _ => (row.input_tokens.unwrap_or(0) + row.output_tokens.unwrap_or(0)) as f64,
    }
}

fn in_window(row: &UsageJobRow, start: DateTime<Utc>, end: DateTime<Utc>) -> bool {
    row_time(row)
        .map(|time| time >= start && time < end)
        .unwrap_or(false)
}

/// Current-window consumption for one agent. Unknown usage contributes zero,
/// so it never consumes the limit. `None` means the agent has no limit config.
pub async fn agent_usage(
    pool: &SqlitePool,
    agent_id: &str,
    usage_limit_json: Option<&str>,
    usage_reset_at: Option<&str>,
    at: DateTime<Utc>,
) -> Result<Option<UsageWindow>> {
    let Some(config) = parse_usage_limit(usage_limit_json) else {
        return Ok(None);
    };
    let reset_at = usage_reset_at
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|value| value.with_timezone(&Utc));
    let local = at.with_timezone(&Local);
    let (start, end) = window_bounds(&config, local, reset_at);
    let rows = sqlx::query_as::<_, UsageJobRow>(
        "SELECT project_id, agent_id, role, input_tokens, output_tokens, \
         cache_read_tokens, cache_write_tokens, reasoning_tokens, cost, duration_ms, \
         usage_known, started_at, finished_at, created_at FROM jobs WHERE agent_id = ?",
    )
    .bind(agent_id)
    .fetch_all(pool)
    .await?;
    let used: f64 = rows
        .iter()
        .filter(|row| row.usage_known != 0)
        .filter(|row| in_window(row, start, end))
        .map(|row| metric_value(&config.metric, row))
        .sum();
    Ok(Some(UsageWindow {
        metric: config.metric.clone(),
        window: config.window.clone(),
        window_start: start.to_rfc3339(),
        window_end: end.to_rfc3339(),
        used,
        limit: config.limit,
        exceeded: limit_exceeded(&config, used),
    }))
}

/// Why an agent cannot be dispatched even though it is enabled and not cooling.
#[derive(Debug, Clone)]
pub enum AgentGate {
    Schedule(String),
    Limit(String),
}

/// Evaluates the availability plan before the concurrency and priority rules.
pub async fn agent_gate(
    pool: &SqlitePool,
    agent_id: &str,
    availability_json: Option<&str>,
    usage_limit_json: Option<&str>,
    usage_reset_at: Option<&str>,
    at: DateTime<Utc>,
) -> Result<Option<AgentGate>> {
    let availability = parse_availability(availability_json);
    let status = availability_status(availability.as_ref(), at.with_timezone(&Local));
    if !status.available {
        return Ok(Some(AgentGate::Schedule(
            status.reason.unwrap_or_else(|| "不在可用时段".into()),
        )));
    }
    if let Some(window) = agent_usage(pool, agent_id, usage_limit_json, usage_reset_at, at).await? {
        if window.exceeded {
            return Ok(Some(AgentGate::Limit(window.exceeded_reason())));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        project: &str,
        agent: Option<&str>,
        role: &str,
        input: Option<i64>,
        output: Option<i64>,
        known: bool,
        time: &str,
    ) -> UsageJobRow {
        UsageJobRow {
            project_id: project.into(),
            agent_id: agent.map(str::to_string),
            role: role.into(),
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost: known.then_some(0.5),
            duration_ms: None,
            usage_known: if known { 1 } else { 0 },
            started_at: Some(time.into()),
            finished_at: Some(time.into()),
            created_at: time.into(),
        }
    }

    #[test]
    fn parses_usage_object_from_result() {
        let raw = r#"{"kind":"done","summary":"ok","usage":{"input_tokens":120,"output_tokens":30,"cache_read_tokens":40,"cache_write_tokens":5,"reasoning_tokens":12,"cost":0.25,"duration_ms":900}}"#;
        let usage = UsageRecord::from_result(raw).unwrap();
        assert_eq!(usage.input_tokens, Some(120));
        assert_eq!(usage.output_tokens, Some(30));
        assert_eq!(usage.cache_read_tokens, Some(40));
        assert_eq!(usage.cache_write_tokens, Some(5));
        assert_eq!(usage.reasoning_tokens, Some(12));
        assert_eq!(usage.cost, Some(0.25));
        assert_eq!(usage.duration_ms, Some(900));
        assert!(usage.known());
    }

    #[test]
    fn token_details_alone_make_usage_known() {
        let usage = UsageRecord::from_value(&serde_json::json!({"cache_read_tokens": 10})).unwrap();
        assert!(usage.known());
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.cache_read_tokens, Some(10));
    }

    #[test]
    fn missing_usage_stays_unknown() {
        assert!(UsageRecord::from_result(r#"{"kind":"done"}"#).is_none());
        let usage = UsageRecord::from_result(r#"{"usage":{"duration_ms":500}}"#).unwrap();
        assert!(!usage.known());
        assert_eq!(usage.duration_ms, Some(500));
    }

    #[test]
    fn aggregates_known_and_unknown_separately() {
        let rows = vec![
            row(
                "p1",
                Some("a"),
                "executor",
                Some(10),
                Some(5),
                true,
                "2026-10-27T10:00:00+00:00",
            ),
            row(
                "p1",
                Some("a"),
                "executor",
                Some(20),
                Some(5),
                true,
                "2026-10-27T11:00:00+00:00",
            ),
            row(
                "p1",
                Some("a"),
                "verifier",
                None,
                None,
                false,
                "2026-10-27T12:00:00+00:00",
            ),
        ];
        let report = summarize(
            &rows,
            &UsageFilter {
                project_id: Some("p1".into()),
                ..UsageFilter::default()
            },
            UsageGroupBy::Role,
            &HashMap::new(),
        );
        assert_eq!(report.totals.jobs, 3);
        assert_eq!(report.totals.input_tokens, 30);
        assert_eq!(report.totals.output_tokens, 10);
        assert_eq!(report.totals.known_jobs, 2);
        assert_eq!(report.totals.unknown_jobs, 1);
        assert_eq!(
            report.totals.jobs,
            report.totals.known_jobs + report.totals.unknown_jobs
        );
        assert_eq!(report.groups.len(), 2);
        let verifier = report
            .groups
            .iter()
            .find(|group| group.key == "verifier")
            .unwrap();
        assert_eq!(verifier.totals.jobs, 1);
        assert_eq!(verifier.totals.known_jobs, 0);
        assert_eq!(verifier.totals.unknown_jobs, 1);
        assert_eq!(verifier.totals.input_tokens, 0);
    }

    #[test]
    fn totals_serialize_known_and_unknown_jobs() {
        let totals = UsageTotals {
            jobs: 3,
            known_jobs: 2,
            unknown_jobs: 1,
            ..UsageTotals::default()
        };
        let value = serde_json::to_value(&totals).unwrap();
        assert_eq!(value["jobs"], 3);
        assert_eq!(value["known_jobs"], 2);
        assert_eq!(value["unknown_jobs"], 1);
        assert_eq!(
            value["jobs"].as_i64().unwrap(),
            value["known_jobs"].as_i64().unwrap() + value["unknown_jobs"].as_i64().unwrap()
        );
    }

    #[test]
    fn usage_coverage_tracks_known_and_unknown_jobs() {
        let all_unknown = vec![
            row(
                "p1",
                Some("a"),
                "executor",
                None,
                None,
                false,
                "2026-10-27T10:00:00+00:00",
            ),
            row(
                "p1",
                Some("a"),
                "executor",
                None,
                None,
                false,
                "2026-10-27T11:00:00+00:00",
            ),
        ];
        let report = summarize(
            &all_unknown,
            &UsageFilter::default(),
            UsageGroupBy::None,
            &HashMap::new(),
        );
        assert_eq!(report.totals.jobs, 2);
        assert_eq!(report.totals.known_jobs, 0);
        assert_eq!(report.totals.unknown_jobs, 2);
        assert_eq!(
            report.totals.jobs,
            report.totals.known_jobs + report.totals.unknown_jobs
        );

        let partial = vec![
            row(
                "p1",
                Some("a"),
                "executor",
                Some(12),
                Some(3),
                true,
                "2026-10-27T10:00:00+00:00",
            ),
            row(
                "p1",
                Some("a"),
                "verifier",
                Some(8),
                Some(2),
                true,
                "2026-10-27T11:00:00+00:00",
            ),
            row(
                "p1",
                Some("a"),
                "summary",
                None,
                None,
                false,
                "2026-10-27T12:00:00+00:00",
            ),
        ];
        let report = summarize(
            &partial,
            &UsageFilter::default(),
            UsageGroupBy::None,
            &HashMap::new(),
        );
        assert_eq!(report.totals.jobs, 3);
        assert_eq!(report.totals.known_jobs, 2);
        assert_eq!(report.totals.unknown_jobs, 1);
        assert_eq!(
            report.totals.jobs,
            report.totals.known_jobs + report.totals.unknown_jobs
        );

        let empty = summarize(
            &[],
            &UsageFilter::default(),
            UsageGroupBy::None,
            &HashMap::new(),
        );
        assert_eq!(empty.totals.jobs, 0);
        assert_eq!(empty.totals.known_jobs, 0);
        assert_eq!(empty.totals.unknown_jobs, 0);
    }

    #[test]
    fn aggregates_token_details() {
        let mut job = row(
            "p1",
            Some("a"),
            "executor",
            Some(10),
            Some(5),
            true,
            "2026-10-27T10:00:00+00:00",
        );
        job.cache_read_tokens = Some(7);
        job.cache_write_tokens = Some(3);
        job.reasoning_tokens = Some(2);
        let report = summarize(
            &[job],
            &UsageFilter::default(),
            UsageGroupBy::None,
            &HashMap::new(),
        );
        assert_eq!(report.totals.input_tokens, 10);
        assert_eq!(report.totals.output_tokens, 5);
        assert_eq!(report.totals.cache_read_tokens, 7);
        assert_eq!(report.totals.cache_write_tokens, 3);
        assert_eq!(report.totals.reasoning_tokens, 2);
    }

    #[test]
    fn filters_by_time_window() {
        let rows = vec![
            row(
                "p1",
                Some("a"),
                "executor",
                Some(10),
                None,
                true,
                "2026-10-27T10:00:00+00:00",
            ),
            row(
                "p1",
                Some("a"),
                "executor",
                Some(20),
                None,
                true,
                "2026-10-28T10:00:00+00:00",
            ),
        ];
        let report = summarize(
            &rows,
            &UsageFilter {
                from: Some(
                    DateTime::parse_from_rfc3339("2026-10-28T00:00:00+00:00")
                        .unwrap()
                        .with_timezone(&Utc),
                ),
                ..UsageFilter::default()
            },
            UsageGroupBy::None,
            &HashMap::new(),
        );
        assert_eq!(report.totals.jobs, 1);
        assert_eq!(report.totals.input_tokens, 20);
    }

    #[test]
    fn availability_uses_local_weekday_and_clock() {
        let at = Local.with_ymd_and_hms(2026, 10, 27, 10, 30, 0).unwrap();
        let open = AvailabilityConfig {
            enabled: true,
            slots: vec![AvailabilitySlot {
                weekday: 2,
                start: "09:00".into(),
                end: "12:00".into(),
            }],
        };
        assert!(availability_status(Some(&open), at).available);
        let closed = AvailabilityConfig {
            enabled: true,
            slots: vec![AvailabilitySlot {
                weekday: 2,
                start: "12:00".into(),
                end: "18:00".into(),
            }],
        };
        let status = availability_status(Some(&closed), at);
        assert!(!status.available);
        assert!(status.reason.unwrap().contains("下次可用"));
        assert!(validate_availability(&open).is_ok());
        let invalid = AvailabilityConfig {
            enabled: true,
            slots: vec![AvailabilitySlot {
                weekday: 8,
                start: "09:00".into(),
                end: "08:00".into(),
            }],
        };
        assert!(validate_availability(&invalid).is_err());
    }

    #[test]
    fn usage_limit_ignores_unknown_rows() {
        let config = UsageLimitConfig {
            enabled: true,
            metric: METRIC_TOKENS.into(),
            window: WINDOW_DAY.into(),
            limit: 100.0,
        };
        assert!(!limit_exceeded(&config, 99.0));
        assert!(limit_exceeded(&config, 100.0));
        let unknown = row(
            "p1",
            Some("a"),
            "executor",
            None,
            None,
            false,
            "2026-10-27T10:00:00+00:00",
        );
        assert_eq!(metric_value(METRIC_TOKENS, &unknown), 0.0);
        assert_eq!(metric_value(METRIC_COST, &unknown), 0.0);
    }

    #[test]
    fn day_window_starts_at_local_midnight_and_honors_reset() {
        let at = Local.with_ymd_and_hms(2026, 10, 27, 10, 30, 0).unwrap();
        let config = UsageLimitConfig {
            enabled: true,
            metric: METRIC_TOKENS.into(),
            window: WINDOW_DAY.into(),
            limit: 10.0,
        };
        let (start, end) = window_bounds(&config, at, None);
        let expected_start = Local
            .with_ymd_and_hms(2026, 10, 27, 0, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(start, expected_start);
        assert!(end > start);
        let reset = at.with_timezone(&Utc);
        let (start, _) = window_bounds(&config, at, Some(reset));
        assert_eq!(start, reset);
    }
}
