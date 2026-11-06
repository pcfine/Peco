//! Peco 空间的动态上下文：清单格式、截断、注入范围与失败降级。
//!
//! 清单会进 Peco 的 system prompt 并当作派发目标的来源：列出一个打不开的空间
//! 就是给出一条必然失败的任务，因此「列谁、不列谁」是行为契约而非展示细节。

mod support;

use peco_core::agent::DynamicContext;
use peco_daemon::db;
use peco_daemon::db::workspaces;
use peco_daemon::dynamic::SpaceCatalogContext;
use peco_daemon::space::bootstrap::bootstrap_peco;
use serde_json::json;
use support::{TestEnv, new_env};

fn catalog(env: &TestEnv) -> SpaceCatalogContext {
    SpaceCatalogContext::new(
        env.pool.clone(),
        env.hub.clone(),
        env.env.peco.clone(),
        env.home.clone(),
    )
}

async fn install_teams(env: &TestEnv, teams: &[&str]) {
    for team in teams {
        peco_daemon::team::create_team(&env.env, team, None, Some("team_default"))
            .await
            .unwrap();
    }
}

/// 已关闭空间与「只有索引、从未打开」的空间都不能出现：前者实例已销毁，后者
/// 派发必然失败。两个条件各有独立可观察的反例（`gone` / `indexed` / `stale`）。
///
/// 变异方法：去掉 `closed_at.is_some()` 过滤 → `stale` 行出现；
/// 去掉 `hub.is_open()` 过滤 → `indexed` 行出现。
#[tokio::test]
async fn the_catalog_lists_only_dispatchable_spaces_and_installed_teams() {
    let env = new_env(&[]).await;
    bootstrap_peco(&env.env).await;
    install_teams(&env, &["research"]).await;

    let busy = env.open_space("busy", "research").await;
    env.insert_dispatch(&busy, "t_busy", "run_machine_task", &busy, "working")
        .await;

    let gone = env.open_space("gone", "research").await;
    let closed = env
        .post(&format!("/api/spaces/{gone}/close"), json!({}))
        .await;
    assert_eq!(closed.status, 200, "body={}", closed.body);

    let indexed_dir = env.work_dir("indexed");
    let indexed = env
        .resolve(&indexed_dir, "research")
        .await
        .expect("补索引行不该失败");

    // 索引与实例表可以分叉：这里直接把 closed_at 写上，实例仍活着。
    // 清单要求两个条件同时满足，否则会向 Peco 报一个「已关闭但还在跑」的空间。
    let stale = env.open_space("stale", "research").await;
    workspaces::set_closed_at(&env.pool, &stale).await.unwrap();
    assert!(
        env.is_open(&stale).await,
        "夹具前提：实例仍在 Hub 里，只能靠 closed_at 筛掉"
    );

    let text = catalog(&env)
        .query("随便问一句")
        .await
        .expect("清单必须生成");

    assert!(text.contains("## 工作空间"), "{text}");
    assert!(text.contains("## 团队"), "{text}");
    assert!(text.contains(&format!("ws_id={busy}")), "{text}");
    assert!(
        text.contains("inflight=1"),
        "在飞任务数必须与关闭门槛同口径: {text}"
    );
    assert!(
        !text.contains(&format!("ws_id={gone}")),
        "已关闭的空间不该出现: {text}"
    );
    assert!(
        !text.contains(&format!("ws_id={indexed}")),
        "只有索引没有实例的空间不该出现: {text}"
    );
    assert!(
        !text.contains(&format!("ws_id={stale}")),
        "closed_at 已写的空间不该出现: {text}"
    );

    let current: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("[当前]"))
        .collect();
    assert_eq!(current.len(), 1, "[当前] 只能标一行: {text}");
    assert!(
        current[0].contains("ws_id=ws_peco"),
        "[当前] 必须标在 Peco 自己的行上: {text}"
    );
    assert!(
        text.contains(&format!(
            "共 2 个空间；当前空间 dir={} ws_id=ws_peco",
            env.work_dir("peco").display()
        )),
        "{text}"
    );
    assert!(
        text.contains("team=research root=") && text.contains("teams/research"),
        "团队必须带绝对根路径: {text}"
    );
    assert!(
        text.chars().count() <= 1200,
        "清单超长: {}",
        text.chars().count()
    );
}

