use crate::types::SearchStrategy;

/// 当前后端提供的能力。
#[derive(Debug, Clone, Default)]
pub struct BackendCapabilities {
    pub has_vector: bool,
    pub has_fulltext: bool,
    pub has_graph: bool,
}

/// 查询路由器 — 将自然语言查询映射到推荐的搜索策略。
///
/// 第二阶段实现简单的基于规则的路由器。第四阶段可能升级为
/// 基于 LLM 的路由器。
pub trait QueryRouter: Send + Sync {
    fn route(&self, query: &str, capabilities: &BackendCapabilities) -> SearchStrategy;
}

// ---------------------------------------------------------------------------
// RuleBasedRouter
// ---------------------------------------------------------------------------

/// 基于规则的路由器，通过检测关键词来偏置策略选择。
pub struct RuleBasedRouter;

impl RuleBasedRouter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RuleBasedRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryRouter for RuleBasedRouter {
    fn route(&self, query: &str, capabilities: &BackendCapabilities) -> SearchStrategy {
        let lower = query.to_lowercase();

        // 检测关系/图谱查询。
        let graph_keywords = [
            "related",
            "connected",
            "linked",
            "references",
            "cites",
            "depends on",
            "belongs to",
            "containing",
            "what documents",
        ];
        let is_graph_query = graph_keywords.iter().any(|kw| lower.contains(kw));

        // 检测事实查找/精确匹配查询。
        let text_keywords = [
            "what is",
            "define",
            "definition of",
            "port is",
            "default is",
            "version",
            "error code",
            "who is",
        ];
        let is_text_query = text_keywords.iter().any(|kw| lower.contains(kw));

        match (
            capabilities.has_vector,
            capabilities.has_fulltext,
            capabilities.has_graph,
        ) {
            (_, _, _) if is_graph_query && capabilities.has_graph => SearchStrategy::FullHybrid {
                vector_weight: 0.2,
                text_weight: 0.2,
                graph_weight: 0.6,
                graph_expansion_depth: 2,
            },
            (_, _, _) if is_text_query && capabilities.has_fulltext => {
                if capabilities.has_vector {
                    SearchStrategy::Hybrid {
                        vector_weight: 0.3,
                        text_weight: 0.7,
                    }
                } else {
                    SearchStrategy::TextOnly
                }
            }
            (true, true, true) => SearchStrategy::FullHybrid {
                vector_weight: 0.4,
                text_weight: 0.4,
                graph_weight: 0.2,
                graph_expansion_depth: 1,
            },
            (true, true, false) => SearchStrategy::Hybrid {
                vector_weight: 0.5,
                text_weight: 0.5,
            },
            (true, false, _) => SearchStrategy::VectorOnly,
            (false, true, _) => SearchStrategy::TextOnly,
            (false, false, true) => SearchStrategy::GraphOnly {
                start_node_ids: Vec::new(),
            },
            (false, false, false) => {
                // 没有可用的后端 — 使用将优雅失败的默认策略。
                SearchStrategy::Hybrid {
                    vector_weight: 0.5,
                    text_weight: 0.5,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(has_vector: bool, has_fulltext: bool, has_graph: bool) -> BackendCapabilities {
        BackendCapabilities {
            has_vector,
            has_fulltext,
            has_graph,
        }
    }

    #[test]
    fn route_graph_query() {
        let router = RuleBasedRouter;
        let s = router.route(
            "what documents are related to Rust?",
            &caps(true, true, true),
        );
        matches!(
            s,
            SearchStrategy::FullHybrid { graph_weight, .. } if graph_weight > 0.5
        );
    }

    #[test]
    fn route_text_query() {
        let router = RuleBasedRouter;
        let s = router.route("what is the default port?", &caps(true, true, false));
        assert!(matches!(s, SearchStrategy::Hybrid { text_weight, .. } if text_weight > 0.5));
    }

    #[test]
    fn route_default_to_full_hybrid() {
        let router = RuleBasedRouter;
        let s = router.route(
            "how do I optimize database performance?",
            &caps(true, true, true),
        );
        assert!(matches!(s, SearchStrategy::FullHybrid { .. }));
    }

    #[test]
    fn route_degrades_when_no_graph() {
        let router = RuleBasedRouter;
        let s = router.route(
            "how do I optimize database performance?",
            &caps(true, true, false),
        );
        assert!(matches!(s, SearchStrategy::Hybrid { .. }));
    }

    #[test]
    fn route_vector_only_when_single_backend() {
        let router = RuleBasedRouter;
        let s = router.route("any query", &caps(true, false, false));
        assert!(matches!(s, SearchStrategy::VectorOnly));

        let s = router.route("any query", &caps(false, true, false));
        assert!(matches!(s, SearchStrategy::TextOnly));
    }
}
