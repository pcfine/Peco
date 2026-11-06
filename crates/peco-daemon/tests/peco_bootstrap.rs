//! Peco 默认空间的启动引导：幂等、坏团队不阻塞其它空间、保留空间不可删。
//!
//! 「幂等」在这里不是「跑两遍不报错」，而是「跑两遍后用户的改动还在、索引仍只有
//! 一行」—— 覆盖式安装与每次新建身份都能让前者变绿而后者变红。

mod support;

use std::path::{Path, PathBuf};

use peco_daemon::db::workspaces;
use peco_daemon::space::bootstrap::bootstrap_peco;
use peco_daemon::space::manifest;
use serde_json::json;
use support::new_env;

fn master_agent_path(home: &Path) -> PathBuf {
    home.join("teams")
        .join("peco")
        .join("agents/@master/agent.md")
}

/// 连跑两次引导：模板文件与索引都必须收敛，用户改过的字节必须留着。
///
/// 变异方法：铺模板时去掉「已存在则跳过」，第二次启动会把 `# edited by user`
/// 覆盖回模板内容 → 本测试的字节相等断言变红。
#[tokio::test]
async fn bootstrapping_twice_keeps_user_edits_and_a_single_row() {
    let env = new_env(&[]).await;
    bootstrap_peco(&env.env).await;

    let agent = master_agent_path(&env.home);
    assert!(agent.is_file(), "peco 模板必须铺进 teams/peco");
    let template_bytes = std::fs::read(&agent).unwrap();
    assert!(!template_bytes.is_empty(), "模板文件不该是空的");

    std::fs::write(&agent, b"# edited by user\n").unwrap();

    bootstrap_peco(&env.env).await;

    assert_eq!(
        std::fs::read(&agent).unwrap(),
        b"# edited by user\n",
        "第二次引导覆盖了用户改过的模板文件"
    );
    assert!(
        env.home.join("teams/peco/team.json").is_file(),
        "team.json 必须存在（rescan 的展示名来源）"
    );

    let rows = workspaces::list(&env.pool).await.unwrap();
    assert_eq!(rows.len(), 1, "引导必须收敛到一行空间: {rows:?}");
    assert_eq!(rows[0].id, "ws_peco");
    assert_eq!(rows[0].team_id, "peco");
    assert_eq!(
        rows[0].work_dir,
        env.home.join("spaces/peco/work").to_string_lossy()
    );
}

/// 记录文件的字段就是 Peco 的身份来源：团队路径写成非 canonical 会让打开
/// 直接 `team_path_mismatch`。
#[tokio::test]
async fn bootstrap_writes_the_marker_with_the_seed_identity() {
    let env = new_env(&[]).await;
    bootstrap_peco(&env.env).await;

    let work_dir = env.work_dir("peco");
    let marker = manifest::read(&work_dir)
        .unwrap()
        .expect("引导必须写出 marker");
    assert_eq!(marker.name, "Peco");
    assert_eq!(marker.description, "Peco 总管默认空间");
    assert_eq!(marker.schema, 1);
    assert_eq!(marker.session_id, "peco");
    assert_eq!(
        Path::new(&marker.team),
        std::fs::canonicalize(env.home.join("teams/peco")).unwrap(),
        "marker 的团队路径必须是 canonical 绝对路径"
    );

    assert!(
        env.is_open("ws_peco").await,
        "引导结束时 Peco 空间必须是打开状态"
    );
}

/// 坏团队目录只 warn：其它空间必须照常打开，进程不 panic。
///
/// 变异方法：把 `install_team_template` 的错误改成 `?` 直接上抛给 `run()`
/// → 启动链在这里中断，其它空间再也打不开。
#[tokio::test]
async fn a_broken_peco_team_does_not_block_other_spaces() {
    let env = new_env(&["research"]).await;
    // 用同名普通文件占住 `teams/peco`：建目录、拷模板、canonicalize 三步都会失败。
    std::fs::write(env.home.join("teams/peco"), b"not a directory").unwrap();

    bootstrap_peco(&env.env).await;

    let ws_id = env.open_space("alpha", "research").await;
    assert!(env.is_open(&ws_id).await, "其它空间仍必须能打开");
}

/// Peco 空间可以关闭，但不能删除；普通空间不受影响。
///
/// 变异方法：删掉 `purge_workspace` 的保留判定，DELETE 会走完删除流程返回 200
/// → 状态码与 code 断言变红。
#[tokio::test]
async fn the_peco_space_cannot_be_deleted_but_can_be_closed() {
    let env = new_env(&["research"]).await;
    bootstrap_peco(&env.env).await;
    let normal = env.open_space("alpha", "research").await;

    let listed = env.get("/api/spaces").await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    let items = listed.body.as_array().expect("列表必须是数组");
    let peco_row = items
        .iter()
        .find(|view| view["ws_id"] == json!("ws_peco"))
        .expect("Peco 空间必须在列表里");
    assert_eq!(peco_row["reserved"], json!(true), "前端据此隐藏删除按钮");
    let other = items
        .iter()
        .find(|view| view["ws_id"] == json!(normal))
        .unwrap();
    assert_eq!(other["reserved"], json!(false));

    let rejected = env.delete("/api/spaces/ws_peco").await;
    assert_eq!(rejected.status, 400, "body={}", rejected.body);
    assert_eq!(rejected.code(), "reserved_space");
    assert!(
        workspaces::by_id(&env.pool, "ws_peco")
            .await
            .unwrap()
            .is_some(),
        "拒绝删除后空间行必须还在"
    );

    let closed = env.post("/api/spaces/ws_peco/close", json!({})).await;
    assert_eq!(closed.status, 200, "body={}", closed.body);

    let deleted_again = env.delete("/api/spaces/ws_peco").await;
    assert_eq!(deleted_again.code(), "reserved_space");

    // 守卫不能写成「一律拒绝」：普通空间仍可删除。
    let purged = env.delete(&format!("/api/spaces/{normal}")).await;
    assert_eq!(purged.status, 200, "body={}", purged.body);
}
