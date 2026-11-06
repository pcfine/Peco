//! `/api/peco` —— 前端定位默认总管空间的那一条。
//!
//! 身份由记录文件派生而非常量：用户改过 `session_id` 的空间重启后必须换一个
//! `ws_id`，否则台账里「Peco 发起的」分组会跟丢。

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::http::Method;
use peco_core::workspace::manifest::{self, MANIFEST_FILE};
use peco_daemon::space::peco::{self, PECO_TEAM_ID, PECO_WS_ID_SEED};
use serde_json::json;
use support::new_env;

fn write_marker(home: &Path, session_id: &str) {
    let work_dir = home.join("spaces").join(PECO_TEAM_ID).join("work");
    std::fs::create_dir_all(&work_dir).unwrap();
    let team = home.join("teams").join(PECO_TEAM_ID);
    let body = json!({
        "format": "peco-workspace",
        "schema": 1,
        "name": PECO_TEAM_ID,
        "description": "",
        "team": team.to_string_lossy(),
        "session_id": session_id,
        "created_at": "2026-02-11T09:14:22.531Z",
    });
    std::fs::write(work_dir.join(MANIFEST_FILE), body.to_string()).unwrap();
}

/// 「重启」= 重新解析身份后重建 `AppState`，再走一次真实路由。
async fn peco_view_as_after_restart(env: &support::TestEnv) -> serde_json::Value {
    let identity = Arc::new(peco::resolve(&env.home));
    let router = peco_daemon::http::router(env.app_state_with_peco(identity));
    let reply = support::call(&router, Method::GET, "/api/peco", None).await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    reply.body
}

/// 记录文件的 `session_id` 决定 `ws_id`：两次重启之间改它，返回值随之变化。
///
/// 变异：把 handler 的 `ws_id` 换成 `PECO_WS_ID_SEED` 常量 → 本测试变红。
#[tokio::test]
async fn ws_id_follows_the_marker_session_across_restarts() {
    let env = new_env(&[PECO_TEAM_ID]).await;
    write_marker(&env.home, "alpha");

    let first = peco_view_as_after_restart(&env).await;
    assert_eq!(
        first["ws_id"],
        json!("ws_alpha"),
        "ws_id 必须由 marker 派生"
    );
    assert_eq!(first["from_marker"], json!(true));
    assert_eq!(first["team_id"], json!(PECO_TEAM_ID));
    assert_eq!(first["reserved"], json!(true));
    assert_eq!(
        first["work_dir"],
        json!(env.home.join("spaces/peco/work").to_string_lossy()),
        "前端要按这个目录显示落点"
    );

    write_marker(&env.home, "beta");
    let second = peco_view_as_after_restart(&env).await;
    assert_eq!(
        second["ws_id"],
        json!("ws_beta"),
        "改过 session_id 之后重启必须换身份"
    );
    assert_ne!(first["ws_id"], second["ws_id"]);
}

/// 没有记录文件时退回种子，并如实报告 `from_marker=false`。
///
/// 变异：把 `from_marker` 写死成 true → 本测试变红。
#[tokio::test]
async fn a_missing_marker_reports_the_seed_id_and_no_marker() {
    let env = new_env(&[PECO_TEAM_ID]).await;

    let view = peco_view_as_after_restart(&env).await;

    assert_eq!(view["ws_id"], json!(PECO_WS_ID_SEED));
    assert_eq!(view["from_marker"], json!(false));
    assert_eq!(
        view["work_dir"],
        json!(env.home.join("spaces/peco/work").to_string_lossy())
    );
    // 种子派生的 id 与派生函数同源：两处各写一份字符串就会在这里分叉。
    assert_eq!(
        manifest::ws_id_from_session(peco::PECO_SESSION_SEED),
        PECO_WS_ID_SEED
    );
}
