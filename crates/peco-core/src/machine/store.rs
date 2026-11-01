use std::collections::HashMap;
use std::sync::RwLock;

use thiserror::Error;

use super::model::{TaskFilter, TaskId, TaskSnapshot};

/// 快照副本的唯一落点。实现可换（内存/磁盘），调用点不变。
///
/// 同步方法：Hub 侧经 latest-wins 队列把写移出临界区，Store 的延迟不传染给 impl。
pub trait TaskStore: Send + Sync {
    fn put_snapshot(&self, snap: &TaskSnapshot) -> Result<(), StoreError>;

    fn load_snapshot(&self, id: &TaskId) -> Result<Option<TaskSnapshot>, StoreError>;

    fn list(&self, filter: &TaskFilter) -> Result<Vec<TaskId>, StoreError>;
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("store is temporarily unavailable: {reason}")]
    Transient { reason: String },
    #[error("store data is corrupt: {reason}")]
    Corrupt { reason: String },
    #[error("unsupported filter: {reason}")]
    Unsupported { reason: String },
}

/// 默认实现 = 关闭持久化：进程重启后 `load_snapshot` 返回 `None`。
#[derive(Default)]
pub struct InMemoryStore {
    snapshots: RwLock<HashMap<TaskId, TaskSnapshot>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn poisoned() -> StoreError {
        StoreError::Transient {
            reason: "in-memory store lock poisoned".to_string(),
        }
    }
}

impl TaskStore for InMemoryStore {
    fn put_snapshot(&self, snap: &TaskSnapshot) -> Result<(), StoreError> {
        self.snapshots
            .write()
            .map_err(|_| Self::poisoned())?
            .insert(snap.id, snap.clone());
        Ok(())
    }

    fn load_snapshot(&self, id: &TaskId) -> Result<Option<TaskSnapshot>, StoreError> {
        Ok(self
            .snapshots
            .read()
            .map_err(|_| Self::poisoned())?
            .get(id)
            .cloned())
    }

    fn list(&self, filter: &TaskFilter) -> Result<Vec<TaskId>, StoreError> {
        let snapshots = self.snapshots.read().map_err(|_| Self::poisoned())?;
        let mut matched: Vec<&TaskSnapshot> = snapshots
            .values()
            .filter(|snap| matches_filter(snap, filter))
            .collect();
        // 稳定顺序：两次调用结果一致，调用方与测试都不必依赖哈希遍历顺序。
        matched.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.0.cmp(&b.id.0))
        });
        Ok(matched.into_iter().map(|snap| snap.id).collect())
    }
}

