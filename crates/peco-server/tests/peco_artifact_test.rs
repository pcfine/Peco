// ============================================================================
// peco 主对话产物落库集成测试 — 私有会话懒建、先落库后发事件、不可删、迁移收敛
// ============================================================================
//
// 每条用例都经 `RunEmitter::emit`（runner 的唯一事件出口：带产物的工具结果先
// 落库、后广播），不需要真实 LLM。T9 例外：它要的不是全新库，故自建 pool。

mod common;

use std::collections::BTreeSet;

use common::TestApp;
use peco_core::agent::LooperEvent;
use peco_core::artifacts::ArtifactDecl;
use peco_core::workdir::WorkDirs;
use peco_server::artifact::ArtifactItem;
use peco_server::chat::ChatSseEvent;
use peco_server::peco::emitter::{RunEmitter, run_event_to_sse};
use peco_server::peco::handler::ArtifactPage;
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

/// 把一条 ToolResult 投进 runner 的事件出口（落库 → 广播），再从广播里取回事件
/// 映射为 SSE 事件 —— 与生产路径同一顺序，故「事件里的 id 已在库」可被直接断言。
async fn tool_result_event_as(
    app: &TestApp,
    user_id: &str,
    id: &str,
    result: &str,
    decls: Vec<ArtifactDecl>,
) -> ChatSseEvent {
    let (tx, mut rx) = tokio::sync::broadcast::channel(8);
    let emitter = RunEmitter::new(
        tx,
        app.state.db.clone(),
        std::sync::Arc::clone(&app.state.workspace_manager),
        user_id.to_string(),
    );
    emitter
        .emit(LooperEvent::ToolResult {
            id: id.to_string(),
            name: "shell".to_string(),
            result: result.to_string(),
            images: Vec::new(),
            artifacts: decls,
        })
        .await;
    let event = rx.recv().await.expect("runner 必须广播每一条事件");
    // 一条 ToolResult 只广播一次：先发 Looper 再发 Persisted 的双发会让前端
    // 按 id 去重掩盖症状（重复建行 / 重复渲染都被吃掉）。
    assert!(rx.try_recv().is_err(), "同一条 ToolResult 不得广播两次");
    run_event_to_sse(user_id, event).expect("ToolResult 必须映射为 SSE 事件")
}

