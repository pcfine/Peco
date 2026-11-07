//! Peco 默认空间的启动引导：幂等、坏团队不阻塞其它空间、保留空间不可删。
//!
//! 「幂等」在这里不是「跑两遍不报错」，而是「跑两遍后用户的改动还在、索引仍只有
//! 一行」—— 覆盖式安装与每次新建身份都能让前者变绿而后者变红。

mod support;

use std::path::{Path, PathBuf};

use peco_daemon::db::teams;
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

/// `peco` 模板里 `@master/agent.md` 的模板版本 —— 与模板一起演进，不写死数字。
fn template_master_version() -> i64 {
    let (_, bytes) = peco_agents::BuiltinTemplate::peco()
        .files
        .iter()
        .find(|(rel, _)| *rel == "agents/@master/agent.md")
        .expect("peco 模板必须带 @master");
    let raw = std::str::from_utf8(bytes).expect("模板 agent.md 必须是 UTF-8");
    i64::from(
        peco_core::agent::agent_config::parse_agent_md(raw)
            .expect("模板 agent.md 必须可解析")
            .0
            .template_version
            .expect("模板 agent.md 必须带 template_version"),
    )
}

fn private_memory_config_path(home: &Path) -> PathBuf {
    home.join("teams")
        .join("peco")
        .join("knowledge/@private_memory/kb_config.json")
}

/// `teams/peco/agents/*/` 下的 `agent.md.bak.*`；升版闸门动过文件才会出现。
fn agent_backups(home: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(agents) = std::fs::read_dir(home.join("teams/peco/agents")) else {
        return found;
    };
    for agent in agents.flatten() {
        let Ok(files) = std::fs::read_dir(agent.path()) else {
            continue;
        };
        for file in files.flatten() {
            if file.file_name().to_string_lossy().contains(".bak.") {
                found.push(file.path());
            }
        }
    }
    found
}

/// 连跑两次引导：模板文件与索引都必须收敛，用户改过的字节必须留着。
///
/// 「留着」的承诺只覆盖记录/知识文件：`agents/*/agent.md` 按 `template_version`
/// 升版覆盖（见 `space::bootstrap` 的模块说明），故这里改的是知识库配置。
///
/// 变异方法：铺模板时去掉「已存在则跳过」，第二次启动会把用户改过的 kb 配置
/// 覆盖回模板内容 → 本测试的字节相等断言变红。
#[tokio::test]
async fn bootstrapping_twice_keeps_user_edits_and_a_single_row() {
    let env = new_env(&[]).await;
    bootstrap_peco(&env.env).await;

    let agent = master_agent_path(&env.home);
    assert!(agent.is_file(), "peco 模板必须铺进 teams/peco");
    let template_bytes = std::fs::read(&agent).unwrap();
    assert!(!template_bytes.is_empty(), "模板文件不该是空的");

    let config = private_memory_config_path(&env.home);
    let edited = b"{\"edited_by_user\": true}\n";
    std::fs::write(&config, edited).unwrap();

    bootstrap_peco(&env.env).await;

    assert_eq!(
        std::fs::read(&config).unwrap(),
        edited,
        "第二次引导覆盖了用户改过的知识文件"
    );
    assert_eq!(
        std::fs::read(&agent).unwrap(),
        template_bytes,
        "版本没变的提示词必须与模板逐字节一致"
    );
    assert!(
        agent_backups(&env.home).is_empty(),
        "没有升版就不该留下备份: {:?}",
        agent_backups(&env.home)
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

/// 已装过的空间必须能拿到提示词修正：把 `@master/agent.md` 换回旧版本 + 旧正文，
/// 再引导一次 —— 内容升版成当前模板，旧文本进 `agent.md.bak.<旧版本>`。
///
/// 变异方法：`@master` 已存在就整步 return，文件会停在旧版本 → 内容比对变红。
#[tokio::test]
async fn an_installed_space_gets_the_newer_prompt_and_a_backup() {
    let env = new_env(&[]).await;
    bootstrap_peco(&env.env).await;

    let agent = master_agent_path(&env.home);
    let old = "---\nagent:\n  name: \"@master\"\n  description: \"旧版\"\n\
               template_version: 12\n---\n\n# 旧的唤醒字段名\n";
    std::fs::write(&agent, old).unwrap();

    bootstrap_peco(&env.env).await;

    let template = peco_agents::BuiltinTemplate::peco()
        .files
        .iter()
        .find(|(rel, _)| *rel == "agents/@master/agent.md")
        .map(|(_, bytes)| *bytes)
        .expect("peco 模板必须带 @master");
    assert_eq!(
        std::fs::read(&agent).unwrap(),
        template,
        "陈旧提示词必须被升版覆盖成当前模板"
    );

    let backups = agent_backups(&env.home);
    assert_eq!(backups.len(), 1, "升版只该留一份备份: {backups:?}");
    assert_eq!(
        std::fs::read_to_string(&backups[0]).unwrap(),
        old,
        "备份里必须是旧文本"
    );
    assert!(
        backups[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".bak.12"),
        "备份名要带旧版本号: {:?}",
        backups[0]
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

/// 旧版二进制装的空间：`agent.md` 已被升到当前模板，`team.json` 还停在上一版。
/// 索引行与详情接口都必须报磁盘上的版本，而 `team.json` 原字节不动 ——
/// 版本对齐是读出来的，不是重写用户文件换来的。
///
/// 清单里查不到 Peco 是刻意为之（保留团队不进团队列表），版本因此由详情接口回答。
///
/// 变异：`meta_from_dir` 只读 `team.json` → 索引行与详情接口都报旧版本变红。
#[tokio::test]
async fn an_upgraded_space_reports_the_version_on_disk() {
    let env = new_env(&[]).await;
    bootstrap_peco(&env.env).await;

    let installed = template_master_version();
    assert!(installed > 1, "夹具需要模板版本高于 1");
    let stale_version = installed - 1;

    let team_json = env.home.join("teams/peco/team.json");
    let stale = json!({
        "team_id": "peco",
        "display_name": "Peco",
        "template_version": stale_version,
        "created_at": "2024-01-01T00:00:00Z",
    });
    std::fs::write(&team_json, stale.to_string()).unwrap();
    let stale_bytes = std::fs::read(&team_json).unwrap();

    bootstrap_peco(&env.env).await;

    let row = teams::get(&env.pool, "peco")
        .await
        .unwrap()
        .expect("引导必须补索引行");
    assert_eq!(
        row.template_version, installed,
        "索引行必须记磁盘上的模板版本"
    );

    let listed = env.get("/api/teams").await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert!(
        listed
            .body
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["team_id"] != json!("peco")),
        "Peco 是保留团队，不得出现在团队清单里: {}",
        listed.body
    );

    let detail = env.get("/api/teams/peco").await;
    assert_eq!(detail.status, 200, "body={}", detail.body);
    assert_eq!(detail.body["template_version"], json!(installed));

    assert_eq!(
        std::fs::read(&team_json).unwrap(),
        stale_bytes,
        "版本对齐不该改写 team.json"
    );
}
