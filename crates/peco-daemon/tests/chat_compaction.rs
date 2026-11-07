//! 聊天 run 与 machine 任务的 looper 配置分叉：压缩只挂聊天。
//!
//! machine 任务每条都是新建的临时会话、跑完即弃；把折叠策略挂上去既没有可压的
//! 历史，又多一次每轮的扫描与潜在的摘要调用。这个文件钉住「两条路径不一样」。

mod support;

use std::sync::Arc;

use model_provider::{
    GenerateRequest, GenerateResult, GenerateStream, ModelProvider, ProviderError,
};
use peco_daemon::space::bootstrap::bootstrap_peco;
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

/// 聊天配置必须同时挂压缩与轮末收尾报告，且不能顶掉观察 hook。
///
/// 变异方法：去掉 `chat_looper_config` 里的 `config.compaction = ...` →
/// 压缩断言变红；把 `chat/run.rs` 改回 `looper_config` → 同上。
#[tokio::test]
async fn chat_runs_get_compaction_and_machine_tasks_do_not() {
    let env = new_env(&[]).await;
    bootstrap_peco(&env.env).await;
    let ws_id = env.env.peco.ws_id.clone();
    let work_dir = env.work_dir("peco");
    let provider: Arc<dyn ModelProvider> = Arc::new(UnreachableProvider);

    let chat = env
        .env
        .chat_looper_config(&ws_id, "peco", work_dir.clone(), provider);
    assert!(chat.compaction.is_some(), "聊天 run 必须挂压缩");
    assert!(
        chat.epilogue.is_some(),
        "撞上 max_iterations 的收尾报告与压缩同属聊天路径"
    );
    assert!(!chat.hooks.is_empty(), "压缩不能顶掉派发观察 hook");
    assert!(
        chat.dynamic_context.is_some(),
        "Peco 空间的动态清单在聊天配置里同样要生效"
    );

    let machine = env.env.looper_config(&ws_id, "peco", work_dir);
    assert!(
        machine.compaction.is_none(),
        "machine 任务没有跨轮累积的历史，不该挂压缩"
    );
    assert!(machine.epilogue.is_none());
    assert!(!machine.hooks.is_empty(), "machine 路径仍要观察派发");
}
