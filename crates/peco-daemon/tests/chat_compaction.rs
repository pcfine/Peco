//! 聊天 run 与 machine 任务的 looper 配置分叉：压缩只挂聊天。
//!
//! machine 任务每条都是新建的临时会话、跑完即弃；把折叠策略挂上去既没有可压的
//! 历史，又多一次每轮的扫描与潜在的摘要调用。这个文件钉住「两条路径不一样」，
//! 并把聊天路径的压缩落到真库上跑一遍边界。

mod support;

use std::sync::Arc;
use std::time::Duration;

use model_provider::{
    GenerateRequest, GenerateResult, GenerateStream, ModelProvider, ProviderError,
};
use peco_core::agent::{
    AgentError, LooperHook, MessageFilter, PecoContextFilter, TurnSummarizer, estimate_item_tokens,
    is_viewable_item,
};
use peco_core::knowledge::KnowledgeManager;
use peco_core::session::{AnnotatedMessage, MessageSource, Session};
use peco_daemon::LOCAL_USER_ID;
use peco_daemon::db::{compaction_log, context_shadow};
use peco_daemon::space::bootstrap::bootstrap_peco;
use peco_daemon::space::compaction::{CompactionLimits, CompactionStripConfig};
use peco_daemon::space::compaction_hooks::{CompactionMetricsHook, ContextShadowHook};
use support::new_env;

/// 装配期不会调模型（摘要在真有轮次被驱逐时才发生），因此桩件只有失败路径。
struct UnreachableProvider;

#[async_trait::async_trait]
impl ModelProvider for UnreachableProvider {
    fn name(&self) -> &str {
        "unreachable"
    }

    async fn generate_full(
        &self,
        _request: &GenerateRequest,
    ) -> Result<GenerateResult, ProviderError> {
        Err(ProviderError::Request("not called during assembly".into()))
    }

    async fn generate_stream(
        &self,
        _request: &GenerateRequest,
    ) -> Result<GenerateStream, ProviderError> {
        Err(ProviderError::Request("not called during assembly".into()))
    }
}

/// 固定输出摘要器：边界用例要断言「摘要落了库」，不能靠真模型。
struct FixedSummarizer;

#[async_trait::async_trait]
impl TurnSummarizer for FixedSummarizer {
    async fn summarize(
        &self,
        _previous: Option<&str>,
        _evicted: &str,
    ) -> Result<String, AgentError> {
        Ok("## 已做决定\n- 固定摘要".to_string())
    }

    async fn summarize_inflight(&self, _inflight: &str) -> Result<String, AgentError> {
        Ok("## 本轮已完成".to_string())
    }
}

