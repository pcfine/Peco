//! `/api/teams/{team_id}` —— 团队装配页的只读详情。
//!
//! 五类能力各有自己的真源：agent / skill / mcp 在能力根的磁盘上，知识库看
//! `knowledge_bases`（团队维度），绑定空间看 `workspaces`。团队页要在**没有任何
//! 空间打开**的情况下把它们一次答全。

mod support;

use peco_daemon::db::documents::{self, CreateDocumentParams};
use peco_daemon::db::knowledge_bases::{self, CreateKbParams};
use peco_daemon::db::teams::{self, NewTeam};
use serde_json::{Value, json};
use support::new_env;

fn write_agent(env: &support::TestEnv, team: &str, name: &str, description: &str) {
    let dir = env.home.join("teams").join(team).join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.md"),
        format!(
            "---\nagent:\n  name: \"{name}\"\n  description: \"{description}\"\n---\n\n# prompt\n"
        ),
    )
    .unwrap();
}

/// 一份带模板版本的 `@master/agent.md`。
fn write_versioned_master(env: &support::TestEnv, team: &str, version: u32) {
    let dir = env.home.join("teams").join(team).join("agents/@master");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.md"),
        format!(
            "---\nagent:\n  name: \"@master\"\n  description: \"总管\"\n\
             template_version: {version}\n---\n\n# prompt\n"
        ),
    )
    .unwrap();
}

fn write_team_json(env: &support::TestEnv, team: &str, display_name: &str, version: i64) {
    let body = json!({
        "team_id": team,
        "display_name": display_name,
        "template_version": version,
        "created_at": "2024-01-01T00:00:00Z",
    });
    std::fs::write(
        env.home.join("teams").join(team).join("team.json"),
        body.to_string(),
    )
    .unwrap();
}

fn write_skill(env: &support::TestEnv, team: &str, name: &str) {
    let dir = env.home.join("teams").join(team).join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: \"{name}\"\ndescription: \"d\"\n---\n\n# skill\n"),
    )
    .unwrap();
}

fn write_mcpconfig(env: &support::TestEnv, team: &str, servers: &[&str]) {
    let mut map = serde_json::Map::new();
    for name in servers {
        map.insert(
            (*name).to_string(),
            json!({"transport": "stdio", "command": "echo"}),
        );
    }
    let body = json!({ "mcpServers": map });
    std::fs::write(
        env.home.join("teams").join(team).join("mcpconfig.json"),
        body.to_string(),
    )
    .unwrap();
}

fn names(items: &Value) -> Vec<String> {
    items
        .as_array()
        .unwrap_or_else(|| panic!("应是数组: {items}"))
        .iter()
        .map(|item| item.as_str().expect("应是字符串").to_string())
        .collect()
}

