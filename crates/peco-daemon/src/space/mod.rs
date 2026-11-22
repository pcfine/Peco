//! 空间生命周期：`.workspace.json` 读写、打开决策表、关闭与删除门槛。

pub mod bootstrap;
pub mod close;
pub mod compaction;
pub mod compaction_hooks;
pub mod fanout;
pub mod index;
pub mod ledger;
pub mod manifest;
pub mod memory_audit;
pub mod open;
pub mod peco;
pub mod reconcile;
pub mod registry;
pub mod report;
pub mod tasks;
pub mod trash;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use model_provider::ModelProvider;
use peco_core::agent::hooks::{DispatchOrigin, DispatchWatchHook, TaskObservationSink};
use peco_core::agent::{
    DynamicContext, LooperConfig, ModelSummarizer, PecoContextFilter, TurnSummarizer,
};
use peco_core::config::SystemConfig;
use peco_core::knowledge::KnowledgeManager;
use peco_core::tools::WorkspaceHub;
use sqlx::SqlitePool;

use crate::LOCAL_USER_ID;
use crate::config::DaemonConfig;
use crate::db::workspaces::{self, WorkspaceRow};
use crate::dynamic::SpaceCatalogContext;
use crate::error::DaemonError;
use crate::memory::analyzer::ModelTurnAnalyzer;
use crate::memory::compose::ComposedDynamicContext;
use crate::memory::config::MemoryConfig;
use crate::memory::hook::MemoryExtractionHook;
use crate::memory::recall::MemoryRecallContext;
use crate::space::compaction_hooks::{CompactionMetricsHook, ContextShadowHook};
use crate::space::peco::PecoIdentity;
use crate::space::tasks::TaskObserver;

/// 打开/关闭一个空间所需的全部外部依赖。
///
/// 打包成一个结构体而不是散着传：打开决策表、关闭门槛与 HTTP handler 必须看
/// 同一份连接池与同一个能力根，否则「按目录判定团队存在」会各说各话。
///
/// 已打开实例表不在这里 —— 它仍是进程级 `peco_core::tools::WorkspaceHub`。
/// 这里只留一个弱引用，让「打开实例之后」的动作（文件监听扇出）能拿到同一张
/// 表；本结构体不承担缓存职责。
pub struct SpaceEnv {
    pub home: PathBuf,
    /// 系统层 provider 文件所在的目录（`config::PROVIDERS_FILE` 拼在它后面）。
    ///
    /// 启动时从 [`DaemonConfig`] 捕获一次：`PECO_PROVIDERS_CONFIG` 改了要重启
    /// 才生效，不在运行期重读 —— 那会造出第二个环境边界。
    providers_root: PathBuf,
    pub pool: SqlitePool,
    pub system_config: SystemConfig,
    /// Peco 默认空间的身份；台账分组计数与 HTTP 视图共用这一份。
    pub peco: Arc<PecoIdentity>,
    /// 台账写入端：观察 hook 是它的生产者，打开空间的用例需要拿到同一份
    /// （另建一个 `TaskObserver` 会让广播事件落进没人订阅的第二个通道）。
    pub tasks: Arc<TaskObserver>,
    /// 已打开实例表，晚绑定：Hub 需要 opener，opener 又需要 Hub 才能把文件
    /// 变更扇出到同团队的其它空间，两者只能有一方事后注入。持 `Weak` 是为了
    /// 不在 Hub 之外给它续命。
    hub: OnceLock<Weak<WorkspaceHub>>,
}

impl SpaceEnv {
    pub fn new(
        config: &DaemonConfig,
        pool: SqlitePool,
        system_config: SystemConfig,
        peco: Arc<PecoIdentity>,
        tasks: Arc<TaskObserver>,
    ) -> Self {
        // 唤醒判据要认「谁是 Peco」，而观察者先于本结构体构造：两份值只在
        // 这个装配点同时在场，身份因此在这里交接。
        tasks.attach_peco(peco.clone());
        Self {
            home: config.peco_home.clone(),
            providers_root: config.system_providers_root().to_path_buf(),
            pool,
            system_config,
            peco,
            tasks,
            hub: OnceLock::new(),
        }
    }