/// 聊天配置必须同时挂上下文窗口、压缩、轮末收尾报告与记忆读写，且不能顶掉观察 hook。
///
/// 变异方法：去掉 `chat_looper_config` 里的 `config.message_filter = ...` →
/// 窗口断言变红；去掉 `config.compaction = ...` → 压缩断言变红；把
/// `chat/run.rs` 改回 `looper_config` → 两者同时变红；去掉任一观测 hook →
/// hook 计数断言变红。
#[tokio::test]
async fn chat_runs_get_compaction_and_machine_tasks_do_not() {
    let env = new_env(&[]).await;
    bootstrap_peco(&env.env).await;
    let ws_id = env.env.peco.ws_id.clone();
    let work_dir = env.work_dir("peco");
    let provider: Arc<dyn ModelProvider> = Arc::new(UnreachableProvider);
    // 装配期不碰记忆库（提取在轮末才发生），路径给一个临时目录即可。
    let kb_dir = tempfile::tempdir().unwrap();
    let km = Arc::new(KnowledgeManager::new(kb_dir.path().to_path_buf()));

    let chat = env.env.chat_looper_config(
        &ws_id,
        "peco",
        work_dir.clone(),
        provider.clone(),
        Arc::clone(&km),
        "conv-chat-1",
    );
    assert!(chat.compaction.is_some(), "聊天 run 必须挂压缩");
    assert!(
        chat.message_filter.is_some(),
        "聊天 run 必须挂上下文窗口：折叠阈值之下的历史同样要有裁剪点"
    );
    assert!(
        chat.epilogue.is_some(),
        "撞上 max_iterations 的收尾报告与压缩同属聊天路径"
    );
    assert!(!chat.hooks.is_empty(), "压缩不能顶掉派发观察 hook");
    assert!(
        chat.dynamic_context.is_some(),
        "Peco 空间的动态清单在聊天配置里同样要生效"
    );

    let machine = env.env.looper_config(&ws_id, "peco", work_dir.clone());
    assert!(
        machine.compaction.is_none(),
        "machine 任务没有跨轮累积的历史，不该挂压缩"
    );
    assert!(
        machine.message_filter.is_none(),
        "machine 任务的会话不跨轮增长，窗口只会白抄一份消息"
    );
    assert!(machine.epilogue.is_none());
    assert!(!machine.hooks.is_empty(), "machine 路径仍要观察派发");
    assert_eq!(
        chat.hooks.len(),
        machine.hooks.len() + 3,
        "聊天路径比 machine 多且只多记忆提取与两个压缩观测 hook"
    );

    // 非 Peco 空间：没有空间清单，但记忆召回与压缩照挂。
    let plain =
        env.env
            .chat_looper_config("ws_plain", "peco", work_dir, provider, km, "conv-plain-1");
    assert!(
        plain.dynamic_context.is_some(),
        "记忆召回对每个空间的 @master 都生效"
    );
    assert!(
        plain.compaction.is_some() && plain.message_filter.is_some(),
        "压缩与窗口对每个 workspace 的聊天会话都生效"
    );
}

/// 一轮含 1000 个汉字的对话文本 ≈ 600 token（校准估算 0.6/字）。
fn turn_text(turn: usize) -> String {
    format!("第{turn}轮{}", "问".repeat(996))
}

fn message_tokens(text: &str) -> usize {
    estimate_item_tokens(&model_provider::InputItem::Message {
        role: model_provider::Role::User,
        content: text.into(),
    })
}

