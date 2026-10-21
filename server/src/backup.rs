//! P2 备份与恢复：SQLite 在线备份（VACUUM INTO）+ 可选工程工作区 tar 快照。
//!
//! - 创建：`POST /api/backups`，备份写入 `<data>/backups/`，并落 manifest。
//! - 列表/删除：只操作 manifest 中登记的文件，拒绝路径穿越。
//! - 恢复：**不在运行中替换数据库**。请求恢复只写 `restore-request.json`，
//!   下次启动时在打开数据库前执行：先备份当前库，再替换库文件；可选解包工作区。
//! - 所有路径都在 data 目录内；备份是只读快照，不修改源库。
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db;
use crate::models::Engineering;

pub fn backup_dir() -> PathBuf {
    db::data_dir().join("backups")
}

fn pending_path() -> PathBuf {
    db::data_dir().join("restore-request.json")
}

fn now_ms() -> i64 {
    db::now()
}

fn next_name() -> String {
    format!("bk-{}", now_ms())
}

fn manifest(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.json"))
}

/// 创建备份。`include_workspace` 且工程有 workspace_path 时额外打一个 tar.gz。
pub fn create(
    c: &Connection,
    engineering: Option<&Engineering>,
    include_workspace: bool,
) -> Result<Value, String> {
    create_in(c, &backup_dir(), engineering, include_workspace)
}

fn create_in(
    c: &Connection,
    dir: &Path,
    engineering: Option<&Engineering>,
    include_workspace: bool,
) -> Result<Value, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create backup dir: {e}"))?;
    let name = next_name();
    let db_file = format!("{name}.db");
    let db_path = dir.join(&db_file);
    c.execute("VACUUM INTO ?1", rusqlite::params![db_path.to_string_lossy()])
        .map_err(|e| format!("VACUUM INTO failed: {e}"))?;

    let mut workspace_file: Option<String> = None;
    let mut workspace_parent: Option<String> = None;
    if include_workspace {
        if let Some(e) = engineering {
            if let Some(root) = e.workspace_path.as_deref().map(Path::new) {
                if root.is_dir() {
                    let base = root.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    let parent = root.parent().unwrap_or_else(|| Path::new("/"));
                    let tar = format!("{name}-workspace.tgz");
                    let out = Command::new("tar")
                        .arg("-czf").arg(dir.join(&tar))
                        .arg("-C").arg(parent)
                        .arg(&base)
                        .output()
                        .map_err(|e| format!("tar workspace: {e}"))?;
                    if !out.status.success() {
                        let _ = std::fs::remove_file(&db_path);
                        return Err(format!("tar workspace failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
                    }
                    workspace_file = Some(tar);
                    // 记录解包目标目录（恢复时 `tar -C <parent>` 还原原路径）
                    workspace_parent = Some(parent.display().to_string());
                }
            }
        }
    }
    let size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    let info = json!({
        "name": name,
        "created_at": now_ms(),
        "db": db_file,
        "workspace": workspace_file,
        "workspace_parent": workspace_parent,
        "engineering_id": engineering.map(|e| e.id.clone()),
        "engineering_name": engineering.map(|e| e.name.clone()),
        "size": size,
        "format": 1,
    });
    std::fs::write(manifest(dir, &name), serde_json::to_string_pretty(&info).unwrap_or_default())
        .map_err(|e| format!("write manifest: {e}"))?;
    Ok(info)
}

pub fn list() -> Vec<Value> {
    list_in(&backup_dir())
}

fn list_in(dir: &Path) -> Vec<Value> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") { continue; }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if v.get("name").and_then(|x| x.as_str()).is_some() {
                out.push(v);
            }
        }
    }
    out.sort_by_key(|v| -(v["created_at"].as_i64().unwrap_or(0)));
    out
}

fn find_in(dir: &Path, name: &str) -> Result<Value, String> {
    if name.is_empty() || name.contains('/') || name.contains("..") {
        return Err("invalid backup name".into());
    }
    let text = std::fs::read_to_string(manifest(dir, name)).map_err(|_| "backup not found".to_string())?;
    serde_json::from_str(&text).map_err(|e| format!("invalid manifest: {e}"))
}

pub fn remove(name: &str) -> Result<(), String> {
    remove_in(&backup_dir(), name)
}

