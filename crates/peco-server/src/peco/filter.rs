// ============================================================================
// PecoContextFilter — 上下文组装器的宿主导出
// ============================================================================
//
// 实现下沉到 `peco_core::agent::filter`：两个宿主（peco-server / peco-daemon）
// 各写一份过滤器，会让「哪段历史进入模型上下文」这条规则随宿主漂移 ——
// 它是会话语义，不是宿主配置。

pub use peco_core::agent::PecoContextFilter;
pub(crate) use peco_core::agent::select_history_turns;
