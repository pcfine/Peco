//! 同一 team 的多个空间：能力文件同一份、运行实例互相独立、KB 命名空间同前缀。

mod support;

use std::sync::Arc;

use peco_core::tools::KnowledgeAccess;
use support::new_env;

async fn open_two_spaces_of_same_team(
    env: &support::TestEnv,
) -> (
    Arc<peco_core::workspace::WorkSpace>,
    Arc<peco_core::workspace::WorkSpace>,
) {
    let first_dir = env.work_dir("space-a");
    let second_dir = env.work_dir("space-b");

    let first_id = env.resolve(&first_dir, "research").await.unwrap();
    let second_id = env.resolve(&second_dir, "research").await.unwrap();
    assert_ne!(first_id, second_id, "不同 work_dir 必须是不同空间");

    let first = env.open(&first_id).await.unwrap().workspace();
    let second = env.open(&second_id).await.unwrap().workspace();
    (first, second)
}

/// P1-10：同 team 两空间的 `agents/@master/agent.md` 指向同一个绝对路径，
/// 但 `AgentManager` 是两个不同实例（各一份缓存与 Hub）。
#[tokio::test]
async fn two_spaces_share_agent_files_but_not_managers() {
    let env = new_env(&["research"]).await;
    let master_dir = env.home.join("teams/research/agents/@master");
    std::fs::create_dir_all(&master_dir).unwrap();
    std::fs::write(master_dir.join("agent.md"), "# placeholder\n").unwrap();

    let (first, second) = open_two_spaces_of_same_team(&env).await;

    let first_path = first.agent_manager().md_path("@master");
    let second_path = second.agent_manager().md_path("@master");
    assert_eq!(
        first_path, second_path,
        "同 team 的 agent 定义必须是同一份文件"
    );
    assert_eq!(first_path, master_dir.join("agent.md"));
    assert!(first_path.is_absolute(), "路径必须是绝对的");
    assert!(first_path.exists());

    assert!(
        !Arc::ptr_eq(first.agent_manager(), second.agent_manager()),
        "AgentManager 必须是两个实例"
    );
    assert!(!Arc::ptr_eq(&first, &second), "WorkSpace 必须是两个实例");
}

/// P1-11：同 team 两空间的 KB 命名空间前缀都是 `{team_id}::`。
#[tokio::test]
async fn kb_namespace_prefix_is_team_id_for_every_space() {
    let env = new_env(&["research"]).await;
    let (first, second) = open_two_spaces_of_same_team(&env).await;

    // `WorkSpace::open` 的第 2 参就是 KB scope；写成 "local" 会让不同 team 的同名 KB 撞在一起。
    assert_eq!(first.user_id(), "research");
    assert_eq!(second.user_id(), "research");

    let first_scope = KnowledgeAccess::user_id(first.agent_manager().as_ref());
    let second_scope = KnowledgeAccess::user_id(second.agent_manager().as_ref());
    assert_eq!(first_scope, "research");
    assert_eq!(second_scope, "research");

    let first_ns = knowledge_base::namespace_for(first_scope, "@private_memory").unwrap();
    let second_ns = knowledge_base::namespace_for(second_scope, "@private_memory").unwrap();
    assert_eq!(first_ns, "research::@private_memory");
    assert_eq!(first_ns, second_ns, "同 team 两空间必须共用命名空间");
    assert!(
        first_ns.starts_with("research::"),
        "前缀必须是 team_id，实际 {first_ns}"
    );
    assert!(
        !first_ns.starts_with("local::"),
        "把 local 传给 open 会让不同 team 的同名 KB 撞车"
    );
}
