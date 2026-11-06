//! 进程内唯一实例表（`WorkspaceHub`）：并发 ensure_open 只建一个实例。

mod support;

use std::sync::Arc;

use peco_core::tools::WorkspaceHandle;
use peco_daemon::space::open::map_tool_error;
use support::new_env;

/// P1-16：10 个并发 `ensure_open` 全拿到同一个实例，opener 只成功一次，
/// `INSERT workspaces` 只发生一次。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_concurrent_opens_create_exactly_one_instance() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("concurrent");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();

    let mut handles = Vec::new();
    for _ in 0..10 {
        let hub = env.hub.clone();
        let dir = work_dir.clone();
        let ws_id = ws_id.clone();
        handles.push(tokio::spawn(async move {
            hub.ensure_open(&dir, None)
                .await
                .map_err(|e| map_tool_error(e, &ws_id).code())
        }));
    }

    let mut instances: Vec<Arc<dyn WorkspaceHandle>> = Vec::new();
    for handle in handles {
        instances.push(handle.await.unwrap().expect("并发 open 不得失败"));
    }

    let first = &instances[0];
    for (index, handle) in instances.iter().enumerate() {
        assert!(
            Arc::ptr_eq(first, handle),
            "第 {index} 个并发 open 拿到了另一个实例"
        );
    }
    assert_eq!(env.opener.calls(), 1, "opener 必须只成功调用一次");
    assert_eq!(
        env.count_workspaces(&work_dir).await,
        1,
        "workspaces 行只允许一行"
    );
}

/// 打开失败后不得留下关闭态/半步缓存：该目录必须仍可再次尝试打开。
#[tokio::test]
async fn failed_open_leaves_the_space_openable() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("broken");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();

    // 破坏 marker 让首次打开必然失败（`marker_corrupt`）
    std::fs::write(
        work_dir.join(peco_daemon::space::manifest::MANIFEST_FILE),
        "{ not json",
    )
    .unwrap();
    let err = env
        .hub
        .ensure_open(&work_dir, None)
        .await
        .err()
        .expect("marker 损坏时必须失败");
    assert_eq!(map_tool_error(err, &ws_id).code(), "marker_corrupt");

    // 修好 marker 后必须能重新打开，而不是永久 busy
    std::fs::remove_file(work_dir.join(peco_daemon::space::manifest::MANIFEST_FILE)).unwrap();
    let opened = env.hub.ensure_open(&work_dir, None).await.unwrap();
    assert_eq!(opened.workspace().work_dir().root(), work_dir.as_path());
    assert_eq!(env.opener.calls(), 1, "第一次失败不应真正建出实例");
}

/// `begin_close` 期间拒绝新 `ensure_open`（`ws_busy`）。
#[tokio::test]
async fn opening_during_close_is_rejected_as_busy() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("closing");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.hub.ensure_open(&work_dir, None).await.unwrap();

    env.hub.begin_close(&work_dir).await.unwrap();
    let err = env
        .hub
        .ensure_open(&work_dir, None)
        .await
        .err()
        .expect("关闭中必须拒绝打开");
    assert_eq!(map_tool_error(err, &ws_id).code(), "ws_busy");
    assert!(!env.is_open(&ws_id).await, "关闭中不可见");

    env.hub.finish_close(&work_dir).await;
    // 摘掉实例之后可以重新打开
    let reopened = env.hub.ensure_open(&work_dir, None).await.unwrap();
    assert_eq!(reopened.workspace().work_dir().root(), work_dir.as_path());
    assert_eq!(env.opener.calls(), 2, "关闭后重开必须重新建实例");
}

/// 关闭一个目录不得阻塞另一个目录的打开。
#[tokio::test]
async fn closing_one_space_does_not_block_another() {
    let env = new_env(&["research"]).await;
    let first = env.work_dir("first");
    let second = env.work_dir("second");
    let first_id = env.resolve(&first, "research").await.unwrap();
    env.resolve(&second, "research").await.unwrap();

    env.hub.ensure_open(&first, None).await.unwrap();
    env.hub.begin_close(&first).await.unwrap();

    let err = env
        .hub
        .ensure_open(&first, None)
        .await
        .err()
        .expect("关闭中的目录必须 Busy");
    assert_eq!(map_tool_error(err, &first_id).code(), "ws_busy");

    let second_handle = env
        .hub
        .ensure_open(&second, None)
        .await
        .expect("另一个目录不应被关闭中的目录阻塞");
    assert_eq!(
        second_handle.workspace().work_dir().root(),
        second.as_path()
    );

    env.hub.finish_close(&first).await;
    env.hub
        .ensure_open(&first, None)
        .await
        .expect("finish_close 后必须可重开");
}