    /// 派发观察 hook 的唯一构造函数。
    ///
    /// 两条注入路径（chat 的一轮 looper、空间打开时的 machine 执行体）必须
    /// 得到同一个形状的 hook：各写一份会让 `origin_dir` / `principal` 在两条
    /// 路径上取值不同，台账从此对不上。hook 只持 `Weak<WorkspaceHub>`，
    /// 不会把已关闭的空间钉在内存里。
    pub fn dispatch_hook(
        &self,
        origin_ws_id: String,
        principal: String,
        origin_dir: PathBuf,
    ) -> Arc<DispatchWatchHook> {
        let hub = self
            .hub()
            .map_or_else(Weak::new, |hub| Arc::downgrade(&hub));
        Arc::new(DispatchWatchHook::new(
            hub,
            DispatchOrigin {
                ws_id: origin_ws_id,
                principal,
                origin_dir,
            },
            Arc::clone(&self.tasks) as Arc<dyn TaskObservationSink>,
        ))
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
        if self.peco.is_peco_space(ws_id) {
            config.dynamic_context = Some(Arc::new(SpaceCatalogContext::new(
                self.pool.clone(),
                self.peco.clone(),
                self.home.clone(),
            )));
        }
        config
    }

    /// 为一次聊天 run 组装配置：在 [`Self::looper_config`] 之上挂上下文窗口、
    /// 压缩、轮末收尾报告与记忆读写。
    ///
    /// 这些只给聊天 run：聊天会话跨轮累积，不折叠就会一路顶到上下文窗口；记忆
    /// 抽取与召回也只对「同一个空间里持续对话的 @master」有意义。machine 任务
    /// 每条都是新建的临时会话、跑完即弃，历史不跨轮增长，挂上只是白付成本。
    ///
    /// `memory` 是本轮空间自己的记忆管理器（打开空间时注入）：记忆库一个空间
    /// 一份，拿团队管理器会把记忆写进别人的库。为 `None` 时不挂记忆读写 ——
    /// 没有库可写，挂上去只会每轮白付一次失败。
    ///
    /// `max_output_tokens` 是本轮 agent 的 `llm.max_tokens`：请求级护栏用它从
    /// 模型窗口里扣掉输出预留，剩余部分才是消息体预算。
    ///
    /// `conversation_id` 只用于把压缩日志与边界观测挂到本轮会话上。
    // 装配点：空间、团队、目录、provider、输出预算、记忆、会话 id 各自正交，
    // 打包成结构体只会把同一份参数搬来搬去。
    #[allow(clippy::too_many_arguments)]
    pub fn chat_looper_config(
        &self,
        ws_id: &str,
        team_id: &str,
        work_dir: PathBuf,
        provider: Arc<dyn ModelProvider>,
        max_output_tokens: Option<u32>,
        memory: Option<Arc<KnowledgeManager>>,
        conversation_id: &str,
    ) -> LooperConfig {
        let mut config = self.looper_config(ws_id, team_id, work_dir);
        // 钳制必须每次装配都跑：它决定下方 policy 与 filter 的入参，缓存一次会让
        // 「改 env 后重启」与「运行期生效」两套口径并存。
        let mut limits = compaction::limits();
        let violations = limits.validate();
        if !violations.is_empty() && compaction::claim_config_log() {
            for v in violations {
                if v.is_error {
                    tracing::error!(
                        invariant = v.invariant,
                        detail = %v.detail,
                        "Compaction config invariant violated; clamped"
                    );
                } else {
                    tracing::warn!(
                        invariant = v.invariant,
                        detail = %v.detail,
                        "Compaction config invariant at risk"
                    );
                }
            }
        }
        // 历史窗口是会话语义：没有它，压缩负责把历史折叠掉，而模型仍会收到全部
        // 未折叠轮 —— 折叠阈值之下的增长没有任何裁剪点。
        //
        // 请求预算再给当前轮加一道护栏：压缩只在轮次边界运行，单轮内工具输出
        // 暴涨仍会推过模型窗口。预算 = 窗口 − 输出预算 − 系统提示/动态上下文余量，
        // 超限时过滤器从旧到新削减工具输出，而不是等 provider 返回 400。
        config.message_filter = Some(Arc::new(
            PecoContextFilter::new(limits.history_token_budget)
                .with_request_budget(limits.request_message_budget(max_output_tokens)),
        ));
        // 摘要器与收尾报告器共用同一个实例：两者都是「读一遍既有内容、产出短文本」
        // 的元任务，复用同一个 provider 与模型即可，另建一份只会多一份配置漂移。
        let summarizer: Arc<dyn TurnSummarizer> = Arc::new(ModelSummarizer::new(
            Arc::clone(&provider),
            limits.summarizer_model.clone(),
        ));
        config.epilogue = Some(Arc::clone(&summarizer));
        config.compaction = Some(Arc::new(limits.to_policy(summarizer)));

        // 折叠写日志、每个边界写观测行 —— 两张表口径不同，都要挂。观测钩子的
        // gate 与削减同源：只评估（shadow）时也落行，否则观测数据永远为零。
        config.hooks.push(Arc::new(CompactionMetricsHook::new(
            self.pool.clone(),
            LOCAL_USER_ID,
            conversation_id,
        )));
        config.hooks.push(Arc::new(ContextShadowHook::new(
            self.pool.clone(),
            LOCAL_USER_ID,
            conversation_id,
            limits.history_token_budget,
            limits.strip.shadow || limits.strip.enforce,
            limits.strip.tool_output_max_chars,
        )));

        let Some(memory_km) = memory else {
            tracing::warn!(
                ws_id,
                "space has no memory manager; memory read/write is off for this turn"
            );
            return config;
        };
        let memory = MemoryConfig::from_env();
        // push 而不是替换：`from_env()` 的 hook 链与本轮的派发观察 hook 都要保留，
        // 记忆提取只是又一位观察者。
        config.hooks.push(Arc::new(MemoryExtractionHook::new(
            Arc::clone(&memory_km),
            Arc::new(ModelTurnAnalyzer::new(provider, memory.model.clone())),
            memory.clone(),
            ws_id,
        )));
        config.dynamic_context =
            Some(self.memory_context(ws_id, memory_km, memory, config.dynamic_context.take()));
        config
    }

