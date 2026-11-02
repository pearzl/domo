//! OpenCode session integration. Every job owns a dedicated session. After the
//! process exits, the session ledger is the authoritative source for tokens and
//! cost: it includes every step plus title and compaction usage. Child subagent
//! sessions are separate rows and are summed through `parent_id`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::usage::UsageRecord;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const DB_PATH_MISS_TTL: Duration = Duration::from_secs(30);

static DB_PATH: OnceLock<PathBuf> = OnceLock::new();
static DB_PATH_MISS: OnceLock<tokio::sync::Mutex<Option<Instant>>> = OnceLock::new();

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
    let path = cached_db_path().await?;
    match fetch_usage_db(&path, session_id).await {
        Ok(usage) => usage,
        Err(error) => {
            tracing::debug!(session_id, error = %error, "从 OpenCode 数据库读取用量失败");
            None
        }
    }
}

async fn cached_db_path() -> Option<PathBuf> {
    if let Some(path) = DB_PATH.get() {
        return Some(path.clone());
    }
    let miss = DB_PATH_MISS.get_or_init(|| tokio::sync::Mutex::new(None));
    {
        let guard = miss.lock().await;
        if let Some(at) = *guard {
            if at.elapsed() < DB_PATH_MISS_TTL {
                return None;
            }
        }
    }
    match db_path().await {
        Some(path) => {
            let _ = DB_PATH.set(path.clone());
            Some(path)
        }
        None => {
            *miss.lock().await = Some(Instant::now());
            None
        }
    }
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

async fn readonly_pool(path: &Path) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .busy_timeout(Duration::from_secs(2));
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("打开 OpenCode 数据库失败: {}", path.display()))
}

async fn fetch_usage_db(path: &Path, session_id: &str) -> Result<Option<UsageRecord>> {
    let pool = readonly_pool(path).await?;
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

async fn command_output(bin: &str, args: &[&str]) -> Option<std::process::Output> {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let output = tokio::time::timeout(
        COMMAND_TIMEOUT,
        crate::platform::command(bin, &args).output(),
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