/// 主用户视角的简写。
async fn tool_result_event(
    app: &TestApp,
    id: &str,
    result: &str,
    decls: Vec<ArtifactDecl>,
) -> ChatSseEvent {
    tool_result_event_as(app, &app.user_id, id, result, decls).await
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

async fn session_artifact_count(app: &TestApp, conversation_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM artifacts WHERE conversation_id = ?")
        .bind(conversation_id)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

// ── T1 先落库后发事件 ───────────────────────────────────────────────────────

/// 变异点：把 `RunEmitter::emit` 改成先发事件、落库放 spawn / 放到后面，或让事件
/// 字段取自入参而非库行 → `find_by_id_and_user` 命中失败或值不等。
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

// ── T1b 无订阅者也照常落库（跨连接存活） ───────────────────────────────────

/// 关页面 / 断网期间生成的产物必须入库：此时桥接任务已随连接断开退出，broadcast
/// 里一条 receiver 都没有，事件无人消费。
///
/// 变异点：把落库退回桥接循环（或任何依赖订阅者的路径）→ 本用例全程无
/// receiver，声明无人处理，产物行数为 0。
#[tokio::test]
async fn persists_without_any_subscriber() {
    let app = TestApp::new().await;
    let abs = write_work_file(&app, "output/offscreen/result.png", b"PNG");
    let (tx, rx) = tokio::sync::broadcast::channel(8);
    drop(rx);
    assert_eq!(tx.receiver_count(), 0, "前提：起手就没有订阅者");

    let emitter = RunEmitter::new(
        tx.clone(),
        app.state.db.clone(),
        std::sync::Arc::clone(&app.state.workspace_manager),
        app.user_id.clone(),
    );
    emitter
        .emit(LooperEvent::ToolResult {
            id: "call_offscreen".to_string(),
            name: "shell".to_string(),
            result: "生成完成".to_string(),
            images: Vec::new(),
            artifacts: vec![decl_of(&abs, Some("离线产物"))],
        })
        .await;

    assert_eq!(tx.receiver_count(), 0, "投递全程确实无人订阅");
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id, title FROM artifacts WHERE user_id = ?")
            .bind(&app.user_id)
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert_eq!(rows.len(), 1, "无订阅者时声明仍必须落库");
    let (artifact_id, title) = &rows[0];
    assert_eq!(title, "离线产物");

    let stored =
        peco_server::db::artifacts::find_by_id_and_user(&app.state.db, artifact_id, &app.user_id)
            .await
            .unwrap()
            .expect("落库行必须可按属主回查");
    assert_eq!(stored.conversation_id, private_session_id(&app.user_id));
    assert_eq!(
        conversation_count(&app).await,
        1,
        "私有会话行同样要懒建（产物外键依赖它）"
    );
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

    // 产物列表端点靠这条查询校验归属：误加 scope 谓词会让私有会话的产物列表变 404。
    assert!(
        peco_server::db::conversations::find_by_id_and_user(
            &app.state.db,
            &private_id,
            &app.user_id
        )
        .await
        .unwrap()
        .is_some(),
        "find_by_id_and_user 必须能命中私有会话行"
    );
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

    // 同一产物被再次声明（重连后重跑生成路径）
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

// ── T11 任何删除端点都删不掉私有会话 ───────────────────────────────────────

/// 变异点：两个删除入口任一仍用 `find_by_id_and_user`（不过滤 scope）→ 该入口
/// 返回 200，会话行与产物行经 ON DELETE CASCADE 一起消失。
#[tokio::test]
async fn private_session_row_not_deletable_via_any_endpoint() {
    let app = TestApp::new().await;
    let private_id = private_session_id(&app.user_id);
    let abs = write_work_file(&app, "output/keep.png", b"PNG");
    artifacts_of(tool_result_event(&app, "call_1", "done", vec![decl_of(&abs, None)]).await);

    let baseline_artifacts = session_artifact_count(&app, &private_id).await;
    assert!(baseline_artifacts >= 1);

    let resp = app
        .delete(&format!(
            "/api/chat/{SESSION_AGENT_NAME}/conversations/{private_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // deprecated 入口：`conversations::delete` 的第二个调用方
    let resp = app
        .delete(&format!("/api/conversations/{private_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM conversations WHERE id = ?")
        .bind(&private_id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(rows, 1, "私有会话行必须仍在");
    let scope: String = sqlx::query_scalar("SELECT scope FROM conversations WHERE id = ?")
        .bind(&private_id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(scope, "peco");
    assert_eq!(
        session_artifact_count(&app, &private_id).await,
        baseline_artifacts,
        "产物不得被 ON DELETE CASCADE 连带删除"
    );
}

// ============================================================================
// 阶段 2：会话产物列表端点（GET /api/peco/session/artifacts）
// ============================================================================
//
// 端点不接受外部 `conversation_id`：会话 id 由登录身份派生，客户端能控制的只有
// 分页参数。因此本节的核心断言都是「返回的 id 集合来自测试自己造的那批产物」，
// 而不是「回显了我传进去的参数」。

/// 在指定用户的 work 目录下写文件 —— 双用户隔离用例要替另一方造产物。
fn write_user_work_file(
    app: &TestApp,
    user_id: &str,
    rel: &str,
    bytes: &[u8],
) -> std::path::PathBuf {
    let dirs = WorkDirs::from_workspace_root(&app.state.workspace_manager.workspace_dir(user_id))
        .expect("test workspace must follow <data_dir>/workspaces/<uid> layout");
    std::fs::create_dir_all(dirs.output()).unwrap();
    let path = dirs.root().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    path
}

/// 替指定用户造 `n` 项产物（走 peco 唯一的落库入口），按插入顺序返回 id。
async fn seed_artifacts(app: &TestApp, user_id: &str, n: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for i in 0..n {
        let abs = write_user_work_file(
            app,
            user_id,
            &format!("output/seed-{user_id}/{i}.png"),
            b"PNG",
        );
        let ev = tool_result_event_as(
            app,
            user_id,
            &format!("call_{user_id}_{i}"),
            "done",
            vec![decl_of(&abs, Some(&format!("产物{i}")))],
        )
        .await;
        let items = artifacts_of(ev);
        assert_eq!(items.len(), 1, "第 {i} 条声明必须落库");
        ids.push(items[0].id.clone());
    }
    ids
}

/// 给测试主用户造一条 `scope='agent'` 的 chat 会话，并在其名下落 `n` 项产物。
///
/// `scope` 显式写死而非依赖列默认值：默认值一旦变动，这条用例就不再代表
/// 「同一用户的另一个会话」，隔离断言会悄悄失去意义。
async fn seed_chat_conversation_artifacts(app: &TestApp, n: usize) -> Vec<String> {
    let conversation_id = format!("conv-chat-{}", app.user_id);
    sqlx::query(
        "INSERT INTO conversations (id, user_id, agent_name, title, scope) \
         VALUES (?, ?, 'writer', 'chat 会话', 'agent')",
    )
    .bind(&conversation_id)
    .bind(&app.user_id)
    .execute(&app.state.db)
    .await
    .unwrap();

    let mut ids = Vec::new();
    for i in 0..n {
        let abs = write_work_file(app, &format!("output/chat/{i}.png"), b"PNG-CHAT");
        let ev = peco_server::artifact::build_tool_result_event(
            &app.state.db,
            Some(&work_dirs(app)),
            &conversation_id,
            &app.user_id,
            format!("call_chat_{i}"),
            "shell".to_string(),
            "done".to_string(),
            Vec::new(),
            &[decl_of(&abs, None)],
        )
        .await;
        let items = artifacts_of(ev);
        assert_eq!(
            items.len(),
            1,
            "第 {i} 条 chat 产物必须真落库，否则下面的隔离断言恒真"
        );
        ids.push(items[0].id.clone());
    }
    ids
}

async fn list_raw(app: &TestApp, query: &str) -> reqwest::Response {
    app.get(&format!("/api/peco/session/artifacts{query}"))
        .send()
        .await
        .unwrap()
}

/// 非整数分页参数由 axum 的 Query 提取层拒绝：框架默认的纯文本 400，不是
/// ApiError 信封 —— 故这里刻意不按 JSON envelope 解析。
async fn assert_non_integer_paging_rejected(app: &TestApp, query: &str) {
    let resp = list_raw(app, query).await;
    assert_eq!(resp.status(), 400, "query={query}");
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = resp.text().await.unwrap();
    assert!(
        !content_type.contains("application/json"),
        "非整数不该被包成 envelope（query={query}），content-type={content_type}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&body).is_err(),
        "响应体不是 JSON（query={query}）：{body}"
    );
}

/// 取一页并按 `ArtifactPage` 类型反序列化（形状错会在这里直接失败）。
async fn list_page(app: &TestApp, query: &str) -> ArtifactPage {
    let resp = list_raw(app, query).await;
    assert_eq!(resp.status(), 200, "列表端点必须 200（query={query}）");
    resp.json()
        .await
        .unwrap_or_else(|e| panic!("响应必须是 ArtifactPage 信封（query={query}）：{e}"))
}

fn ids_of(page: &ArtifactPage) -> BTreeSet<String> {
    page.artifacts.iter().map(|a| a.id.clone()).collect()
}

fn item_by_id<'a>(page: &'a ArtifactPage, id: &str) -> &'a ArtifactItem {
    page.artifacts
        .iter()
        .find(|a| a.id == id)
        .unwrap_or_else(|| panic!("列表缺产物 {id}"))
}

// ── 只返回本人产物 ──────────────────────────────────────────────────────────

/// 变异点：把派生 id 换成 query 里的 `conversation_id`（user2 带上 user1 的私有
/// 会话 id 就能拿到 user1 的产物）→ user2 的集合断言红；或查询漏掉会话归属过滤
/// → 两方集合相交。
///
/// 双方各造 2 项是刻意的：只比条数（2 == 2）在「user2 拿到 user1 的两项」时
/// 也成立，等于恒真。
#[tokio::test]
async fn peco_list_scoped_to_owner() {
    let app = TestApp::new().await;
    let user1_ids: BTreeSet<String> = seed_artifacts(&app, &app.user_id, 2)
        .await
        .into_iter()
        .collect();
    let (user2_id, token2) = app.register_user2().await;
    let user2_ids: BTreeSet<String> = seed_artifacts(&app, &user2_id, 2)
        .await
        .into_iter()
        .collect();
    assert_eq!(user1_ids.len(), 2);
    assert_eq!(user2_ids.len(), 2);

    let page1 = list_page(&app, "").await;
    assert_eq!(page1.total, 2);
    assert_eq!(ids_of(&page1), user1_ids, "user1 只能看到自己的 2 项");

    // 带上对方的私有会话 id 也不该换来对方的数据：端点根本不读这个参数。
    let bait = format!("?conversation_id={}", private_session_id(&app.user_id));
    let resp = app
        .get_as(&format!("/api/peco/session/artifacts{bait}"), &token2)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let page2: ArtifactPage = resp.json().await.unwrap();
    assert_eq!(page2.total, 2);
    assert_eq!(ids_of(&page2), user2_ids, "user2 只能看到自己的 2 项");
    assert!(ids_of(&page2).is_disjoint(&ids_of(&page1)));
}

// ── 同一用户的 chat 会话产物也不出现 ────────────────────────────────────────

/// 变异点：把 peco handler 的 `WHERE conversation_id = ?` 改成 `WHERE user_id = ?`
/// → user1 的 chat 会话产物混进列表，`total` 与 id 集合断言红。
///
/// 与上一条互补：那条只证明「别的用户的会话不可见」，按 `user_id` 查照样能过；
/// 这条造的是同一用户、不同 `scope` 的会话，只有按 `conversation_id` 查才过得去。
#[tokio::test]
async fn peco_list_excludes_chat_conversations() {
    let app = TestApp::new().await;
    let peco_ids: BTreeSet<String> = seed_artifacts(&app, &app.user_id, 2)
        .await
        .into_iter()
        .collect();
    let chat_ids: BTreeSet<String> = seed_chat_conversation_artifacts(&app, 2)
        .await
        .into_iter()
        .collect();

    assert_eq!(peco_ids.len(), 2);
    assert_eq!(chat_ids.len(), 2);
    assert!(
        peco_ids.is_disjoint(&chat_ids),
        "两侧造数必须落在不同 id 上，否则下面的断言无意义"
    );

    let page = list_page(&app, "").await;
    assert_eq!(page.total, 2, "chat 会话的产物不得计入 total");
    assert_eq!(ids_of(&page), peco_ids, "列表只含私有会话的产物");
}

// ── 最新在前 ────────────────────────────────────────────────────────────────

/// 变异点：换回既有的正序查询（`list_by_conversation`）→ 序列变成插入序，
/// 断言红。`created_at` 只到秒，三项同秒，实际定序靠 `rowid`。
#[tokio::test]
async fn peco_list_newest_first() {
    let app = TestApp::new().await;
    let inserted = seed_artifacts(&app, &app.user_id, 3).await;

    let page = list_page(&app, "").await;
    assert_eq!(page.total, 3);
    let got: Vec<&str> = page.artifacts.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(
        got,
        vec![
            inserted[2].as_str(),
            inserted[1].as_str(),
            inserted[0].as_str()
        ],
        "最新的在最前（插入序的倒序）"
    );
}

// ── 分页 ────────────────────────────────────────────────────────────────────

/// 变异点：忽略 `offset` → 第 2 页与第 1 页重叠，交集断言红；忽略 `limit` →
/// 第一页返回 5 项，条数断言红。
#[tokio::test]
async fn peco_list_paginates() {
    let app = TestApp::new().await;
    let seeded: BTreeSet<String> = seed_artifacts(&app, &app.user_id, 5)
        .await
        .into_iter()
        .collect();

    let first = list_page(&app, "?limit=2&offset=0").await;
    assert_eq!(first.artifacts.len(), 2);
    assert_eq!(first.total, 5, "total 是总数，不随分页变化");
    // 这两条测的是 envelope 回显契约（以及 clamp 生效后的值），不是分页生效；
    // 分页生效由下方的 is_disjoint 与三页并集断言证明。
    assert_eq!(first.limit, 2);
    assert_eq!(first.offset, 0);

    let second = list_page(&app, "?limit=2&offset=2").await;
    assert_eq!(second.artifacts.len(), 2);
    assert!(
        ids_of(&first).is_disjoint(&ids_of(&second)),
        "两页不得重叠：{:?} vs {:?}",
        ids_of(&first),
        ids_of(&second)
    );

    let third = list_page(&app, "?limit=2&offset=4").await;
    assert_eq!(third.artifacts.len(), 1, "第三页只剩 1 项");

    // 三页并起来必须正好覆盖全部 5 项：防「每页都返回同一段头部」这类假分页
    let union: BTreeSet<String> = ids_of(&first)
        .union(&ids_of(&second))
        .cloned()
        .chain(ids_of(&third))
        .collect();
    assert_eq!(union, seeded);
}

// ── 空态 200 且不建会话行 ───────────────────────────────────────────────────

/// 变异点：把空态改成 404（status 断言红），或让 GET 顺手 `ensure_private_session`
/// （会话行数变 1，count 断言红 —— GET 一旦有副作用就不再幂等）。
#[tokio::test]
async fn peco_list_empty_without_creating_session() {
    let app = TestApp::new().await;

    let page = list_page(&app, "").await;
    assert!(page.artifacts.is_empty());
    assert_eq!(page.total, 0);
    assert_eq!(page.limit, 200, "空态也回显生效的页大小");
    assert_eq!(page.offset, 0);

    assert_eq!(conversation_count(&app).await, 0, "GET 不得创建私有会话行");
}

// ── available 跟随文件系统 ──────────────────────────────────────────────────

/// 变异点：把 `available` 恒置 true（不 stat）→ 删文件后仍 true，中间一步红；
/// 恒置 false → 第一步红。
#[tokio::test]
async fn peco_list_available_follows_file_existence() {
    let app = TestApp::new().await;
    let abs = write_work_file(&app, "output/live/a.png", b"PNG");
    let items = artifacts_of(
        tool_result_event(&app, "call_live", "done", vec![decl_of(&abs, Some("还在"))]).await,
    );
    assert_eq!(items.len(), 1);
    let id = items[0].id.clone();

    assert!(
        item_by_id(&list_page(&app, "").await, &id).available,
        "文件在时 available 必须为 true"
    );

    std::fs::remove_file(&abs).unwrap();
    assert!(
        !item_by_id(&list_page(&app, "").await, &id).available,
        "文件被清理后必须为 false"
    );

    std::fs::write(&abs, b"PNG-AGAIN").unwrap();
    assert!(
        item_by_id(&list_page(&app, "").await, &id).available,
        "文件回来后必须重新为 true（不是一次性缓存）"
    );
}

// ── 分页参数一律 clamp ──────────────────────────────────────────────────────

/// 变异点：把 clamp 改成 400 → status 断言红；去掉 clamp（`limit=99999` 原样回显）
/// → `limit == 500` 断言红；把非整数也包成 ApiError 信封 → 纯文本断言红。
///
/// 前三组的期望值（1 / 0 / 500）都不等于传入值（0 / -1 / 99999），不是自证式断言；
/// `limit=500` 那组故意用等值断言 —— 上界本身合法，clamp 只对越界值生效。
#[tokio::test]
async fn peco_list_paging_clamped() {
    let app = TestApp::new().await;
    seed_artifacts(&app, &app.user_id, 3).await;

    let resp = list_raw(&app, "?limit=0").await;
    assert_eq!(resp.status(), 200);
    let page: ArtifactPage = resp.json().await.unwrap();
    assert_eq!(page.limit, 1, "limit < 1 收到 1");
    // 造了 3 项、生效 limit 为 1：回 0 项同样算坏，故用等值而非上界
    assert_eq!(page.artifacts.len(), 1);

    let home = list_page(&app, "").await;
    let negative = list_page(&app, "?offset=-1").await;
    assert_eq!(negative.offset, 0, "offset < 0 收到 0");
    assert_eq!(
        ids_of(&negative),
        ids_of(&home),
        "offset 收敛到 0 后就是首页"
    );

    let resp = list_raw(&app, "?limit=99999").await;
    assert_eq!(resp.status(), 200);
    let page: ArtifactPage = resp.json().await.unwrap();
    assert_eq!(page.limit, 500, "limit > 500 收到 500");

    // 上界本身是可接受值：clamp 只对越界生效，回显必须原样 500
    let page = list_page(&app, "?limit=500").await;
    assert_eq!(page.limit, 500, "limit == 500 不该被夹");

    assert_non_integer_paging_rejected(&app, "?limit=abc").await;
    assert_non_integer_paging_rejected(&app, "?offset=abc").await;
}

// ── 需要认证 ────────────────────────────────────────────────────────────────

/// 变异点：把路由挂到不带 auth 的 router（或去掉 `AuthUser` extractor）→ 200，
/// 断言红。
#[tokio::test]
async fn peco_list_requires_auth() {
    let app = TestApp::new().await;

    let resp = app
        .client
        .get(format!("{}/api/peco/session/artifacts", app.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

// ── 清空对话不动产物 ────────────────────────────────────────────────────────

/// 变异点：给 `clear_session` 加上删产物行（或删文件后行仍在但过滤掉）→
/// 清空后的 total / id 集合断言红。
#[tokio::test]
async fn peco_clear_does_not_delete_artifacts() {
    let app = TestApp::new().await;
    let seeded: BTreeSet<String> = seed_artifacts(&app, &app.user_id, 2)
        .await
        .into_iter()
        .collect();

    let resp = app.delete("/api/peco/session").send().await.unwrap();
    assert_eq!(resp.status(), 200, "清空对话必须成功");

    let page = list_page(&app, "").await;
    assert_eq!(page.total, 2, "清空对话 = 清历史，不删产物");
    assert_eq!(ids_of(&page), seeded);
}
