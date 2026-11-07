//! `/api/teams` 的 HTTP 语义：清单来源（目录 ∪ 行）、模板安装、重名与非法 id。

mod support;

use peco_daemon::db::teams::{self, NewTeam};
use peco_daemon::space::peco::PECO_TEAM_ID;
use serde_json::json;
use support::new_env;

/// 一份带模板版本的 `@master/agent.md` —— 磁盘上的版本才是装着的模板。
fn write_master_agent(env: &support::TestEnv, team: &str, version: u32) {
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

/// `team.json` 是首装时的快照：模板升级不会改写它。
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

fn team_ids(reply: &support::HttpReply) -> Vec<String> {
    reply
        .body
        .as_array()
        .unwrap_or_else(|| panic!("清单应是数组: {}", reply.body))
        .iter()
        .map(|item| item["team_id"].as_str().unwrap().to_string())
        .collect()
}

/// 目录有、索引行没有的团队也要出现在清单里（索引可重建，目录是权威）。
#[tokio::test]
async fn list_reports_directory_only_teams() {
    let env = new_env(&["research", "writing"]).await;

    let reply = env.get("/api/teams").await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(team_ids(&reply), vec!["research", "writing"]);
    assert_eq!(reply.body[0]["display_name"], json!("research"));
    assert_eq!(reply.body[0]["agents"], json!([]), "空目录没有 agent");
}

/// 模板安装：`@master` 与私有记忆库落到 `teams/<id>/`，索引行与 `team.json` 同步。
#[tokio::test]
async fn create_team_installs_template_and_index_row() {
    let env = new_env(&["research"]).await;

    let reply = env
        .post(
            "/api/teams",
            json!({"id": "writing", "display_name": "写作", "from_template": "team_default"}),
        )
        .await;

    assert_eq!(reply.status, 201, "body={}", reply.body);
    assert_eq!(reply.body["team_id"], json!("writing"));
    assert_eq!(reply.body["display_name"], json!("写作"));
    assert_eq!(reply.body["template_version"], json!(1));
    assert_eq!(reply.body["agents"], json!(["@master"]));
    assert_eq!(reply.body["bound_spaces"], json!([]));

    let root = env.home.join("teams/writing");
    assert!(
        root.join("agents/@master/agent.md").is_file(),
        "模板 agent 必须落盘"
    );
    assert!(
        root.join("knowledge/@private_memory/kb_config.json")
            .is_file(),
        "模板知识库配置必须落盘"
    );
    let team_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("team.json")).unwrap()).unwrap();
    assert_eq!(team_json["display_name"], json!("写作"));
    assert_eq!(team_json["template_version"], json!(1));

    let row: (String, i64) =
        sqlx::query_as("SELECT display_name, template_version FROM teams WHERE id = 'writing'")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(row, ("写作".to_string(), 1));

    // 新建的团队可以立刻绑定空间：目录即能力根，open 不该报 team_missing。
    let work_dir = env.work_dir("with-writing");
    let space = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "writing"}),
        )
        .await;
    assert_eq!(space.status, 201, "body={}", space.body);

    let listed = env.get("/api/teams").await;
    let writing = listed
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["team_id"] == json!("writing"))
        .expect("新建团队应在清单里");
    assert_eq!(
        writing["bound_spaces"],
        json!([space.body["ws_id"].as_str().unwrap()]),
        "绑定的空间要出现团队页上"
    );
}

/// 空团队：`from_template` 缺省时只建目录，不装任何能力文件。
#[tokio::test]
async fn create_empty_team_without_template() {
    let env = new_env(&["research"]).await;

    let reply = env.post("/api/teams", json!({"id": "blank"})).await;

    assert_eq!(reply.status, 201, "body={}", reply.body);
    assert_eq!(reply.body["template_version"], json!(0));
    assert_eq!(reply.body["agents"], json!([]));
    assert!(env.home.join("teams/blank").is_dir());
    assert!(!env.home.join("teams/blank/agents/@master").exists());
}

/// 重名（目录已存在）⇒ `409 team_exists`，不得覆盖既有能力文件。
#[tokio::test]
async fn duplicate_team_is_409_and_keeps_existing_files() {
    let env = new_env(&["research"]).await;
    let master = env.home.join("teams/research/agents/@master/agent.md");
    std::fs::create_dir_all(master.parent().unwrap()).unwrap();
    std::fs::write(&master, "# hand written\n").unwrap();

    let reply = env
        .post(
            "/api/teams",
            json!({"id": "research", "from_template": "team_default"}),
        )
        .await;

    assert_eq!(reply.status, 409, "body={}", reply.body);
    assert_eq!(reply.code(), "team_exists");
    assert_eq!(
        std::fs::read_to_string(&master).unwrap(),
        "# hand written\n",
        "已存在的团队目录不得被模板覆盖"
    );
}

/// 非法 id 与不支持的模板都走 `400 invalid_argument`。
#[tokio::test]
async fn invalid_team_id_and_unknown_template_are_400() {
    let env = new_env(&["research"]).await;

    for bad in ["Writing", "a", "@master", "with space", "research2!"] {
        let reply = env.post("/api/teams", json!({"id": bad})).await;
        assert_eq!(reply.status, 400, "id={bad} body={}", reply.body);
        assert_eq!(reply.code(), "invalid_argument");
        assert!(!env.home.join("teams").join(bad).exists());
    }

    let empty = env.post("/api/teams", json!({"id": ""})).await;
    assert_eq!(empty.status, 400, "空 id 也是非法: {}", empty.body);
    assert_eq!(empty.code(), "invalid_argument");

    let unsupported = env
        .post(
            "/api/teams",
            json!({"id": "peco2", "from_template": "peco"}),
        )
        .await;
    assert_eq!(unsupported.status, 400, "body={}", unsupported.body);
    assert_eq!(unsupported.code(), "invalid_argument");
    assert!(
        !env.home.join("teams/peco2/agents").exists(),
        "不支持的模板不得留下半装目录"
    );
}