/// 折叠端到端：真 SQLite + 固定摘要器，断言折叠、日志与边界观测三方都落到位。
///
/// 预算刻意取小（≈ 每轮 1200 token）以便真触发驱逐；`strip` 取默认的只观测
/// 不改写 —— 观测行因此必须同时带评估值与零生效值。
#[tokio::test]
async fn boundary_fold_persists_summary_and_both_observation_rows() {
    let env = new_env(&[]).await;
    let conversation_id = "conv-fold-e2e";

    let limits = CompactionLimits {
        trigger_tokens: 23_000,
        keep_recent_tokens: 4_800,
        summarizer_model: "fixed".to_string(),
        history_token_budget: 1_200,
        strip: CompactionStripConfig::default(),
    };
    let policy = limits.to_policy(Arc::new(FixedSummarizer));

    // 21 轮 × 1200 token = 25_200，越过 23_000 的触发阈值；保留区 4_800 只装得下
    // 最近 4 轮 ⇒ 其余 17 轮必须被驱逐。
    let mut session = Session::new(conversation_id.to_string(), "fold".to_string());
    for turn in 0..21 {
        let text = turn_text(turn);
        assert_eq!(message_tokens(&text), 600, "夹具 token 必须可手算");
        session.start_turn(text.clone().into()).unwrap();
        session
            .stage_item(
                MessageSource::ModelGeneration,
                model_provider::InputItem::Message {
                    role: model_provider::Role::Assistant,
                    content: text.into(),
                },
            )
            .unwrap();
        session.commit_turn().unwrap();
    }

    let reduction = policy.apply_boundary(&mut session).await.unwrap();

    // ── 折叠 ──────────────────────────────────────────────────────────
    assert!(reduction.folded(), "越过阈值必须折叠: {reduction:?}");
    let outcome = reduction.compaction.as_ref().unwrap();
    assert_eq!(outcome.evicted_turns, 17);
    assert_eq!(session.committed_turns().len(), 4);
    let pinned = session
        .pinned_summary()
        .expect("折叠后必须留下 pinned 摘要");
    assert_eq!(
        pinned.source,
        MessageSource::SystemInjection {
            reason: "compaction".to_string()
        }
    );

    // ── 观测：钩子按 looper 的调用序走一遍 ────────────────────────────
    CompactionMetricsHook::new(env.pool.clone(), LOCAL_USER_ID, conversation_id)
        .on_context_compacted(outcome)
        .await;
    ContextShadowHook::new(
        env.pool.clone(),
        LOCAL_USER_ID,
        conversation_id,
        limits.history_token_budget,
        limits.strip.shadow || limits.strip.enforce,
        limits.strip.tool_output_max_chars,
    )
    .on_boundary_reduction(&session, &reduction)
    .await;

    let folds = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows =
                compaction_log::list_by_conversation(&env.pool, LOCAL_USER_ID, conversation_id)
                    .await
                    .unwrap();
            if !rows.is_empty() {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("折叠日志必须在后台落库");
    assert_eq!(folds.len(), 1);
    assert_eq!(folds[0].evicted_turns, 17);
    assert_eq!(
        folds[0].tokens_before,
        outcome.estimated_tokens_before as i64
    );
    assert_eq!(folds[0].tokens_after, outcome.estimated_tokens_after as i64);
    assert_eq!(
        folds[0].summary_chars,
        outcome.summary.chars().count() as i64
    );

    let shadows = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = context_shadow::list_by_user(&env.pool, LOCAL_USER_ID)
                .await
                .unwrap();
            if !rows.is_empty() {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("边界观测行必须在后台落库");
    assert_eq!(shadows.len(), 1);
    let row = &shadows[0];
    assert_eq!(row.conversation_id, conversation_id);
    assert_eq!(row.fold_triggered, 1);
    assert_eq!(row.evicted_turns, Some(17));
    assert_eq!(row.trigger_caliber, "full");
    assert_eq!(row.strip_evaluated, 1, "shadow 只观测也要有评估值");
    assert_eq!(row.strip_enforced, 0, "未开启 enforce 不得改写历史");
    // 钩子的窗口口径必须与 filter 同算法：4 轮各 1200 view token、预算 1200
    assert_eq!(row.view_window_tokens, 1_200);

    // ── 过滤器与压缩协同：pinned 摘要置顶，且历史总量不超预算 ──────────
    let filter = PecoContextFilter::new(limits.history_token_budget);
    let refs: Vec<&AnnotatedMessage> = session.all_message_refs().collect();
    let filtered = filter.filter(&refs);
    assert!(
        matches!(filtered[0].source, MessageSource::SystemInjection { .. }),
        "pinned 摘要必须排在第一条"
    );
    let current_turn = refs.last().unwrap().turn_index;
    let mut kept_history: Vec<usize> = filtered
        .iter()
        .filter(|am| !matches!(am.source, MessageSource::SystemInjection { .. }))
        .filter(|am| am.turn_index < current_turn)
        .map(|am| am.turn_index)
        .collect();
    // 一轮两条消息（用户 + 助手），断言取「保留了哪几轮」
    kept_history.dedup();
    let history_view: usize = filtered
        .iter()
        .filter(|am| !matches!(am.source, MessageSource::SystemInjection { .. }))
        .filter(|am| am.turn_index < current_turn)
        .filter(|am| is_viewable_item(&am.message))
        .map(|am| estimate_item_tokens(&am.message))
        .sum();
    assert!(
        history_view <= limits.history_token_budget,
        "历史窗口总量 {history_view} 超出预算 {}",
        limits.history_token_budget
    );
    assert_eq!(
        kept_history,
        vec![current_turn - 1],
        "预算 1200 只装得下最近一轮历史，更早的历史轮整轮裁掉"
    );
}