/// `list` 与 Hub 侧「活跃槽 ∪ Store」共用同一份过滤语义，避免两处判据漂移。
pub(crate) fn matches_filter(snap: &TaskSnapshot, filter: &TaskFilter) -> bool {
    if filter.kind.as_deref().is_some_and(|kind| snap.kind != kind) {
        return false;
    }
    if filter.state.is_some_and(|state| snap.state != state) {
        return false;
    }
    if filter
        .scope
        .as_deref()
        .is_some_and(|scope| snap.scope.as_deref() != Some(scope))
    {
        return false;
    }
    if filter
        .created_after
        .is_some_and(|after| snap.created_at <= after)
    {
        return false;
    }
    filter
        .labels
        .iter()
        .all(|(key, value)| snap.labels.get(key) == Some(value))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::machine::model::{OutputView, Progress, TaskSource};
    use crate::machine::state::TaskState;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).expect("valid fixed timestamp")
    }

    fn snapshot(id: u128) -> TaskSnapshot {
        TaskSnapshot {
            id: TaskId(Uuid::from_u128(id)),
            kind: "echo".to_string(),
            state: TaskState::Submitted,
            state_detail: None,
            progress: Progress {
                phase: None,
                current: None,
                total: None,
            },
            output: OutputView {
                tail: Vec::new(),
                start_offset: 0,
                end_offset: 0,
            },
            outcome: None,
            artifacts: Vec::new(),
            deadline: None,
            source: TaskSource::External {
                principal: "tester".to_string(),
            },
            labels: BTreeMap::new(),
            scope: None,
            created_at: at(1_800_000_000),
            updated_at: at(1_800_000_000),
            last_seq: 0,
        }
    }

    fn no_filter() -> TaskFilter {
        TaskFilter {
            kind: None,
            state: None,
            labels: BTreeMap::new(),
            scope: None,
            created_after: None,
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    fn id_of(raw: u128) -> TaskId {
        TaskId(Uuid::from_u128(raw))
    }

    #[test]
    fn put_then_load_round_trips() {
        let store = InMemoryStore::new();
        let mut snap = snapshot(1);
        snap.kind = "shell".to_string();
        snap.state = TaskState::Working;
        snap.labels = labels(&[("tenant", "t1")]);
        snap.scope = Some("job-7".to_string());
        snap.last_seq = 3;

        store.put_snapshot(&snap).expect("put should succeed");
        assert_eq!(
            store.load_snapshot(&id_of(1)).expect("load should succeed"),
            Some(snap)
        );
    }

    #[test]
    fn load_missing_returns_none() {
        let store = InMemoryStore::new();
        assert_eq!(
            store
                .load_snapshot(&id_of(42))
                .expect("load should succeed"),
            None
        );
    }

    #[test]
    fn put_overwrites_by_id() {
        let store = InMemoryStore::new();
        let mut snap = snapshot(1);
        snap.state = TaskState::Working;
        store.put_snapshot(&snap).expect("first put should succeed");

        snap.state = TaskState::Completed;
        snap.last_seq = 9;
        store
            .put_snapshot(&snap)
            .expect("second put should succeed");

        let loaded = store
            .load_snapshot(&id_of(1))
            .expect("load should succeed")
            .expect("snapshot should exist");
        assert_eq!(loaded.state, TaskState::Completed);
        assert_eq!(loaded.last_seq, 9);
        assert_eq!(
            store.list(&no_filter()).expect("list should succeed"),
            vec![id_of(1)]
        );
    }

    #[test]
    fn list_with_empty_filter_returns_everything() {
        let store = InMemoryStore::new();
        for raw in [1, 2, 3] {
            store
                .put_snapshot(&snapshot(raw))
                .expect("put should succeed");
        }
        assert_eq!(
            store.list(&no_filter()).expect("list should succeed"),
            vec![id_of(1), id_of(2), id_of(3)]
        );
    }

    #[test]
    fn list_filters_by_kind() {
        let store = InMemoryStore::new();
        let mut shell = snapshot(1);
        shell.kind = "shell".to_string();
        let mut echo = snapshot(2);
        echo.kind = "echo".to_string();
        store.put_snapshot(&shell).expect("put should succeed");
        store.put_snapshot(&echo).expect("put should succeed");

        let mut filter = no_filter();
        filter.kind = Some("shell".to_string());
        assert_eq!(
            store.list(&filter).expect("list should succeed"),
            vec![id_of(1)]
        );
    }

    #[test]
    fn list_filters_by_state() {
        let store = InMemoryStore::new();
        let mut done = snapshot(1);
        done.state = TaskState::Completed;
        let mut running = snapshot(2);
        running.state = TaskState::Working;
        store.put_snapshot(&done).expect("put should succeed");
        store.put_snapshot(&running).expect("put should succeed");

        let mut filter = no_filter();
        filter.state = Some(TaskState::Completed);
        assert_eq!(
            store.list(&filter).expect("list should succeed"),
            vec![id_of(1)]
        );
    }

    #[test]
    fn list_requires_all_labels_to_match() {
        let store = InMemoryStore::new();
        let mut both = snapshot(1);
        both.labels = labels(&[("tenant", "t1"), ("kind", "batch")]);
        let mut one_only = snapshot(2);
        one_only.labels = labels(&[("tenant", "t1")]);
        let mut same_key_other_value = snapshot(3);
        same_key_other_value.labels = labels(&[("tenant", "t2"), ("kind", "batch")]);
        for snap in [&both, &one_only, &same_key_other_value] {
            store.put_snapshot(snap).expect("put should succeed");
        }

        let mut filter = no_filter();
        filter.labels = labels(&[("tenant", "t1"), ("kind", "batch")]);
        assert_eq!(
            store.list(&filter).expect("list should succeed"),
            vec![id_of(1)]
        );
    }

    #[test]
    fn list_filters_by_scope() {
        let store = InMemoryStore::new();
        let mut scoped = snapshot(1);
        scoped.scope = Some("job-7".to_string());
        let unscoped = snapshot(2);
        store.put_snapshot(&scoped).expect("put should succeed");
        store.put_snapshot(&unscoped).expect("put should succeed");

        let mut filter = no_filter();
        filter.scope = Some("job-7".to_string());
        assert_eq!(
            store.list(&filter).expect("list should succeed"),
            vec![id_of(1)]
        );
    }

    #[test]
    fn list_created_after_is_strictly_greater() {
        let store = InMemoryStore::new();
        let mut older = snapshot(1);
        older.created_at = at(1_000);
        let mut boundary = snapshot(2);
        boundary.created_at = at(2_000);
        let mut newer = snapshot(3);
        newer.created_at = at(3_000);
        for snap in [&older, &boundary, &newer] {
            store.put_snapshot(snap).expect("put should succeed");
        }

        let mut filter = no_filter();
        filter.created_after = Some(at(2_000));
        assert_eq!(
            store.list(&filter).expect("list should succeed"),
            vec![id_of(3)]
        );
    }

    #[test]
    fn list_orders_by_created_at_then_id() {
        let store = InMemoryStore::new();
        let mut late = snapshot(1);
        late.created_at = at(3_000);
        let mut early_high_id = snapshot(9);
        early_high_id.created_at = at(1_000);
        let mut early_low_id = snapshot(4);
        early_low_id.created_at = at(1_000);
        // 插入顺序与期望输出顺序无关。
        for snap in [&late, &early_high_id, &early_low_id] {
            store.put_snapshot(snap).expect("put should succeed");
        }

        let listed = store.list(&no_filter()).expect("list should succeed");
        assert_eq!(listed, vec![id_of(4), id_of(9), id_of(1)]);
        assert_eq!(
            store.list(&no_filter()).expect("list should succeed"),
            listed,
            "list must be stable across calls"
        );
    }
}
