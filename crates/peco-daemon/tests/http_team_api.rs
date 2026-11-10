//! `/api/teams` 的 HTTP 语义：清单来源（目录 ∪ 行）、模板安装、重名与非法 id。

mod support;

use peco_daemon::db::teams::{self, NewTeam};
use peco_daemon::space::peco::PECO_TEAM_ID;
use serde_json::json;
use support::new_env;

/// 写入带模板版本的 `@master/agent.md`。
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

/// 写入 `team.json`。
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

/// 目录-only 团队也出现在清单。
#[tokio::test]
async fn list_reports_directory_only_teams() {
    let env = new_env(&["research", "writing"]).await;

    let reply = env.get("/api/teams").await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(team_ids(&reply), vec!["research", "writing"]);
    assert_eq!(reply.body[0]["display_name"], json!("research"));
    assert_eq!(reply.body[0]["agents"], json!([]), "空目录没有 agent");
}

/// 模板安装写入能力文件与索引行。
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

/// 空团队只建目录。
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

/// 重名返回 409。
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

/// 非法 id 与未知模板返回 400。
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

/// 清单版本以磁盘为准。
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

/// 清单计数与详情同口径。
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

/// Peco 不进清单，详情可读。
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

/// `?include=peco` 才把保留团队并入清单，`reserved` 标出谁是保留团队。
#[tokio::test]
async fn include_peco_adds_the_reserved_team() {
    let env = new_env(&[PECO_TEAM_ID, "research"]).await;

    // 不带参数：与现状一致（Peco 被挡在清单外）。
    let listed = env.get("/api/teams").await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert_eq!(team_ids(&listed), vec!["research"]);
    assert_eq!(
        listed.body[0]["reserved"],
        json!(false),
        "普通团队不是保留团队"
    );

    let with_peco = env.get("/api/teams?include=peco").await;
    assert_eq!(with_peco.status, 200, "body={}", with_peco.body);
    assert_eq!(
        team_ids(&with_peco),
        vec![PECO_TEAM_ID.to_string(), "research".to_string()]
    );
    let peco = with_peco
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["team_id"] == json!(PECO_TEAM_ID))
        .expect("Peco 应在清单里");
    assert_eq!(peco["reserved"], json!(true));
    let research = with_peco
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["team_id"] == json!("research"))
        .expect("research 应在清单里");
    assert_eq!(research["reserved"], json!(false));

    // 不认识的取值忽略：参数拼错不该把保留团队放进来。
    let unknown = env.get("/api/teams?include=ghost").await;
    assert_eq!(team_ids(&unknown), vec!["research"]);
}

/// 保留 id 不能创建。
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

/// PATCH 只改图标。
#[tokio::test]
async fn patch_icon_persists_and_preserves_metadata() {
    let env = new_env(&["research"]).await;
    teams::upsert(
        &env.pool,
        &NewTeam {
            id: "research",
            display_name: "研究组",
            template_version: 3,
        },
    )
    .await
    .unwrap();

    let reply = env
        .patch(
            "/api/teams/research",
            json!({"icon": "/uploads/teams/a.png"}),
        )
        .await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["icon"], json!("/uploads/teams/a.png"));

    let row: (String, i64, String) = sqlx::query_as(
        "SELECT display_name, template_version, icon FROM teams WHERE id = 'research'",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        ("研究组".to_string(), 3, "/uploads/teams/a.png".to_string())
    );
}

/// 目录-only 团队可改图标。
#[tokio::test]
async fn patch_icon_creates_row_for_directory_only_team() {
    let env = new_env(&["research"]).await;

    let reply = env
        .patch(
            "/api/teams/research",
            json!({"icon": "/uploads/teams/a.png"}),
        )
        .await;

    assert_eq!(reply.status, 200, "body={}", reply.body);

    let row: (String, i64, String) = sqlx::query_as(
        "SELECT display_name, template_version, icon FROM teams WHERE id = 'research'",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "research".to_string(),
            0,
            "/uploads/teams/a.png".to_string()
        )
    );
}

/// 图标 URL 白名单。
#[tokio::test]
async fn patch_rejects_non_upload_icon_urls() {
    let env = new_env(&["research"]).await;

    for bad in [
        "javascript:alert(1)",
        "https://x/a.png",
        "/uploads/../etc/passwd",
        "pixel:abc",
    ] {
        let reply = env.patch("/api/teams/research", json!({"icon": bad})).await;
        assert_eq!(reply.status, 400, "icon={bad} body={}", reply.body);
        assert_eq!(reply.code(), "invalid_argument");
    }

    let cleared = env.patch("/api/teams/research", json!({"icon": ""})).await;
    assert_eq!(cleared.status, 200, "空串表示清空: {}", cleared.body);
}

