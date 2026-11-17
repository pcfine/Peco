//! 集成测试共用的夹具：临时 `$PECO_HOME`、单用户 DB、进程内 Hub 与计数型 opener。
//!
//! 测试不调用 `install_workspace_hub`：同一进程的多个测试会撞 `OnceLock`。
//! 每个 `TestEnv` 自己构造一个带自定义 opener 的局部 `WorkspaceHub`。

// 每个测试文件各自 `mod support;`，未被本文件用到的辅助函数不算问题。
#![allow(dead_code)]

pub mod upstream;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use peco_core::config::SystemConfig;
use peco_core::tools::{
    OpenRequest, WorkspaceHandle, WorkspaceHub, WorkspaceOpenError, WorkspaceOpener,
};
use peco_daemon::config::DaemonConfig;
use peco_daemon::db;
use peco_daemon::db::dispatches::{self, NewDispatch, WakeSkipReason, WakeState};
use peco_daemon::db::workspaces;
use peco_daemon::error::DaemonError;
use peco_daemon::space::SpaceEnv;
use peco_daemon::space::ledger::SqliteTaskLedger;
use peco_daemon::space::open::{self, DaemonOpener, ensure_open_indexed};
use peco_daemon::space::registry::SpaceRegistry;
use peco_daemon::space::tasks::TaskObserver;
use peco_daemon::state::AppState;
use peco_daemon::wake::pump::WakePump;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use upstream::FakeUpstream;

/// 包装真实 `DaemonOpener`：只在打开成功时计数。
///
/// 成功计数保留旧夹具的语义 ——「失败的那次没有真正建出实例」，而 Hub 的
/// 单飞保证并发同一目录只成功一次。
pub struct CountingOpener {
    inner: Arc<DaemonOpener>,
    calls: AtomicUsize,
}

