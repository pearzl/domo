//! OpenCode session usage lookup. Every job owns a dedicated OpenCode session.
//! After the process exits, the session ledger is the authoritative source for
//! tokens and cost: it includes every step plus title and compaction usage.
//! Child subagent sessions are separate rows and are summed through `parent_id`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::usage::UsageRecord;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_SESSIONS: usize = 256;

pub fn bin() -> String {
    std::env::var("DOMO_OPENCODE_BIN").unwrap_or_else(|_| "opencode".into())
}

/// Session IDs must stay unique per job execution. DOMO job IDs already carry a
/// UUID, so the derived session never collides across retries.
pub fn session_id(job_id: &str) -> String {
    let suffix = job_id.strip_prefix("job_").unwrap_or(job_id);
    format!("ses_domo{suffix}")
}

pub async fn fetch_usage(session_id: &str) -> Option<UsageRecord> {
    if let Some(path) = db_path().await {
        match fetch_usage_db(&path, session_id).await {
            Ok(Some(usage)) => return Some(usage),
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(session_id, error = %error, "从 OpenCode 数据库读取用量失败");
            }
        }
    }
    match fetch_usage_api(session_id).await {
        Ok(Some(usage)) => Some(usage),
        Ok(None) => None,
        Err(error) => {
            tracing::debug!(session_id, error = %error, "从 OpenCode API 读取用量失败");
            None
        }
    }
}

/// Stops a session that outlived its OpenCode client. The server-side execution
/// keeps running after the client process dies, so interrupt and wait before the
/// final usage snapshot.
pub async fn settle(session_id: &str) {
    let bin = bin();
    let interrupt = format!("/api/session/{session_id}/interrupt");
    let wait = format!("/api/experimental/session/{session_id}/wait");
    let _ = command_output(&bin, &["api", "post", &interrupt]).await;
    let _ = command_output(&bin, &["api", "post", &wait]).await;
}

async fn db_path() -> Option<PathBuf> {
    let output = command_output(&bin(), &["debug", "paths", "db"]).await?;
    let raw = String::from_utf8_lossy(&output.stdout);
    let path = raw.lines().next()?.trim();
    if path.is_empty() {
        return None;
    }
    let path = PathBuf::from(path);
    path.is_file().then_some(path)
}

async fn fetch_usage_db(path: &Path, session_id: &str) -> Result<Option<UsageRecord>> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .busy_timeout(Duration::from_secs(2));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("打开 OpenCode 数据库失败: {}", path.display()))?;
    let result = sqlx::query_as::<_, UsageRow>(
        "WITH RECURSIVE tree(id) AS ( \
             SELECT ?1 \
             UNION \
             SELECT session.id FROM session_v2 AS session \
             JOIN tree ON session.parent_id = tree.id \
         ) \
         SELECT \
             COALESCE(SUM(session.cost), 0.0) AS cost, \
             COALESCE(SUM(session.tokens_input), 0) AS input, \
             COALESCE(SUM(session.tokens_output), 0) AS output, \
             COALESCE(SUM(session.tokens_reasoning), 0) AS reasoning, \
             COALESCE(SUM(session.tokens_cache_read), 0) AS cache_read, \
             COALESCE(SUM(session.tokens_cache_write), 0) AS cache_write, \
             COUNT(*) AS sessions \
         FROM session_v2 AS session \
         WHERE session.id IN (SELECT id FROM tree)",
    )
    .bind(session_id)
    .fetch_one(&pool)
    .await;
    pool.close().await;
    let row = result?;
    if row.sessions == 0 {
        return Ok(None);
    }
    Ok(Some(record(
        row.cost,
        row.input,
        row.output,
        row.reasoning,
        row.cache_read,
        row.cache_write,
    )))
}

async fn fetch_usage_api(session_id: &str) -> Result<Option<UsageRecord>> {
    let mut totals = ApiTotals::default();
    let mut pending = vec![session_id.to_string()];
    let mut seen = HashSet::new();
    let mut found = false;
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if seen.len() > MAX_SESSIONS {
            break;
        }
        let Some(data) = api_session(&id).await? else {
            continue;
        };
        found = true;
        totals.add(&data);
        pending.extend(api_children(&id).await?);
    }
    if !found {
        return Ok(None);
    }
    Ok(Some(totals.into_record()))
}

async fn api_session(session_id: &str) -> Result<Option<Value>> {
    let path = format!("/api/session/{session_id}");
    let Some(output) = command_output(&bin(), &["api", "get", &path]).await else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&output.stdout)?;
    Ok(value.get("data").filter(|data| !data.is_null()).cloned())
}