    /// 记忆召回与「既有动态上下文」的组合。
    ///
    /// Peco 空间已有空间清单，两段都得进同一次注入（core 只认一个
    /// `dynamic_context`）；其它空间只有记忆召回，多包一层组合结构没有意义。
    fn memory_context(
        &self,
        ws_id: &str,
        km: Arc<KnowledgeManager>,
        memory: MemoryConfig,
        existing: Option<Arc<dyn DynamicContext>>,
    ) -> Arc<dyn DynamicContext> {
        let recall: Arc<dyn DynamicContext> =
            Arc::new(MemoryRecallContext::new(km, memory, ws_id.to_string()));
        match existing {
            Some(existing) => Arc::new(ComposedDynamicContext::new(vec![existing, recall])),
            None => recall,
        }
    }

    /// 注入进程内唯一的实例表。装配顺序被破坏（重复注入）只 warn：
    /// 先到的那一份才是真正在用的 Hub，第二次注入不会让它变成另一个。
    pub fn attach_hub(&self, hub: &Arc<WorkspaceHub>) {
        if self.hub.set(Arc::downgrade(hub)).is_err() {
            tracing::warn!("space env already has a hub; keeping the first one");
        }
    }

    /// 已打开实例表；未注入时为 `None`。
    pub fn hub(&self) -> Option<Arc<WorkspaceHub>> {
        self.hub.get().and_then(Weak::upgrade)
    }

    /// 系统层 provider 文件的目录（最低优先层）。
    pub fn system_providers_root(&self) -> &Path {
        &self.providers_root
    }

    pub fn team_root(&self, team_id: &str) -> PathBuf {
        self.home.join("teams").join(team_id)
    }

    pub fn team_exists(&self, team_id: &str) -> bool {
        self.team_root(team_id).is_dir()
    }

    /// 已安装的团队（升序），用于 `team_missing` 的可选动作列表。
    pub fn available_teams(&self) -> Vec<String> {
        let mut teams: Vec<String> = std::fs::read_dir(self.home.join("teams"))
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.path().is_dir())
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        teams.sort();
        teams
    }
}

/// 改绑空间的团队：marker 是身份真源，先写它，DB 只是索引。
///
/// 调用方必须先确认目标团队的能力根存在 —— 这里不做校验，避免「校验一次、
/// 写入一次」之间目录被删掉时静默改成悬空绑定。
pub async fn rebind_team(
    env: &SpaceEnv,
    row: &WorkspaceRow,
    team_id: &str,
) -> Result<(), DaemonError> {
    let work_dir = PathBuf::from(&row.work_dir);
    if let Some(mut existing) = manifest::read(&work_dir)? {
        existing.team = env.team_root(team_id).to_string_lossy().into_owned();
        manifest::write(&work_dir, &existing)?;
    }
    workspaces::set_team(&env.pool, &row.id, team_id).await?;
    Ok(())
}
