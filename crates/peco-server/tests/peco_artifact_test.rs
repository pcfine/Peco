// ============================================================================
// peco 主对话产物落库集成测试 — 私有会话懒建、先落库后发事件、不可删、迁移收敛
// ============================================================================
//
// 每条用例都直调 `looper_event_to_sse`（桥循环里的唯一落库入口），不需要真实
// LLM。T9 例外：它要的不是全新库，故自建 pool。

mod common;

use common::TestApp;
use peco_core::agent::LooperEvent;
use peco_core::artifacts::ArtifactDecl;
use peco_core::workdir::WorkDirs;
use peco_server::artifact::ArtifactItem;
use peco_server::chat::ChatSseEvent;
use peco_server::peco::session::{SESSION_AGENT_NAME, SESSION_TITLE, private_session_id};

// ── helpers ─────────────────────────────────────────────────────────────────

fn work_dirs(app: &TestApp) -> WorkDirs {
    WorkDirs::from_workspace_root(&app.state.workspace_manager.workspace_dir(&app.user_id))
        .expect("test workspace must follow <data_dir>/workspaces/<uid> layout")
}

/// 在 work 目录下写文件，返回绝对路径。
fn write_work_file(app: &TestApp, rel: &str, bytes: &[u8]) -> std::path::PathBuf {
    // output/ 必须存在：路径校验对 output 做 canonicalize（前缀比较的前提）
    let dirs = work_dirs(app);
    std::fs::create_dir_all(dirs.output()).unwrap();
    let path = dirs.root().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    path
}

fn decl_of(abs: &std::path::Path, title: Option<&str>) -> ArtifactDecl {
    ArtifactDecl {
        path: abs.to_string_lossy().into_owned(),
        title: title.map(str::to_string),
        mime: None,
    }
}

/// 走 peco 唯一的落库入口，把一条 ToolResult 映射为 SSE 事件。
async fn tool_result_event(
    app: &TestApp,
    id: &str,
    result: &str,
    decls: Vec<ArtifactDecl>,
) -> ChatSseEvent {
    peco_server::peco::handler::looper_event_to_sse(
        &app.state.db,
        &app.state.workspace_manager,
        &app.user_id,
        LooperEvent::ToolResult {
            id: id.to_string(),
            name: "shell".to_string(),
            result: result.to_string(),
            images: Vec::new(),
            artifacts: decls,
        },
    )
    .await
    .expect("ToolResult 必须映射为 SSE 事件")
}

fn artifacts_of(ev: ChatSseEvent) -> Vec<ArtifactItem> {
    let ChatSseEvent::ToolResult { artifacts, .. } = ev else {
        panic!("期望 ToolResult 事件，实际 {ev:?}");
    };
    artifacts
}

