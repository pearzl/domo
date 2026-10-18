use std::{collections::HashMap, sync::{atomic::{AtomicU64, Ordering}, Mutex}};
use tokio::{sync::{broadcast, Notify}, task::JoinHandle};
use serde_json::Value;

pub type Db = Mutex<rusqlite::Connection>;

pub struct HandleEntry {
    pub id: u64,
    pub handle: JoinHandle<()>,
}

pub struct App {
    pub db: Db,
    pub tx: broadcast::Sender<(String, Value)>,
    /// task_id -> live worker handles (execution / acceptance sessions)
    pub handles: Mutex<HashMap<String, Vec<HandleEntry>>>,
    pub handle_seq: AtomicU64,
    pub wake: Notify,
    /// M2: logical session key (e.g. `tsk_x_a1`, `planner-<prj>`) -> real
    /// opencode session id discovered at runtime (CLI may reject custom ids).
    pub sessions: Mutex<HashMap<String, String>>,
    /// M4.5 全局 agent 熔断（限流/配置错误临时禁用，跨项目生效）
    pub circuits: std::sync::Mutex<std::collections::HashMap<String, (i64, String)>>,
}

impl App {
    pub fn send(&self, kind: &str, v: Value) {
        let _ = self.tx.send((kind.to_string(), v));
    }

    /// Coalescing wakeup: notify_one stores at most one permit.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    pub fn alloc_handle_id(&self) -> u64 {
        self.handle_seq.fetch_add(1, Ordering::Relaxed)
    }

    pub fn register_handle(&self, task_id: &str, id: u64, h: JoinHandle<()>) {
        self.handles
            .lock()
            .unwrap()
            .entry(task_id.to_string())
            .or_default()
            .push(HandleEntry { id, handle: h });
    }

    pub fn has_live_handle(&self, task_id: &str) -> bool {
        self.handles
            .lock()
            .unwrap()
            .get(task_id)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// 某任务当前在途 worker 句柄数（M4：审计/冒烟断言「同一 task 永远只有一个在途执行」）。
    pub fn live_handles(&self, task_id: &str) -> usize {
        self.handles
            .lock()
            .unwrap()
            .get(task_id)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// Remove our own handle entry. Called from a worker's Drop guard.
    pub fn release_handle(&self, task_id: &str, id: u64) {
        let mut map = self.handles.lock().unwrap();
        if let Some(v) = map.get_mut(task_id) {
            v.retain(|e| e.id != id);
            if v.is_empty() {
                map.remove(task_id);
            }
        }
    }

    /// Abort all live workers of a task.
    pub fn abort_task_handles(&self, task_id: &str) {
        let hs = self.handles.lock().unwrap().remove(task_id);
        if let Some(hs) = hs {
            for e in hs {
                e.handle.abort();
            }
        }
    }

    pub fn abort_project_handles(&self, task_ids: &[String]) {
        let mut map = self.handles.lock().unwrap();
        for tid in task_ids {
            if let Some(hs) = map.remove(tid) {
                for e in hs {
                    e.handle.abort();
                }
            }
        }
    }
}

/// Removes its handle entry when the worker task finishes (normal exit or abort).
pub struct HandleGuard {
    pub state: std::sync::Arc<App>,
    pub task_id: String,
    pub id: u64,
}

impl Drop for HandleGuard {
    fn drop(&mut self) {
        self.state.release_handle(&self.task_id, self.id);
    }
}
