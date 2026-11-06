//! 进程内共享状态：HTTP 处理函数与启动链路的唯一依赖入口。
//!
//! 打包成一个 `Clone` 结构体而不是让 handler 各取所需：`AppState` 是
//! `Router::with_state` 的唯一参数，加依赖时只改这里，不必改每个 handler 签名。
//!
//! 已打开实例表只有一张：`peco_core::tools::WorkspaceHub`。daemon 不再维护
//! `ws_id → 实例` 的第二份缓存；ws_id 与目录的映射只存在 `workspaces.work_dir`。

use std::path::PathBuf;
use std::sync::Arc;

use peco_core::agent::LooperConfig;
use peco_core::agent::hooks::DispatchWatchHook;
use peco_core::tools::WorkspaceHub;
use sqlx::SqlitePool;

use crate::chat::active::ChatActiveRuns;
use crate::config::DaemonConfig;
use crate::dynamic::SpaceCatalogContext;
use crate::space::SpaceEnv;
use crate::space::registry::SpaceRegistry;
use crate::space::tasks::TaskObserver;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub hub: Arc<WorkspaceHub>,
    pub env: Arc<SpaceEnv>,
    pub config: Arc<DaemonConfig>,
    /// 跨空间任务台账 sink + 广播（HTTP 查询与未来 SSE 共用）。
    pub tasks: Arc<TaskObserver>,
    /// 资源模块（agent / skill / knowledge / mcp）把 `ws_id` 变成实例的唯一入口。
    pub spaces: Arc<SpaceRegistry>,
    /// 会话级 chat 控制注册表：暂停/恢复必须从**另一条请求**触达在途 looper，
    /// 而 looper 由 SSE 请求任务独占持有，故只能在这里留一个会话 → 控制通道的表。
    pub chat_runs: Arc<ChatActiveRuns>,
}

impl AppState {
    /// 为一次 AgentLooper 会话组装派发观察 hook。
    ///
    /// 只是转调 [`SpaceEnv::dispatch_hook`]：空间打开时的 machine 执行体走同一条
    /// 构造路径，两处各写一份会让 `origin_dir` / `principal` 的取值漂移。
    pub fn dispatch_hook(
        &self,
        origin_ws_id: String,
        principal: String,
        origin_dir: PathBuf,
    ) -> Arc<DispatchWatchHook> {
        self.env.dispatch_hook(origin_ws_id, principal, origin_dir)
    }

    /// 为一次 `AgentLooper` 会话组装配置：观察 hook 与动态上下文的唯一装配点。
    ///
    /// 清单只给 Peco 自己的空间：它列的是「可派发目标 + 当前空间」，团队总管拿到
    /// 这份清单就会把任务派给不属于自己的空间。Peco 与其它空间共用同一条会话
    /// 路径，因此差异只能落在这里。
    pub fn looper_config(&self, ws_id: &str, team_id: &str, work_dir: PathBuf) -> LooperConfig {
        // push 而不是整体覆盖 `hooks`：`from_env()` 给出的 hook 链仍要生效；
        // 少了这一条，本轮派发出去的异步任务没有任何人观察。
        let mut config = LooperConfig {
            event_buffer: 256,
            persist_on_failure: true,
            ..LooperConfig::from_env()
        };
        config
            .hooks
            .push(self.dispatch_hook(ws_id.to_string(), team_id.to_string(), work_dir));
        if self.env.peco.is_peco_space(ws_id) {
            config.dynamic_context = Some(Arc::new(SpaceCatalogContext::new(
                self.pool.clone(),
                self.hub.clone(),
                self.env.peco.clone(),
                self.env.home.clone(),
            )));
        }
        config
    }
}