/// 已装好的空间升过模板：`agent.md` 是 13，`team.json` 与索引行都还是 12 ——
/// 清单必须报磁盘上的版本，否则 UI 永远看不到模板已经升级。
///
/// 变异：`meta_from_dir` 只读 `team.json`，或清单只信索引行的版本 → 下面的
/// 断言拿到 12 变红。
#[tokio::test]
async fn list_reports_the_version_installed_on_disk() {
    let env = new_env(&["research"]).await;
    write_master_agent(&env, "research", 13);
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

    let reply = env.get("/api/teams").await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body[0]["team_id"], json!("research"));
    assert_eq!(
        reply.body[0]["template_version"],
        json!(13),
        "清单必须报磁盘上的模板版本"
    );
    assert_eq!(
        reply.body[0]["display_name"],
        json!("研究组"),
        "展示名仍信索引行"
    );
}

/// 团队卡片要的四个计数：来源就是详情页那几份清单的长度。
///
/// 夹具刻意让「装了但缺 `SKILL.md` 的目录」与「损坏的 mcpconfig」各出现一次：
/// 计数必须与详情接口同口径，否则卡片说 2 个 skill 而详情页只列 1 个。
#[tokio::test]
async fn list_reports_agent_skill_mcp_and_space_counts() {
    let env = new_env(&["research", "writing"]).await;
    let root = env.home.join("teams/research");
    for agent in ["@master", "@coder"] {
        let dir = root.join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.md"),
            format!("---\nagent:\n  name: \"{agent}\"\n---\n"),
        )
        .unwrap();
    }
    let skill = root.join("skills/review");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "# review\n").unwrap();
    // 缺 SKILL.md 的目录不是可加载的 skill，不进计数。
    std::fs::create_dir_all(root.join("skills/half-baked")).unwrap();
    std::fs::write(
        root.join("mcpconfig.json"),
        json!({"mcpServers": {
            "fs": {"transport": "stdio", "command": "npx"},
            "web": {"transport": "stdio", "command": "npx"},
        }})
        .to_string(),
    )
    .unwrap();

    let research_space = env.open_space("alpha", "research").await;
    env.open_space("beta", "research").await;
    env.open_space("gamma", "writing").await;

    let reply = env.get("/api/teams").await;
    assert_eq!(reply.status, 200, "body={}", reply.body);

    let research = reply
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["team_id"] == json!("research"))
        .expect("research 应在清单里");
    assert_eq!(research["agent_count"], json!(2), "两个 agent.md");
    assert_eq!(
        research["skill_count"],
        json!(1),
        "只有带 SKILL.md 的算一个"
    );
    assert_eq!(
        research["mcp_count"],
        json!(2),
        "mcpconfig.json 里两个 server"
    );
    assert_eq!(research["space_count"], json!(2), "research 绑了两个空间");
    assert_eq!(
        research["agent_count"],
        json!(research["agents"].as_array().unwrap().len()),
        "计数与明细必须同源"
    );
    assert_eq!(
        research["space_count"],
        json!(research["bound_spaces"].as_array().unwrap().len()),
        "空间计数就是 bound_spaces 的长度"
    );
    assert!(
        research["bound_spaces"]
            .as_array()
            .unwrap()
            .contains(&json!(research_space)),
        "绑定的空间 id 仍要出现在 bound_spaces 里"
    );

    let writing = reply
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["team_id"] == json!("writing"))
        .expect("writing 应在清单里");
    assert_eq!(writing["agent_count"], json!(0), "空团队没有 agent");
    assert_eq!(writing["skill_count"], json!(0));
    assert_eq!(
        writing["mcp_count"],
        json!(0),
        "没有 mcpconfig.json 是 0，不是错误"
    );
    assert_eq!(writing["space_count"], json!(1));
}

/// Peco 是保留团队：清单里过滤掉它，详情接口仍要能读到。
///
/// 变异：去掉 `list` 里的 `PECO_TEAM_ID` 过滤 → 第一条断言拿到 ["peco","research"] 变红。
#[tokio::test]
async fn peco_is_absent_from_the_list_but_readable_in_detail() {
    let env = new_env(&[PECO_TEAM_ID, "research"]).await;

    let listed = env.get("/api/teams").await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert_eq!(
        team_ids(&listed),
        vec!["research"],
        "Peco 不得出现在团队清单"
    );

    let detail = env.get(&format!("/api/teams/{PECO_TEAM_ID}")).await;
    assert_eq!(detail.status, 200, "body={}", detail.body);
    assert_eq!(detail.body["id"], json!(PECO_TEAM_ID));
}

/// 保留团队 id 不能通过接口创建：这是 `400 invalid_argument`，不是 `409 team_exists`
/// （目录已存在只是夹具的既成事实，语义上这条 id 永远不许建）。
#[tokio::test]
async fn creating_the_reserved_peco_team_is_400() {
    let env = new_env(&[PECO_TEAM_ID]).await;

    let reply = env.post("/api/teams", json!({"id": PECO_TEAM_ID})).await;

    assert_eq!(reply.status, 400, "body={}", reply.body);
    assert_eq!(reply.code(), "invalid_argument");
    assert!(
        env.home.join("teams").join(PECO_TEAM_ID).is_dir(),
        "被拒的创建不得动既有目录"
    );
}