fn remove_in(dir: &Path, name: &str) -> Result<(), String> {
    let info = find_in(dir, name)?;
    for key in ["db", "workspace"] {
        if let Some(f) = info[key].as_str() {
            let _ = std::fs::remove_file(dir.join(f));
        }
    }
    let _ = std::fs::remove_file(manifest(dir, name));
    Ok(())
}

/// 请求恢复：只写标记，由下次启动在开库前执行。
pub fn request_restore(name: &str) -> Result<Value, String> {
    let info = find_in(&backup_dir(), name)?;
    let req = json!({
        "name": name,
        "db": info["db"],
        "workspace": info["workspace"],
        "requested_at": now_ms(),
    });
    if let Some(parent) = pending_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&pending_path(), serde_json::to_string_pretty(&req).unwrap_or_default())
        .map_err(|e| format!("write restore request: {e}"))?;
    Ok(json!({"requested": true, "restart_required": true, "backup": name}))
}

/// 启动时执行待恢复请求。返回 Some(备份名) 表示已执行。
pub fn apply_pending_restore(db_path: &str) -> Option<String> {
    apply_pending_restore_in(db_path, &pending_path(), &backup_dir())
}

fn apply_pending_restore_in(db_path: &str, pending: &Path, dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(pending).ok()?;
    let req: Value = serde_json::from_str(&text).ok()?;
    let name = req["name"].as_str().unwrap_or("").to_string();
    let result = (|| -> Result<(), String> {
        let info = find_in(dir, &name)?;
        let db_file = info["db"].as_str().ok_or("manifest db missing")?;
        let src = dir.join(db_file);
        if !src.exists() { return Err("backup db missing".into()); }
        // 先给当前库做安全备份（文件级；启动早期尚未打开新连接）。
        let current = Path::new(db_path);
        if current.exists() {
            let safety = dir.join(format!("pre-restore-{}.db", now_ms()));
            std::fs::copy(current, &safety).map_err(|e| format!("safety copy: {e}"))?;
        }
        std::fs::copy(&src, current).map_err(|e| format!("restore db: {e}"))?;
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{db_path}{suffix}"));
        }
        if let Some(wf) = info["workspace"].as_str() {
            let tar = dir.join(wf);
            let parent = info["workspace_parent"].as_str().unwrap_or("");
            if parent.is_empty() || !Path::new(parent).is_dir() {
                return Err("workspace_parent missing or not a directory".into());
            }
            if tar.exists() {
                let out = Command::new("tar").arg("-xzf").arg(&tar).arg("-C").arg(parent).output()
                    .map_err(|e| format!("extract workspace: {e}"))?;
                if !out.status.success() {
                    return Err(format!("extract workspace failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
                }
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            let _ = std::fs::remove_file(pending);
            tracing::warn!(backup = %name, "database restored from backup at startup");
            Some(name)
        }
        Err(e) => {
            let failed = pending.with_extension("failed.json");
            let _ = std::fs::rename(pending, failed);
            tracing::error!(backup = %name, error = %e, "restore request failed; removed marker");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("domo-backup-{tag}-{}-{}", std::process::id(), uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn backup_create_list_remove_and_restore_marker() {
        let dir = tmp("crud");
        let dbp = dir.join("domo.db");
        let c = Connection::open(&dbp).unwrap();
        crate::db::migrate_for_tests(&c);
        c.execute("CREATE TABLE IF NOT EXISTS t (x INT)", []).unwrap();
        let info = create_in(&c, &dir.join("backups"), None, false).unwrap();
        let name = info["name"].as_str().unwrap().to_string();
        assert_eq!(list_in(&dir.join("backups")).len(), 1);
        assert!(dir.join("backups").join(format!("{name}.db")).exists());
        remove_in(&dir.join("backups"), &name).unwrap();
        assert!(list_in(&dir.join("backups")).is_empty());

        let info = create_in(&c, &dir.join("backups"), None, false).unwrap();
        let name = info["name"].as_str().unwrap().to_string();
        let pending = dir.join("restore-request.json");
        std::fs::write(&pending, serde_json::to_string(&json!({"name": name})).unwrap()).unwrap();
        // 模拟库文件存在；恢复后标记被清理
        assert!(apply_pending_restore_in(dbp.to_str().unwrap(), &pending, &dir.join("backups")).is_some());
        assert!(!pending.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