async fn api_children(session_id: &str) -> Result<Vec<String>> {
    let parent = format!("parentID={session_id}");
    let Some(output) = command_output(
        &bin(),
        &[
            "api",
            "get",
            "/api/session",
            "--param",
            &parent,
            "--param",
            "limit=100",
        ],
    )
    .await
    else {
        return Ok(Vec::new());
    };
    let value: Value = serde_json::from_slice(&output.stdout)?;
    Ok(value
        .get("data")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

async fn command_output(bin: &str, args: &[&str]) -> Option<std::process::Output> {
    let output = tokio::time::timeout(
        COMMAND_TIMEOUT,
        tokio::process::Command::new(bin).args(args).output(),
    )
    .await
    .ok()?
    .ok()?;
    output.status.success().then_some(output)
}

#[derive(Debug, sqlx::FromRow)]
struct UsageRow {
    cost: f64,
    input: i64,
    output: i64,
    reasoning: i64,
    cache_read: i64,
    cache_write: i64,
    sessions: i64,
}

fn record(
    cost: f64,
    input: i64,
    output: i64,
    reasoning: i64,
    cache_read: i64,
    cache_write: i64,
) -> UsageRecord {
    UsageRecord {
        input_tokens: Some(input.saturating_add(cache_read).saturating_add(cache_write)),
        output_tokens: Some(output.saturating_add(reasoning)),
        cache_read_tokens: Some(cache_read),
        cache_write_tokens: Some(cache_write),
        reasoning_tokens: Some(reasoning),
        cost: Some(cost),
        duration_ms: None,
    }
}

#[derive(Debug, Default)]
struct ApiTotals {
    cost: f64,
    input: i64,
    output: i64,
    reasoning: i64,
    cache_read: i64,
    cache_write: i64,
}

impl ApiTotals {
    fn add(&mut self, data: &Value) {
        self.cost += data.get("cost").and_then(number).unwrap_or(0.0);
        let Some(tokens) = data.get("tokens") else {
            return;
        };
        self.input += tokens.get("input").and_then(integer).unwrap_or(0);
        self.output += tokens.get("output").and_then(integer).unwrap_or(0);
        self.reasoning += tokens.get("reasoning").and_then(integer).unwrap_or(0);
        if let Some(cache) = tokens.get("cache") {
            self.cache_read += cache.get("read").and_then(integer).unwrap_or(0);
            self.cache_write += cache.get("write").and_then(integer).unwrap_or(0);
        }
    }

    fn into_record(self) -> UsageRecord {
        record(
            self.cost,
            self.input,
            self.output,
            self.reasoning,
            self.cache_read,
            self.cache_write,
        )
    }
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|value| value as f64))
}

fn integer(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn write_session_db(path: &Path) {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE TABLE session_v2 ( \
                 id TEXT PRIMARY KEY, \
                 parent_id TEXT, \
                 cost REAL NOT NULL DEFAULT 0, \
                 tokens_input INTEGER NOT NULL DEFAULT 0, \
                 tokens_output INTEGER NOT NULL DEFAULT 0, \
                 tokens_reasoning INTEGER NOT NULL DEFAULT 0, \
                 tokens_cache_read INTEGER NOT NULL DEFAULT 0, \
                 tokens_cache_write INTEGER NOT NULL DEFAULT 0 \
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO session_v2 (id, parent_id, cost, tokens_input, tokens_output, \
             tokens_reasoning, tokens_cache_read, tokens_cache_write) \
             VALUES ('ses_root', NULL, 0.5, 10, 3, 2, 5, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO session_v2 (id, parent_id, cost, tokens_input, tokens_output, \
             tokens_reasoning, tokens_cache_read, tokens_cache_write) \
             VALUES ('ses_child', 'ses_root', 0.25, 20, 7, 4, 9, 2)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    #[test]
    fn derives_session_id_from_job_id() {
        assert_eq!(session_id("job_abc123"), "ses_domoabc123");
        assert_eq!(session_id("ses_existing"), "ses_domoses_existing");
    }

    #[test]
    fn maps_api_tokens_into_usage_record() {
        let mut totals = ApiTotals::default();
        totals.add(&serde_json::json!({
            "cost": 0.5,
            "tokens": {
                "input": 10,
                "output": 3,
                "reasoning": 2,
                "cache": { "read": 5, "write": 1 }
            }
        }));
        let usage = totals.into_record();
        assert_eq!(usage.input_tokens, Some(16));
        assert_eq!(usage.output_tokens, Some(5));
        assert_eq!(usage.cache_read_tokens, Some(5));
        assert_eq!(usage.cache_write_tokens, Some(1));
        assert_eq!(usage.reasoning_tokens, Some(2));
        assert_eq!(usage.cost, Some(0.5));
    }

    #[tokio::test]
    async fn sums_root_and_child_sessions() {
        let path = std::env::temp_dir().join(format!("domo-opencode-{}.db", uuid::Uuid::new_v4()));
        write_session_db(&path).await;
        let usage = fetch_usage_db(&path, "ses_root").await.unwrap().unwrap();
        assert_eq!(usage.input_tokens, Some(47));
        assert_eq!(usage.output_tokens, Some(16));
        assert_eq!(usage.cache_read_tokens, Some(14));
        assert_eq!(usage.cache_write_tokens, Some(3));
        assert_eq!(usage.reasoning_tokens, Some(6));
        assert_eq!(usage.cost, Some(0.75));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn missing_session_stays_unknown() {
        let path = std::env::temp_dir().join(format!("domo-opencode-{}.db", uuid::Uuid::new_v4()));
        write_session_db(&path).await;
        assert!(fetch_usage_db(&path, "ses_missing")
            .await
            .unwrap()
            .is_none());
        let _ = std::fs::remove_file(&path);
    }
}