/// 2 agent / 1 skill / 1 MCP server / 1 知识库（2 文档）/ 2 绑定空间，逐项核对。
///
/// 变异：
/// - agents 从 `agents/` 目录名取而不看 `agent.md` → 本测试仍绿，但下面
///   「空壳团队」用例会把 `has_master` 判据暴露出来；
/// - `document_count` 写死 0 → KB 断言变红；
/// - `spaces` 换成全库空间列表 → 数量断言变红。
#[tokio::test]
async fn detail_reports_every_capability_of_a_team() {
    let env = new_env(&["research", "writing"]).await;
    write_agent(&env, "research", "@master", "总管");
    write_agent(&env, "research", "@coder", "写代码");
    write_skill(&env, "research", "code-review");
    write_mcpconfig(&env, "research", &["playwright"]);
    // 别的团队也得有同样的能力，才证明过滤条件真的在起作用。
    write_agent(&env, "writing", "@master", "写作总管");

    knowledge_bases::insert(
        &env.pool,
        &CreateKbParams {
            id: "kb_research".into(),
            team_id: "research".into(),
            name: "kb1".into(),
            description: String::new(),
        },
    )
    .await
    .unwrap();
    for index in 0..2 {
        documents::upsert_pending(
            &env.pool,
            &CreateDocumentParams {
                kb_id: "kb_research".into(),
                filename: format!("d{index}.md"),
                filepath: format!("/kb/d{index}.md"),
                file_size: 10,
                mime_type: "text/markdown".into(),
                relative_path: format!("d{index}.md"),
            },
        )
        .await
        .unwrap();
    }
    knowledge_bases::insert(
        &env.pool,
        &CreateKbParams {
            id: "kb_writing".into(),
            team_id: "writing".into(),
            name: "other".into(),
            description: String::new(),
        },
    )
    .await
    .unwrap();

    let first = env.open_space("research-a", "research").await;
    let second = env.open_space("research-b", "research").await;
    let _elsewhere = env.open_space("writing-a", "writing").await;

    let reply = env.get("/api/teams/research").await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["id"], json!("research"));
    assert_eq!(reply.body["display_name"], json!("research"));
    assert_eq!(
        reply.body["root"],
        json!(env.home.join("teams/research").to_string_lossy()),
        "装配页要显示能力根"
    );
    assert_eq!(reply.body["has_master"], json!(true));

    let agents = reply.body["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 2, "agents={}", reply.body["agents"]);
    assert_eq!(agents[0]["name"], json!("@coder"));
    assert_eq!(agents[1]["name"], json!("@master"));
    assert_eq!(
        agents[1]["description"],
        json!("总管"),
        "描述来自 agent.md 的 frontmatter"
    );

    assert_eq!(names(&reply.body["skills"]), vec!["code-review"]);
    assert_eq!(names(&reply.body["mcp_servers"]), vec!["playwright"]);

    let kbs = reply.body["knowledge_bases"].as_array().unwrap();
    assert_eq!(kbs.len(), 1, "只能有本团队的知识库: {}", reply.body);
    assert_eq!(kbs[0]["name"], json!("kb1"));
    assert_eq!(kbs[0]["document_count"], json!(2));

    let spaces = reply.body["spaces"].as_array().unwrap();
    let mut ws_ids: Vec<&str> = spaces
        .iter()
        .map(|space| space["ws_id"].as_str().unwrap())
        .collect();
    ws_ids.sort();
    let mut expected = vec![first.as_str(), second.as_str()];
    expected.sort();
    assert_eq!(ws_ids, expected, "只列本团队绑定的空间");
    assert_eq!(spaces[0]["closed_at"], Value::Null);
}

/// 目录与索引行都没有 ⇒ `404 team_not_found`。
///
/// 变异：删掉 handler 里的存在性判断（缺行就按目录名兜底）→ 本测试变红。
#[tokio::test]
async fn unknown_team_is_404() {
    let env = new_env(&["research"]).await;

    let reply = env.get("/api/teams/ghost").await;

    assert_eq!(reply.status, 404, "body={}", reply.body);
    assert_eq!(reply.code(), "team_not_found");
}

/// 只有目录、没有任何能力文件的空壳团队仍是 200，`has_master=false`。
///
/// 变异：把 `has_master` 写死 true（或按目录是否存在判断）→ 本测试变红。
#[tokio::test]
async fn an_empty_team_has_no_master_and_still_answers() {
    let env = new_env(&["blank"]).await;

    let reply = env.get("/api/teams/blank").await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["has_master"], json!(false));
    assert_eq!(reply.body["agents"], json!([]));
    assert_eq!(reply.body["skills"], json!([]));
    assert_eq!(
        reply.body["mcp_servers"],
        json!([]),
        "缺 mcpconfig.json 即空"
    );
    assert_eq!(reply.body["knowledge_bases"], json!([]));
    assert_eq!(reply.body["spaces"], json!([]));
}

/// 模板升过级但 `team.json` 与索引行都停在旧版本：详情必须报磁盘上的 13。
///
/// 变异：详情只信索引行的 `template_version` → 下面拿到 12 变红。
#[tokio::test]
async fn detail_reports_the_version_installed_on_disk() {
    let env = new_env(&["research"]).await;
    write_versioned_master(&env, "research", 13);
    write_team_json(&env, "research", "研究组", 12);
    teams::upsert(
        &env.pool,
        &NewTeam {
            id: "research",
            display_name: "研究组",
            template_version: 12,
        },
    )
    .await
    .unwrap();

    let reply = env.get("/api/teams/research").await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(
        reply.body["template_version"],
        json!(13),
        "详情必须报磁盘上的模板版本"
    );
    assert_eq!(
        reply.body["display_name"],
        json!("研究组"),
        "展示名仍信索引行"
    );
    assert_eq!(reply.body["has_master"], json!(true));
}

/// 缺 `team.json`（外部工具手搭的团队）也要按磁盘上报版本，展示名退回目录名。
#[tokio::test]
async fn detail_falls_back_to_the_disk_version_without_team_json() {
    let env = new_env(&["research"]).await;
    write_versioned_master(&env, "research", 13);

    let reply = env.get("/api/teams/research").await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["display_name"], json!("research"));
    assert_eq!(reply.body["template_version"], json!(13));
}