/// 未知团队返回 404。
#[tokio::test]
async fn patch_unknown_team_is_404() {
    let env = new_env(&["research"]).await;

    let reply = env
        .patch("/api/teams/ghost", json!({"icon": "/uploads/teams/a.png"}))
        .await;

    assert_eq!(reply.status, 404, "body={}", reply.body);
    assert_eq!(reply.code(), "team_not_found");
}

/// 从已有团队复制。
#[tokio::test]
async fn create_team_by_copying_an_existing_team() {
    let env = new_env(&["research"]).await;
    let source = env.home.join("teams/research");
    let agent_dir = source.join("agents/@master");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("agent.md"),
        "---\nagent:\n  name: \"@master\"\n  description: \"总管\"\n\
         template_version: 3\n---\n\n# prompt\n",
    )
    .unwrap();
    let skill_dir = source.join("skills/review");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "# review\n").unwrap();
    std::fs::write(
        source.join("mcpconfig.json"),
        json!({"mcpServers": {"fs": {"transport": "stdio", "command": "npx"}}}).to_string(),
    )
    .unwrap();
    write_team_json(&env, "research", "研究组", 3);
    let source_team_json = std::fs::read_to_string(source.join("team.json")).unwrap();

    let reply = env
        .post(
            "/api/teams",
            json!({"id": "clone", "display_name": "克隆团队", "from_team": "research"}),
        )
        .await;

    assert_eq!(reply.status, 201, "body={}", reply.body);
    assert_eq!(reply.body["team_id"], json!("clone"));
    assert_eq!(reply.body["display_name"], json!("克隆团队"));
    assert_eq!(reply.body["template_version"], json!(3));
    assert_eq!(reply.body["agent_count"], json!(1));
    assert_eq!(reply.body["skill_count"], json!(1));
    assert_eq!(reply.body["mcp_count"], json!(1));

    let clone = env.home.join("teams/clone");
    assert!(clone.join("agents/@master/agent.md").is_file());
    assert!(clone.join("skills/review/SKILL.md").is_file());
    assert!(clone.join("mcpconfig.json").is_file());

    let cloned_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(clone.join("team.json")).unwrap()).unwrap();
    assert_eq!(cloned_json["team_id"], json!("clone"));
    assert_eq!(cloned_json["display_name"], json!("克隆团队"));
    assert_eq!(cloned_json["template_version"], json!(3));

    assert_eq!(
        std::fs::read_to_string(source.join("team.json")).unwrap(),
        source_team_json,
        "复制不得改写源团队"
    );
    assert!(
        teams::get(&env.pool, "clone").await.unwrap().is_some(),
        "复制出来的团队要有索引行"
    );
}

/// 复制继承源团队图标。
#[tokio::test]
async fn copy_team_inherits_source_icon() {
    let env = new_env(&["research"]).await;
    let patched = env
        .patch(
            "/api/teams/research",
            json!({"icon": "/uploads/teams/research.png"}),
        )
        .await;
    assert_eq!(patched.status, 200, "body={}", patched.body);

    let reply = env
        .post(
            "/api/teams",
            json!({"id": "clone", "from_team": "research"}),
        )
        .await;

    assert_eq!(reply.status, 201, "body={}", reply.body);
    assert_eq!(reply.body["icon"], json!("/uploads/teams/research.png"));
}

/// 非法或保留来源被拒绝。
#[tokio::test]
async fn copy_team_rejects_bad_sources() {
    let env = new_env(&[PECO_TEAM_ID, "research"]).await;

    let missing = env
        .post("/api/teams", json!({"id": "clone", "from_team": "ghost"}))
        .await;
    assert_eq!(missing.status, 404, "body={}", missing.body);
    assert_eq!(missing.code(), "team_not_found");
    assert!(!env.home.join("teams/clone").exists());

    for bad in [PECO_TEAM_ID, "..", "Writing"] {
        let reply = env
            .post("/api/teams", json!({"id": "clone", "from_team": bad}))
            .await;
        assert_eq!(reply.status, 400, "source={bad} body={}", reply.body);
        assert_eq!(reply.code(), "invalid_argument");
        assert!(
            !env.home.join("teams/clone").exists(),
            "被拒的复制不得留下目标目录"
        );
    }

    let both = env
        .post(
            "/api/teams",
            json!({"id": "clone", "from_team": "research", "from_template": "team_default"}),
        )
        .await;
    assert_eq!(both.status, 400, "body={}", both.body);
    assert_eq!(both.code(), "invalid_argument");
    assert!(!env.home.join("teams/clone").exists());
}

/// Peco 图标只写 DB。
#[tokio::test]
async fn patch_peco_icon_leaves_team_json_untouched() {
    let env = new_env(&[PECO_TEAM_ID]).await;
    let path = env.home.join("teams").join(PECO_TEAM_ID).join("team.json");
    std::fs::write(&path, "{\"team_id\":\"peco\"}").unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    let reply = env
        .patch(
            &format!("/api/teams/{PECO_TEAM_ID}"),
            json!({"icon": "/uploads/teams/a.png"}),
        )
        .await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}