/// 空壳团队（没有 `agents/@master/agent.md`）不能进清单：`create_workspace` 会
/// 拿着这个路径去打开一个没有总管的团队。
#[tokio::test]
async fn teams_without_a_master_agent_are_not_listed() {
    let env = new_env(&["empty-shell"]).await;
    bootstrap_peco(&env.env).await;
    install_teams(&env, &["research"]).await;

    let text = catalog(&env).query("").await.expect("清单必须生成");
    assert!(text.contains("team=research"), "{text}");
    assert!(!text.contains("team=empty-shell"), "{text}");
}

/// 60 个打开的空间必然超出 1200 字符：按最近活跃截断，并显式告知还有多少个。
///
/// 变异方法：去掉 `render()` 的收缩循环 → 长度断言变红；去掉「另有 ... 未列出」
/// → 关键字断言变红（截断而不告知，等于让 Peco 以为清单完整）。
#[tokio::test]
async fn the_catalog_truncates_and_says_how_many_are_missing() {
    let env = new_env(&[]).await;
    bootstrap_peco(&env.env).await;
    install_teams(&env, &["research"]).await;

    for index in 0..60 {
        env.open_space(&format!("s{index:02}"), "research").await;
    }

    let text = catalog(&env).query("").await.expect("清单必须生成");
    let length = text.chars().count();
    assert!(length <= 1200, "清单超长: {length} 字符");
    assert!(text.contains("另有"), "截断必须告知: {text}");
    assert!(text.contains("个空间未列出"), "{text}");
    assert!(
        text.contains("当前空间 dir="),
        "截断后仍必须自报当前空间，否则 Peco 会给自己派错目标: {text}"
    );
}

/// 清单只注入 Peco 自己的空间：团队总管看到别的空间会把任务派到不属于自己的地方。
///
/// 变异方法：去掉 `is_peco_space` 判定（所有空间都注入）→ 团队空间那条变红。
#[tokio::test]
async fn only_the_peco_space_gets_the_catalog_but_every_space_keeps_the_hook() {
    let env = new_env(&["research"]).await;
    bootstrap_peco(&env.env).await;
    let team_space = env.open_space("alpha", "research").await;
    let state = env.app_state();

    let peco_config = state.looper_config(&env.env.peco.ws_id, "peco", env.work_dir("peco"));
    assert!(
        peco_config.dynamic_context.is_some(),
        "Peco 空间必须拿到清单"
    );
    assert!(!peco_config.hooks.is_empty(), "动态上下文不能顶掉观察 hook");

    let team_config =
        state.looper_config(&team_space, "research", env.work_dir_of(&team_space).await);
    assert!(
        team_config.dynamic_context.is_none(),
        "团队空间不该看到其它空间的清单"
    );
    assert!(!team_config.hooks.is_empty(), "团队空间同样需要观察 hook");
}

/// 清单失败不能让一轮对话中止：DB 不可用时返回 `None`，不 panic。
///
/// 变异方法：把 `catalog()` 的错误改成 `expect`/`unwrap` → 本测试 panic。
#[tokio::test]
async fn a_broken_database_yields_no_catalog_instead_of_an_error() {
    let env = new_env(&[]).await;
    bootstrap_peco(&env.env).await;

    // 独立连接池：关掉它不影响夹具自己的池，失败原因单一。
    let doomed = db::connect(&env.config.db_url()).await.unwrap();
    let context = SpaceCatalogContext::new(
        doomed.clone(),
        env.hub.clone(),
        env.env.peco.clone(),
        env.home.clone(),
    );
    doomed.close().await;

    assert!(
        context.query("hi").await.is_none(),
        "清单失败必须降级为 None"
    );
}
