// ============================================================================
// TestApp — 集成测试辅助工具
// ============================================================================
//
// 每个测试用例获得一个独立的 TestApp 实例，包含：
// - 临时 SQLite 数据库（独立文件，非 :memory:，以支持连接池）
// - 临时数据目录
// - 随机端口绑定的 Axum server
// - 预注册的测试用户及其 JWT token
//
// # 用法
//
// ```ignore
// #[tokio::test]
// async fn test_something() {
//     let app = TestApp::new().await;
//     let resp = app.post("/api/agents").json(...).send().await;
//     assert_eq!(resp.status(), 201);
// }
// ```

use std::sync::Arc;

use peco_server::build_router;
use peco_server::config::ServerConfig;
use peco_server::db;
use peco_server::state::AppState;
use peco_server::workflow::scheduler::CronScheduler;
use reqwest::Client;
use serde::Deserialize;
use tokio::net::TcpListener;

// ── TestApp ───────────────────────────────────────────────────────────────────

/// 集成测试应用实例。
///
/// 包含一个完整运行的 HTTP server、预认证的客户端和直接访问 DB 的能力。
pub struct TestApp {
    /// 服务端地址（如 `http://127.0.0.1:45678`）。
    pub base_url: String,
    /// 应用全局状态（可直接操作 DB 准备/验证数据）。
    #[allow(dead_code)]
    pub state: Arc<AppState>,
    /// 预配置的 HTTP 客户端。
    pub client: Client,
    /// 预注册测试用户的 JWT token。
    pub user_token: String,
    /// 预注册测试用户的 user_id。
    #[allow(dead_code)]
    pub user_id: String,
    /// 测试数据目录（Drop 时清理）。
    _temp_dir: tempfile::TempDir,
    /// 后台 server task 的 abort handle。
    _server_handle: ServerHandle,
}

/// 后台 server 的 abort handle —— drop 时自动停止 server。
struct ServerHandle(Option<tokio::task::AbortHandle>);

impl Drop for ServerHandle {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            h.abort();
        }
    }
}

impl TestApp {
    /// 创建全新的 TestApp 实例。
    ///
    /// 步骤：
    /// 1. 创建临时目录
    /// 2. 初始化 SQLite DB + 迁移
    /// 3. 创建 CronScheduler + AppState
    /// 4. 构建 Router → 启动 server（随机端口）
    /// 5. 注册测试用户 → 获取 JWT token
    pub async fn new() -> Self {
        // ── 1. 创建临时目录 ──────────────────────────────────────────────────
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let data_dir = temp_dir.path().to_path_buf();
        let db_path = data_dir.join("test.db");

        tokio::fs::create_dir_all(&data_dir).await.unwrap();

        // ── 2. 初始化 SQLite DB ──────────────────────────────────────────────
        let database_url = format!("sqlite:{}?mode=rwc", db_path.display());
        let pool = db::connect(&database_url)
            .await
            .expect("failed to connect to test DB");
        db::run_migrations(&pool)
            .await
            .expect("failed to run migrations");

        // ── 3. 创建 CronScheduler ────────────────────────────────────────────
        let cron_scheduler = Arc::new(
            CronScheduler::new()
                .await
                .expect("failed to create CronScheduler"),
        );

        // ── 4. 创建 AppState ─────────────────────────────────────────────────
        let jwt_secret = "test-secret-key-do-not-use-in-production".to_string();
        let config = ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0, // OS 分配随机端口
            database_url,
            jwt_secret,
            data_dir: data_dir.clone(),
        };
        let state = Arc::new(AppState::new(&config, pool, cron_scheduler.clone()).await);

        // ── 5. 构建 Router + 绑定随机端口 ────────────────────────────────────
        let app = build_router(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind");
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{}", addr);

        // 在后台运行 server
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let server_handle = ServerHandle(Some(server_task.abort_handle()));

        // ── 6. 创建 HTTP 客户端 ──────────────────────────────────────────────
        let client = Client::new();

        // ── 7. 注册测试用户 ──────────────────────────────────────────────────
        let (user_id, user_token) = Self::register_test_user(&base_url, &client).await;

        Self {
            base_url,
            state,
            client,
            user_token,
            user_id,
            _temp_dir: temp_dir,
            _server_handle: server_handle,
        }
    }

    /// 注册一个测试用户，返回 (user_id, token)。
    async fn register_test_user(base_url: &str, client: &Client) -> (String, String) {
        #[derive(Deserialize)]
        struct AuthResponse {
            user: UserData,
            token: String,
        }
        #[derive(Deserialize)]
        struct UserData {
            id: String,
        }

        let resp = client
            .post(format!("{base_url}/api/auth/register"))
            .json(&serde_json::json!({
                "username": "testuser",
                "email": "test@example.com",
                "password": "testpass123"
            }))
            .send()
            .await
            .expect("failed to register test user");

        assert!(
            resp.status().is_success(),
            "test user registration failed: {}",
            resp.status()
        );

        let body: AuthResponse = resp.json().await.expect("failed to parse auth response");
        (body.user.id, body.token)
    }

    // ── 辅助方法 ──────────────────────────────────────────────────────────────

    /// 注册第二个用户，返回 (user_id, token)。用于权限隔离测试。
    #[allow(dead_code)]
    pub async fn register_user2(&self) -> (String, String) {
        #[derive(Deserialize)]
        struct AuthResponse {
            user: UserData,
            token: String,
        }
        #[derive(Deserialize)]
        struct UserData {
            id: String,
        }

        let resp = self
            .client
            .post(format!("{}/api/auth/register", self.base_url))
            .json(&serde_json::json!({
                "username": "testuser2",
                "email": "test2@example.com",
                "password": "testpass456"
            }))
            .send()
            .await
            .expect("failed to register second user");

        let body: AuthResponse = resp.json().await.expect("failed to parse auth response");
        (body.user.id, body.token)
    }

    /// 发送带认证的 GET 请求。
    pub fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{}{}", self.base_url, path))
            .bearer_auth(&self.user_token)
    }

    /// 发送带认证的 POST 请求。
    #[allow(dead_code)]
    pub fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}{}", self.base_url, path))
            .bearer_auth(&self.user_token)
    }

    /// 发送带认证的 PATCH 请求。
    #[allow(dead_code)]
    pub fn patch(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .patch(format!("{}{}", self.base_url, path))
            .bearer_auth(&self.user_token)
    }

    /// 发送带认证的 DELETE 请求。
    #[allow(dead_code)]
    pub fn delete(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .delete(format!("{}{}", self.base_url, path))
            .bearer_auth(&self.user_token)
    }

    /// 使用第二个用户的 token 发送 GET 请求。
    #[allow(dead_code)]
    pub fn get_as(&self, path: &str, token: &str) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{}{}", self.base_url, path))
            .bearer_auth(token)
    }
}

// ── TestServerConfig ───────────────────────────────────────────────────────────
// 由于 ServerConfig 无 pub 字段构造器且不方便在测试中直接使用，
// 此处直接暴露字段以供 TestApp::new() 构造。
//
// 注意：ServerConfig 在 config.rs 中定义为 pub struct 且所有字段均 pub，
// 因此可直接通过字面量初始化。