async fn artifact_count(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM artifacts")
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

async fn conversation_count(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM conversations")
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

// ── T1 先落库后发事件 ───────────────────────────────────────────────────────

/// 变异点：把 `looper_event_to_sse` 改成先返回事件、落库放 spawn / 放到后面，
/// 或让事件字段取自入参而非库行 → `find_by_id_and_user` 命中失败或值不等。
#[tokio::test]
async fn persists_before_emitting() {
    let app = TestApp::new().await;
    let abs = write_work_file(
        &app,
        "output/media-gen/2026-10-20/persist.jpeg",
        b"JPEGDATA",
    );
    let decls = vec![decl_of(&abs, Some("红狐狸"))];

    let items = artifacts_of(tool_result_event(&app, "call_1", "生成完成", decls).await);
    assert_eq!(items.len(), 1, "一条合法声明必须产出一项");

    // 返回后立刻查库，不给任何补写窗口
    let stored =
        peco_server::db::artifacts::find_by_id_and_user(&app.state.db, &items[0].id, &app.user_id)
            .await
            .unwrap()
            .expect("事件里的 id 必须已经在库里 —— 它是前端取流的唯一凭据");

    assert_eq!(items[0].title, "红狐狸");
    // `created_at` 是秒粒度，同秒内自算也能相等，只作弱信号；
    // 「预生成 id 再落库」由 T4 / T5 打红（那两条不携带 id）。
    assert_eq!(items[0].created_at, stored.created_at);
    assert_eq!(items[0].size_bytes, stored.size_bytes);
}

// ── T2 私有会话只建一条且产物归属它 ────────────────────────────────────────

/// 变异点：把 `ON CONFLICT DO NOTHING` 换成裸 INSERT（第二次调用报错）或
/// check-then-insert；或让产物 `conversation_id` 取自别的值 → 行数 / 归属断言红。
#[tokio::test]
async fn creates_private_session_conversation_once() {
    let app = TestApp::new().await;
    let private_id = private_session_id(&app.user_id);

    let a = write_work_file(&app, "output/a.png", b"PNG-A");
    let b = write_work_file(&app, "output/b.png", b"PNG-B");
    artifacts_of(tool_result_event(&app, "call_a", "ok", vec![decl_of(&a, None)]).await);
    artifacts_of(tool_result_event(&app, "call_b", "ok", vec![decl_of(&b, None)]).await);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM conversations WHERE id = ?")
        .bind(&private_id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(count, 1, "两次懒建必须收敛到同一行");

    let (scope, agent_name, user_id, title): (String, String, String, String) =
        sqlx::query_as("SELECT scope, agent_name, user_id, title FROM conversations WHERE id = ?")
            .bind(&private_id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(scope, "peco");
    assert_eq!(agent_name, SESSION_AGENT_NAME);
    assert_eq!(user_id, app.user_id);
    assert_eq!(title, SESSION_TITLE);

    // 归属断言用独立查询：拿待验证的值做筛选条件是循环论证
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT conversation_id FROM artifacts WHERE user_id = ?")
            .bind(&app.user_id)
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    // 没有这一行时，下面的循环遍历空结果集一次都不执行，「一条都没落」也能全绿
    assert_eq!(rows.len(), 2, "两条声明必须落成两行产物");
    for conversation_id in &rows {
        assert_eq!(conversation_id, &private_id);
    }
}

// ── T3 事件形状与 /api/chat 路径一致 ───────────────────────────────────────

/// 变异点：peco 侧手搓 `ChatSseEvent::ToolResult`（少传 result / images，或
/// title 派生规则不一致）→ 与 `/api/chat` 同一构造函数的输出逐字段不等。
#[tokio::test]
async fn event_shape_matches_chat_path() {
    let app = TestApp::new().await;
    // 两侧复用同一文件：唯一索引是 (conversation_id, rel_path)，会话 id 不同不冲突。
    // 换路径会让 title 从文件名派生出不同值，使下面的 title 断言假红。
    let abs = write_work_file(&app, "output/media-gen/2026-10-20/shared.jpeg", b"JPEGDATA");
    let size_bytes = std::fs::metadata(&abs).unwrap().len() as i64;
    let decls = vec![decl_of(&abs, Some("红狐狸"))];

    let peco_ev = tool_result_event(&app, "call_t3", "生成完成", decls.clone()).await;

    // 对照侧需要一个真实会话行：artifacts 外键要求它先存在
    let agent_conv = "conv-t3-agent".to_string();
    peco_server::db::conversations::insert(
        &app.state.db,
        &peco_server::db::conversations::CreateConversationParams {
            id: agent_conv.clone(),
            user_id: app.user_id.clone(),
            agent_id: None,
            agent_name: "writer".to_string(),
            title: "对照会话".to_string(),
        },
    )
    .await
    .unwrap();
    let chat_ev = peco_server::artifact::build_tool_result_event(
        &app.state.db,
        Some(&work_dirs(&app)),
        &agent_conv,
        &app.user_id,
        "call_t3".to_string(),
        "shell".to_string(),
        "生成完成".to_string(),
        Vec::new(),
        &decls,
    )
    .await;

    let (
        ChatSseEvent::ToolResult {
            id: peco_id,
            name: peco_name,
            result: peco_result,
            images: peco_images,
            artifacts: peco_artifacts,
            conversation_id: peco_conv,
        },
        ChatSseEvent::ToolResult {
            id: chat_id,
            name: chat_name,
            result: chat_result,
            images: chat_images,
            artifacts: chat_artifacts,
            conversation_id: chat_conv,
        },
    ) = (peco_ev, chat_ev)
    else {
        panic!("两侧都必须产出 ToolResult 事件");
    };

    assert_eq!(peco_id, chat_id);
    assert_eq!(peco_name, chat_name);
    assert_eq!(peco_result, chat_result);
    assert_eq!(peco_images, chat_images);
    assert_eq!(peco_conv, private_session_id(&app.user_id));
    assert_eq!(chat_conv, agent_conv);
    assert_eq!(peco_artifacts.len(), 1);
    assert_eq!(chat_artifacts.len(), 1);

    let (peco_item, chat_item) = (&peco_artifacts[0], &chat_artifacts[0]);
    assert_eq!(peco_item.title, chat_item.title);
    assert_eq!(peco_item.kind, chat_item.kind);
    assert_eq!(peco_item.mime, chat_item.mime);

    // 形状校验（离线也能手搓，只验形状，不承担「值只能来自库」的证明）
    assert!(!peco_item.created_at.is_empty());
    assert_eq!(peco_item.size_bytes, size_bytes);
    assert_eq!(
        peco_item.url,
        format!("/api/artifacts/{}/content", peco_item.id)
    );
}

// ── T4 重复声明不重复落库 ──────────────────────────────────────────────────

/// 变异点：把去重交给 check-then-insert 或直接裸 INSERT → 第二次要么报错、
/// 要么带出第二条产物行 / 非空 artifacts。
#[tokio::test]
async fn duplicate_declaration_yields_empty_artifacts() {
    let app = TestApp::new().await;
    let abs = write_work_file(&app, "output/dup.png", b"PNG");
    let decls = vec![decl_of(&abs, None)];

    let first = artifacts_of(tool_result_event(&app, "call_1", "done", decls.clone()).await);
    assert_eq!(first.len(), 1);

    // 模拟第二个仍在订阅的桥处理同一条事件
    let ev = tool_result_event(&app, "call_1", "done", decls).await;
    let ChatSseEvent::ToolResult {
        result,
        images,
        artifacts,
        ..
    } = ev
    else {
        panic!("期望 ToolResult 事件");
    };
    assert!(artifacts.is_empty(), "重复声明不得携带 id");
    assert_eq!(result, "done");
    assert!(images.is_empty());

    assert_eq!(artifact_count(&app).await, 1, "去重由唯一索引承担");
}

// ── T5 被拒声明只降级 ──────────────────────────────────────────────────────

/// 变异点：路径校验改成「文件存在即接受」（`..` 逃逸不再被拦）或把拒绝
/// 升级成 Err / panic → 事件缺失或产物行增量非 0。
#[tokio::test]
async fn rejected_declaration_degrades() {
    let app = TestApp::new().await;
    let before = artifact_count(&app).await;

    // 文件真实存在：canonicalize 消解 `..` 后落在 output 之外 → 证明是越界而非缺文件
    write_work_file(&app, "tmp/x.txt", b"secret");
    let escaping = work_dirs(&app).output().join("../tmp/x.txt");

    let ev = tool_result_event(&app, "call_1", "done", vec![decl_of(&escaping, None)]).await;
    let ChatSseEvent::ToolResult {
        result, artifacts, ..
    } = ev
    else {
        panic!("期望 ToolResult 事件");
    };
    assert!(artifacts.is_empty(), "越界声明不得登记");
    assert_eq!(result, "done", "产物登记失败不得否决工具结果");
    assert_eq!(artifact_count(&app).await, before, "被拒声明不得写入任何行");
}

// ── T6 产物只对属主可取 ────────────────────────────────────────────────────

/// 变异点：桥把 `user_id` 传成别人 / 事件 id 指向他人行 → 本人取流 404 或
/// 他人取流 200。
#[tokio::test]
async fn artifact_is_servable_only_to_owner() {
    let app = TestApp::new().await;
    let abs = write_work_file(&app, "output/owned.png", b"PNG-BYTES");
    let items =
        artifacts_of(tool_result_event(&app, "call_1", "done", vec![decl_of(&abs, None)]).await);
    assert_eq!(items.len(), 1);

    let resp = app
        .get(&format!("/api/artifacts/{}/content", items[0].id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), b"PNG-BYTES");

    let (_, token2) = app.register_user2().await;
    let resp = app
        .get_as(&format!("/api/artifacts/{}/content", items[0].id), &token2)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "他人产物一律 404，不泄露存在性");
}

// ── T7 无产物的工具结果零副作用 ────────────────────────────────────────────

/// 变异点：把懒建提到 `match` 之外（每条工具结果都建行）→ conversations 计数
/// 增量非 0；或让空 artifacts 分支也走落库 → 事件与本地构造的期望值不等。
#[tokio::test]
async fn plain_tool_result_has_no_side_effect() {
    let app = TestApp::new().await;
    let before = conversation_count(&app).await;

    let result = "done".to_string();
    let ev = tool_result_event(&app, "call_plain", &result, Vec::new()).await;

    // 对照侧只能本地构造：`map_looper_event` 在 tests/ 不可见（chat::sse 是
    // pub(crate)），且不为测试改可见性 / 给生产 DTO 加 PartialEq。
    let expected = ChatSseEvent::ToolResult {
        id: "call_plain".to_string(),
        name: "shell".to_string(),
        result,
        images: vec![],
        artifacts: vec![],
        conversation_id: private_session_id(&app.user_id),
    };
    assert_eq!(
        serde_json::to_value(&ev).unwrap(),
        serde_json::to_value(&expected).unwrap(),
        "无产物分支必须与纯映射逐字段一致"
    );
    assert_eq!(conversation_count(&app).await, before, "不得触发懒建");
}

// ── T8 私有会话不出现在 agent 视图 ─────────────────────────────────────────

/// 变异点：6 条 SQL 任一条漏加 `AND scope = 'agent'` → 私有会话以
/// `SESSION_AGENT_NAME` 命中该条查询（它的 agent_name 恰好等于入参）。
#[tokio::test]
async fn private_session_hidden_from_agent_views() {
    let app = TestApp::new().await;
    let abs = write_work_file(&app, "output/hidden.png", b"PNG");
    artifacts_of(tool_result_event(&app, "call_1", "done", vec![decl_of(&abs, None)]).await);

    let db = &app.state.db;
    let uid = app.user_id.as_str();
    let private_id = private_session_id(uid);
    let agent = SESSION_AGENT_NAME;

    let all = peco_server::db::conversations::list_by_user_and_agent(db, uid, agent, true)
        .await
        .unwrap();
    assert!(
        all.iter().all(|c| c.id != private_id),
        "含归档分支不得列出私有会话"
    );

    // WebUI 默认 status=active 走的分支
    let active = peco_server::db::conversations::list_by_user_and_agent(db, uid, agent, false)
        .await
        .unwrap();
    assert!(active.iter().all(|c| c.id != private_id));

    let filtered = peco_server::db::conversations::list_by_user(db, uid, Some(agent))
        .await
        .unwrap();
    assert!(filtered.iter().all(|c| c.id != private_id));

    // deprecated GET /api/conversations 路径
    let unfiltered = peco_server::db::conversations::list_by_user(db, uid, None)
        .await
        .unwrap();
    assert!(unfiltered.iter().all(|c| c.id != private_id));

    let active_count = peco_server::db::conversations::count_active(db, uid, agent)
        .await
        .unwrap();
    assert_eq!(active_count, 0, "私有会话不得计入活跃上限");

    let archived = peco_server::db::conversations::archive_oldest(db, uid, agent, 10)
        .await
        .unwrap();
    assert_eq!(archived, 0, "私有会话不得被自动归档");
    let archived_at: Option<String> =
        sqlx::query_scalar("SELECT archived_at FROM conversations WHERE id = ?")
            .bind(&private_id)
            .fetch_one(db)
            .await
            .unwrap();
    assert!(archived_at.is_none(), "该行 archived_at 必须仍为 NULL");
}

// ── T9 迁移 016 三种起点收敛 ───────────────────────────────────────────────

/// 仅用于本用例：模拟存量库的 conversations（无 scope 列）。
const LEGACY_CONVERSATIONS_DDL: &str = r#"
CREATE TABLE conversations (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    agent_id TEXT,
    agent_name TEXT NOT NULL DEFAULT 'unknown',
    title TEXT NOT NULL DEFAULT '新对话',
    archived_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
)"#;

async fn scope_column_shape(pool: &sqlx::SqlitePool) -> (String, i64, Option<String>) {
    sqlx::query_as(
        r#"SELECT type, "notnull", dflt_value FROM pragma_table_info('conversations') WHERE name = 'scope'"#,
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// 变异点：016 的 `DEFAULT` 抄错（存量行拿到别的值 → 6 条 scope 谓词把它们
/// 全部过滤 → 侧栏全空）或守卫改判表（ALTER 重跑报 duplicate column name）。
#[tokio::test]
async fn migration_016_converges_from_old_db() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}/legacy.db?mode=rwc", dir.path().display());
    let pool = peco_server::db::connect(&url).await.unwrap();

    // ① 存量库起点：先迁移到终态，再把 conversations 换回不含 scope 的老形状
    peco_server::db::run_migrations(&pool).await.unwrap();
    sqlx::raw_sql("DROP TABLE conversations")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(LEGACY_CONVERSATIONS_DDL)
        .execute(&pool)
        .await
        .unwrap();
    // 存量数据：conversations.user_id 有外键 → 先有用户
    sqlx::query("INSERT INTO users (id, username, email, password_hash) VALUES (?, ?, ?, ?)")
        .bind("u-legacy")
        .bind("legacy")
        .bind("legacy@example.com")
        .bind("x")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO conversations (id, user_id, agent_name, title) VALUES (?, ?, ?, ?)")
        .bind("c-legacy")
        .bind("u-legacy")
        .bind("writer")
        .bind("存量对话")
        .execute(&pool)
        .await
        .unwrap();

    // 016 的 ALTER 分支在这里真跑一次
    peco_server::db::run_migrations(&pool)
        .await
        .expect("存量库升级不得失败");

    let scope_columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('conversations') WHERE name = 'scope'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(scope_columns, 1);
    let legacy_shape = scope_column_shape(&pool).await;
    assert_eq!(
        legacy_shape,
        ("TEXT".to_string(), 1, Some("'agent'".to_string())),
        "列默认值必须是 'agent'，否则存量会话行会被 scope 谓词过滤"
    );
    let legacy_scope: String = sqlx::query_scalar("SELECT scope FROM conversations WHERE id = ?")
        .bind("c-legacy")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(legacy_scope, "agent", "存量行必须落到默认值");

    // ② 同 pool 连跑第二次：列守卫生效，不重复 ALTER
    peco_server::db::run_migrations(&pool)
        .await
        .expect("重复执行迁移必须幂等");
    let scope_columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('conversations') WHERE name = 'scope'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(scope_columns, 1, "守卫必须避免重复 ALTER");

    // ③ 与全新库的 scope 列形状逐项相等（只比这一列：ALTER 只能追加到末位，
    // 整表行序比对会天然假红）
    let fresh_dir = tempfile::tempdir().unwrap();
    let fresh_url = format!("sqlite:{}/fresh.db?mode=rwc", fresh_dir.path().display());
    let fresh_pool = peco_server::db::connect(&fresh_url).await.unwrap();
    peco_server::db::run_migrations(&fresh_pool).await.unwrap();

    assert_eq!(
        scope_column_shape(&fresh_pool).await,
        legacy_shape,
        "全新库（schema.sql 建列）与存量库（016 补列）的 scope 列形状必须相同"
    );
}