impl CountingOpener {
    fn new(inner: Arc<DaemonOpener>) -> Self {
        Self {
            inner,
            calls: AtomicUsize::new(0),
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl WorkspaceOpener for CountingOpener {
    async fn open(&self, req: OpenRequest) -> Result<Arc<dyn WorkspaceHandle>, WorkspaceOpenError> {
        let result = self.inner.open(req).await;
        if result.is_ok() {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        result
    }
}

pub struct TestEnv {
    pub home: PathBuf,
    pub pool: SqlitePool,
    pub env: Arc<SpaceEnv>,
    pub hub: Arc<WorkspaceHub>,
    pub config: Arc<DaemonConfig>,
    pub opener: Arc<CountingOpener>,
    pub tasks: Arc<TaskObserver>,
    /// 与观察者同一份的投递泵；投递用例要给它接上 `PecoDelivery` 才能跑。
    pub wake: Arc<WakePump>,
    _home_guard: TempDir,
}

/// 建一个空 `$PECO_HOME`，按 `teams` 建出能力根目录，跑完全部迁移。
pub async fn new_env(teams: &[&str]) -> TestEnv {
    let home_guard = tempfile::tempdir().unwrap();
    for team in teams {
        std::fs::create_dir_all(home_guard.path().join("teams").join(team)).unwrap();
    }

    // 走 load_from 而不是 load：环境变量不该影响测试的实际路径与默认值。
    let config = DaemonConfig::load_from(home_guard.path()).unwrap();
    let pool = db::connect(&config.db_url()).await.unwrap();
    db::run_migrations(&pool).await.unwrap();

    // 与 `main` 同源：基座恒为空 providers，系统层是 `$PECO_HOME/providers.toml`
    // 这一最低优先文件层。读 core 的默认位置会让用例随开发机的配置漂移。
    let system_config = peco_daemon::config::base_system_config();
    let peco = Arc::new(peco_daemon::space::peco::resolve(&config.peco_home));
    let wake = Arc::new(WakePump::new(pool.clone(), peco.clone()));
    let tasks = Arc::new(TaskObserver::new(pool.clone(), wake.clone()));
    let env = Arc::new(SpaceEnv::new(
        &config,
        pool.clone(),
        system_config.clone(),
        peco,
        tasks.clone(),
    ));
    let daemon_opener = Arc::new(DaemonOpener::new(env.clone()));
    let opener = Arc::new(CountingOpener::new(daemon_opener));
    let hub = Arc::new(
        WorkspaceHub::with_system_config(Arc::new(system_config))
            .with_opener(opener.clone() as Arc<dyn WorkspaceOpener>)
            // daemon 是单用户宿主：label 只作展示，不做隔离校验。
            .with_label_enforcement(false)
            // 与 `main` 同装：重启后回退查台账的路径在测试里也走真的 SQLite。
            .with_ledger(Arc::new(SqliteTaskLedger::new(pool.clone()))),
    );
    // 与 daemon 启动同序：先建 Hub，再把它注入 SpaceEnv（文件监听扇出要用）。
    env.attach_hub(&hub);
    let config = Arc::new(config);

    TestEnv {
        home: config.peco_home.clone(),
        pool,
        env,
        hub,
        config,
        opener,
        tasks,
        wake,
        _home_guard: home_guard,
    }
}

/// 团队目录里的一个 agent：`agent.md` 的 `name` 与文件名一致。
pub struct Fixture<'a> {
    pub name: &'a str,
    /// 同时是假上游的分派键：脚本按模型名决定这一轮该做什么。
    pub model: &'a str,
    pub tools: &'a [&'a str],
}

pub fn fixture<'a>(name: &'a str, model: &'a str, tools: &'a [&'a str]) -> Fixture<'a> {
    Fixture { name, model, tools }
}

/// 写一个团队目录：`providers.toml` 指向假上游，每个 agent 一个 `agent.md`。
///
/// `agent.md` 不设 `llm.stream`，于是 looper 走批量路径 —— 假上游只需实现
/// 非流式的 chat completions。
///
/// 纯文件写入，故是同步的：包成 `async` 会让每个调用点多一个永不 pending 的
/// `.await`，读起来像「这里要做 IO 等待」。
pub fn install_team(env: &TestEnv, team: &str, upstream: &FakeUpstream, agents: &[Fixture<'_>]) {
    let root = env.home.join("teams").join(team);
    std::fs::create_dir_all(root.join("agents")).expect("create the team root");
    let providers = format!(
        "default_provider = \"fake\"\n\n\
         [providers.fake]\n\
         type = \"openai\"\n\
         api = \"chat\"\n\
         base_url = \"{}\"\n\
         api_key = \"test\"\n\n\
         [providers.fake.default]\n\
         model = \"unused\"\n",
        upstream.base_url
    );
    std::fs::write(root.join("providers.toml"), providers).expect("write providers.toml");

    for agent in agents {
        let dir = root.join("agents").join(agent.name);
        std::fs::create_dir_all(&dir).expect("create the agent dir");
        let tools = if agent.tools.is_empty() {
            String::new()
        } else {
            let lines: String = agent
                .tools
                .iter()
                .map(|tool| format!("  - {tool}\n"))
                .collect();
            format!("tools:\n{lines}")
        };
        let md = format!(
            "---\nagent:\n  name: \"{}\"\n  description: \"closure fixture\"\n\
             llm:\n  provider: \"fake\"\n  model: \"{}\"\n  temperature: 0.0\n{tools}---\n",
            agent.name, agent.model
        );
        std::fs::write(dir.join("agent.md"), md).expect("write agent.md");
    }
}

impl TestEnv {
    pub fn app_state(&self) -> AppState {
        self.with_env(self.env.clone())
    }

    pub fn router(&self) -> Router {
        peco_daemon::http::router(self.app_state())
    }

    /// 换一份 Peco 身份重建 `AppState`：等价于「改完记录文件后重启 daemon」。
    ///
    /// `/api/peco` 回的是**启动时**解析出的身份，只有换掉 `SpaceEnv` 才能观察到
    /// 记录文件的改动；若哪天改成请求时读盘，这条夹具就会立刻失效。
    pub fn app_state_with_peco(
        &self,
        peco: Arc<peco_daemon::space::peco::PecoIdentity>,
    ) -> AppState {
        self.with_env(self.env_with_system_config(peco_daemon::config::base_system_config(), peco))
    }

    /// 换一份系统配置重建 `AppState`：系统层的 provider 兜底只在这一层可见，
    /// 想断言「系统层补的 key 也算已配置」就必须先把它注入进来。
    pub fn app_state_with_system_config(&self, system_config: SystemConfig) -> AppState {
        let peco = Arc::new(peco_daemon::space::peco::resolve(&self.config.peco_home));
        self.with_env(self.env_with_system_config(system_config, peco))
    }

    fn env_with_system_config(
        &self,
        system_config: SystemConfig,
        peco: Arc<peco_daemon::space::peco::PecoIdentity>,
    ) -> Arc<SpaceEnv> {
        let env = Arc::new(SpaceEnv::new(
            &self.config,
            self.pool.clone(),
            system_config,
            peco,
            self.tasks.clone(),
        ));
        env.attach_hub(&self.hub);
        env
    }

    fn with_env(&self, env: Arc<SpaceEnv>) -> AppState {
        AppState {
            pool: self.pool.clone(),
            hub: self.hub.clone(),
            env: env.clone(),
            config: self.config.clone(),
            tasks: self.tasks.clone(),
            spaces: Arc::new(SpaceRegistry::new(self.hub.clone(), env)),
            chat_runs: Arc::new(peco_daemon::chat::ChatActiveRuns::new()),
            wake: self.wake.clone(),
        }
    }

    /// 建出 `spaces/<name>/work` 并返回它 —— `work_dir` 必须已存在，
    /// 否则规范化结果会依赖「目录是否已创建」。
    pub fn work_dir(&self, name: &str) -> PathBuf {
        let dir = self.home.join("spaces").join(name).join("work");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub async fn resolve(&self, dir: &Path, team_id: &str) -> Result<String, DaemonError> {
        open::resolve_ws_id(&self.env, dir, team_id).await
    }

    /// 按 `ws_id` 打开已索引空间（daemon 启动恢复同一条路径）。
    pub async fn open(&self, ws_id: &str) -> Result<Arc<dyn WorkspaceHandle>, DaemonError> {
        ensure_open_indexed(&self.env, &self.hub, ws_id).await
    }

    /// 丢弃缓存实例后重新走一次磁盘决策表。
    ///
    /// 旧测试直接调 `open_workspace` 重跑打开逻辑；Hub 会先返回缓存实例，
    /// 因此这里显式 `forget`，保证 marker/目录的变更能被重新观察到。
    pub async fn reopen(&self, ws_id: &str) -> Result<Arc<dyn WorkspaceHandle>, DaemonError> {
        let row = workspaces::by_id(&self.pool, ws_id)
            .await
            .unwrap()
            .expect("空间行必须存在");
        self.hub.forget(Path::new(&row.work_dir));
        ensure_open_indexed(&self.env, &self.hub, ws_id).await
    }

    pub async fn is_open(&self, ws_id: &str) -> bool {
        match workspaces::by_id(&self.pool, ws_id).await.unwrap() {
            Some(row) => self.hub.is_open(Path::new(&row.work_dir)),
            None => false,
        }
    }

    pub async fn work_dir_of(&self, ws_id: &str) -> PathBuf {
        let row = workspaces::by_id(&self.pool, ws_id)
            .await
            .unwrap()
            .expect("空间行必须存在");
        PathBuf::from(row.work_dir)
    }

    pub async fn count_users(&self) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    pub async fn count_workspaces(&self, work_dir: &Path) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM workspaces WHERE work_dir = ?")
            .bind(work_dir.to_string_lossy().into_owned())
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    pub async fn count_dispatches(&self, target_ws_id: &str) -> i64 {
        dispatches::count_by_target(&self.pool, target_ws_id)
            .await
            .unwrap()
    }

    pub async fn insert_dispatch(
        &self,
        target_ws_id: &str,
        task_id: &str,
        origin_kind: &str,
        origin_ws_id: &str,
        task_state: &str,
    ) -> u64 {
        let id = format!(
            "d_{task_id}_{}",
            peco_core::workspace::manifest::new_ws_id()
        );
        dispatches::insert(
            &self.pool,
            &NewDispatch {
                id: &id,
                task_id,
                target_ws_id,
                target_work_dir: "/tmp/work",
                target_team_id: "research",
                origin_ws_id,
                agent_name: "@coder",
                origin_kind,
                task_state,
                wake_state: WakeState::Pending,
                wake_skip_reason: None,
                prompt_digest: None,
                dispatch_id: None,
            },
        )
        .await
        .unwrap()
    }

    /// §8.5 L3 补记形态的外来在飞：命中 `inflight_foreign`，且不带唤醒。
    pub async fn insert_l3_foreign(&self, target_ws_id: &str, task_id: &str) -> u64 {
        let id = format!(
            "d_{task_id}_{}",
            peco_core::workspace::manifest::new_ws_id()
        );
        dispatches::insert(
            &self.pool,
            &NewDispatch {
                id: &id,
                task_id,
                target_ws_id,
                target_work_dir: "/tmp/work",
                target_team_id: "research",
                origin_ws_id: target_ws_id,
                agent_name: "@master",
                origin_kind: "run_machine_task",
                task_state: "working",
                wake_state: WakeState::Skipped,
                wake_skip_reason: Some(WakeSkipReason::NotPecoOrigin),
                prompt_digest: None,
                dispatch_id: None,
            },
        )
        .await
        .unwrap()
    }

    /// 播种一条待唤醒的台账行。
    ///
    /// `seq` 写进 `created_at`：同一秒的时间戳分不出先后，`list_pending` 的排序
    /// 会退化到随机 id，跨行断言的「前几项」就不再稳定。
    pub async fn pending_row(
        &self,
        seq: i64,
        task_id: &str,
        task_state: &str,
        digest: Option<&str>,
    ) {
        let id = format!("d_{task_id}");
        sqlx::query(
            "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, \
             target_team_id, origin_ws_id, agent_name, origin_kind, task_state, wake_state, \
             result_digest, created_at) \
             VALUES (?, ?, 'ws_target', '/tmp/work', 'research', 'ws_origin', '@master', \
             'run_machine_task', ?, 'pending', ?, ?)",
        )
        .bind(&id)
        .bind(task_id)
        .bind(task_state)
        .bind(digest)
        .bind(format!("2026-01-01 00:00:{seq:02}"))
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// 播种一条「Peco 自己被唤醒」的行：发起方与目标都是 Peco 空间，摘要非空。
    ///
    /// 与 [`Self::pending_row`] 的差别就是这两个 `ws_id`：唤醒投递要按发起方判
    /// 归属，用夹具里的占位空间名会让这条行看起来来自别的空间。
    pub async fn peco_pending_row(&self, seq: i64, task_id: &str, peco_ws_id: &str) {
        sqlx::query(
            "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, \
             target_team_id, origin_ws_id, agent_name, origin_kind, task_state, wake_state, \
             result_digest, created_at) \
             VALUES (?, ?, ?, '/tmp/work', 'peco', ?, '@master', 'run_machine_task', \
             'completed', 'pending', '重构完成，12 个测试通过', ?)",
        )
        .bind(format!("d_{task_id}"))
        .bind(task_id)
        .bind(peco_ws_id)
        .bind(peco_ws_id)
        .bind(format!("2026-01-01 00:00:{seq:02}"))
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// 资源模块用的 `ws_id → 实例` 适配层；与 `AppState` 里那份同源同库。
    pub fn spaces(&self) -> Arc<SpaceRegistry> {
        Arc::new(SpaceRegistry::new(self.hub.clone(), self.env.clone()))
    }

    /// `d_dispatches` 一行的 `(task_state, wake_state)`：两套状态域各自断言。
    pub async fn task_and_wake_state(&self, target_ws_id: &str, task_id: &str) -> (String, String) {
        sqlx::query_as::<_, (String, String)>(
            "SELECT task_state, wake_state FROM d_dispatches \
             WHERE target_ws_id = ? AND task_id = ?",
        )
        .bind(target_ws_id)
        .bind(task_id)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// 该行的跳过成因；`None` 表示没跳过，或历史行的成因不可考。
    pub async fn wake_skip_reason(&self, target_ws_id: &str, task_id: &str) -> Option<String> {
        sqlx::query_scalar(
            "SELECT wake_skip_reason FROM d_dispatches \
             WHERE target_ws_id = ? AND task_id = ?",
        )
        .bind(target_ws_id)
        .bind(task_id)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    pub async fn team_ids(&self) -> Vec<String> {
        peco_daemon::db::teams::list_ids(&self.pool).await.unwrap()
    }
}

/// 一次 HTTP 往返的结果：状态码、解析后的 JSON 体。
pub struct HttpReply {
    pub status: StatusCode,
    pub body: Value,
}

impl HttpReply {
    pub fn code(&self) -> &str {
        self.body["code"].as_str().unwrap_or_default()
    }
}

/// 走 `tower::ServiceExt::oneshot`：真实经过路由、中间件与提取器，不占端口。
pub async fn call(router: &Router, method: Method, uri: &str, body: Option<Value>) -> HttpReply {
    call_in_space(router, method, uri, body, None).await
}

/// 同上，附带空间选择器请求头。
///
/// 资源模块的每个 handler 都要求 `LocalUser`，缺这条头就只能走 `?ws=` 兜底，
/// 两条路径要分开测。
pub async fn call_in_space(
    router: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    ws_id: Option<&str>,
) -> HttpReply {
    use tower::ServiceExt;

    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(ws_id) = ws_id {
        builder = builder.header("X-Peco-Space", ws_id);
    }
    let request_body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(request_body).unwrap())
        .await
        .expect("router 不会失败");

    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("响应体可读");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    HttpReply { status, body }
}

/// 发起一次请求并返回**未消费**的响应体。
///
/// 断连用例要能「读 N 帧后丢弃响应体」：[`call_in_space`] 会把整个响应体读完，
/// 造不出「连接中途消失」。语义与 [`call_in_space`] 的唯一差别就是最后不读体。
pub async fn stream_in_space(
    router: &Router,
    method: Method,
    uri: &str,
    ws_id: Option<&str>,
) -> axum::response::Response {
    use tower::ServiceExt;

    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(ws_id) = ws_id {
        builder = builder.header("X-Peco-Space", ws_id);
    }
    router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .expect("router 不会失败")
}

impl TestEnv {
    pub async fn get(&self, uri: &str) -> HttpReply {
        call(&self.router(), Method::GET, uri, None).await
    }

    pub async fn post(&self, uri: &str, body: Value) -> HttpReply {
        call(&self.router(), Method::POST, uri, Some(body)).await
    }

    pub async fn patch(&self, uri: &str, body: Value) -> HttpReply {
        call(&self.router(), Method::PATCH, uri, Some(body)).await
    }

    pub async fn delete(&self, uri: &str) -> HttpReply {
        call(&self.router(), Method::DELETE, uri, None).await
    }

    pub async fn get_in(&self, ws_id: &str, uri: &str) -> HttpReply {
        call_in_space(&self.router(), Method::GET, uri, None, Some(ws_id)).await
    }

    pub async fn post_in(&self, ws_id: &str, uri: &str, body: Value) -> HttpReply {
        call_in_space(&self.router(), Method::POST, uri, Some(body), Some(ws_id)).await
    }

    pub async fn patch_in(&self, ws_id: &str, uri: &str, body: Value) -> HttpReply {
        call_in_space(&self.router(), Method::PATCH, uri, Some(body), Some(ws_id)).await
    }

    pub async fn put_in(&self, ws_id: &str, uri: &str, body: Value) -> HttpReply {
        call_in_space(&self.router(), Method::PUT, uri, Some(body), Some(ws_id)).await
    }

    pub async fn delete_in(&self, ws_id: &str, uri: &str) -> HttpReply {
        call_in_space(&self.router(), Method::DELETE, uri, None, Some(ws_id)).await
    }

    /// 建出一个已打开的空间，返回 `ws_id`（`POST /api/spaces` 成功即代表实例在表里）。
    pub async fn open_space(&self, name: &str, team_id: &str) -> String {
        let work_dir = self.work_dir(name);
        let reply = self
            .post(
                "/api/spaces",
                json!({"work_dir": work_dir.to_string_lossy(), "team_id": team_id}),
            )
            .await;
        assert_eq!(reply.status, 201, "建空间失败: {}", reply.body);
        reply.body["ws_id"].as_str().unwrap().to_string()
    }
}
