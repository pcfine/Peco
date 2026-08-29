// ============================================================================
// 记忆双路径配置
// ============================================================================
//
// 记忆双路径的参数集中处。写路径（MemoryExtractionHook）与读路径
// （MemoryRecallContext）共享同一份 MemoryConfig，由 PecoManager 在
// 构造期装配进 PecoConfig.hooks / .dynamic_context。

/// 记忆双路径配置。
///
/// 存储载体是 workspace 内的 `@private_memory` 知识库（personal 模板
/// 幂等安装，per-user 目录隔离，LanceDb 后端）——本配置只描述行为参数，
/// 不描述存储后端。
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// 总开关。`false` 时 PecoManager 不装配任何记忆组件（零开销）。
    pub enabled: bool,
    /// 记忆知识库名（与 personal 模板保持一致）。
    pub kb_name: String,
    /// 提取模型（Flash 档，低延迟低成本；复用主 Agent 的 provider）。
    pub model: String,
    /// 本轮对话总字符数低于该值时不提取（寒暄过滤）。
    pub analyze_min_chars: usize,
    /// 提取前检索既有记忆的条数（进入 prompt 供模型判断"是否为新信息"）。
    pub extraction_top_k: usize,
    /// 读路径单次检索条数。
    pub recall_top_k: usize,
    /// 读路径注入的 token 上限（校准估算），超出整行丢弃。
    pub injection_token_cap: usize,
    /// 单次提取调用的超时（秒）。
    pub analyzer_timeout_secs: u64,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            kb_name: "@private_memory".to_string(),
            model: "deepseek-v4-flash".to_string(),
            analyze_min_chars: 50,
            extraction_top_k: 5,
            recall_top_k: 3,
            injection_token_cap: 800,
            analyzer_timeout_secs: 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let c = MemoryConfig::default();
        assert!(c.enabled);
        assert_eq!(c.kb_name, "@private_memory");
        assert_eq!(c.model, "deepseek-v4-flash");
        assert_eq!(c.analyze_min_chars, 50);
        assert_eq!(c.injection_token_cap, 800);
    }
}
