//! Layer 1 — 查询理解与意图分类。
//!
//! 在启动任何检索路径之前，先分析原始查询文本以确定：
//! * 意图类别（关键词查找、概念检索、关系查询等）
//! * 长度类别（影响 BM25 与向量的相对有效性）
//! * 语言特征（中文 vs 英文，影响分词行为）
//!
//! 分析结果用于调整每条检索路径的权重
//!（例如，短关键词提升全文权重；概念查询提升向量权重）。

use crate::types::SearchStrategy;

// ---------------------------------------------------------------------------
// QueryIntent
// ---------------------------------------------------------------------------

/// 查询意图的粗粒度分类。
///
/// 每条检索路径对不同意图具有不同的优势：
/// * **FactLookup** — BM25 擅长精确名称/ID/日期匹配。
/// * **Conceptual** — 向量嵌入擅长语义相似性。
/// * **Relational** — 图遍历擅长多跳关系。
/// * **ShortKeyword** — 超短查询（1–3 字符），需要激进的全文本提升。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryIntent {
    /// 事实查找：精确名称、ID、日期、代码片段。
    /// 例如 "彭琛"、"error E0425"、"2024-01-15"
    FactLookup,
    /// 概念查询：主题探索、释义、思想。
    /// 例如 "分布式系统设计原则"、"how to improve performance"
    Conceptual,
    /// 关系查询：实体之间如何关联。
    /// 例如 "what depends on module X"、"related documents"
    Relational,
    /// 探索性：宽泛的开放式问题。
    /// 例如 "tell me about Rust"、"what's in this knowledge base"
    Exploratory,
    /// 超短关键词：1–3 个字符，通常是中文。
    /// 例如 "苹果"、"AI"、"Rust"
    ShortKeyword,
}

// ---------------------------------------------------------------------------
// QueryLength
// ---------------------------------------------------------------------------

/// 按字符数划分的查询长度分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryLength {
    /// 1–3 个字符 — 太少，无法进行有意义的向量比较。
    UltraShort,
    /// 4–10 个字符 — 向量与文本均可能有效。
    Short,
    /// > 10 个字符 — 向量具有足够的语义信号。
    Normal,
}

// ---------------------------------------------------------------------------
// QueryAnalysis
// ---------------------------------------------------------------------------

/// 查询的完整分析结果，由 [`QueryAnalyzer`] 生成。
#[derive(Debug, Clone)]
pub struct QueryAnalysis {
    pub intent: QueryIntent,
    pub length_class: QueryLength,
    /// 查询是否主要包含 CJK 字符。
    pub is_chinese: bool,
    /// 非停用词唯一词项的数量（近似）。
    pub unique_terms: usize,
    /// 查询是否可能包含特定实体名称（专有名词、ID）。
    pub has_exact_entity: bool,
}

// ---------------------------------------------------------------------------
// QueryAnalyzer trait
// ---------------------------------------------------------------------------

/// 查询分析器 trait — 将原始查询文本映射为 [`QueryAnalysis`]。
///
/// 实现必须是线程安全的（`Send + Sync`），因为它们存储在
/// `HybridSearchEngine` 中，该引擎可能跨 tokio 任务共享。
pub trait QueryAnalyzer: Send + Sync {
    fn analyze(&self, query: &str) -> QueryAnalysis;
}

// ---------------------------------------------------------------------------
// RuleBasedAnalyzer
// ---------------------------------------------------------------------------

/// 基于规则的查询分析器 — 零依赖、确定性的意图分类。
///
/// 使用关键词模式、字符类分布和长度启发式方法
/// 以最小开销对查询进行分类。
pub struct RuleBasedAnalyzer;

impl RuleBasedAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RuleBasedAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryAnalyzer for RuleBasedAnalyzer {
    fn analyze(&self, query: &str) -> QueryAnalysis {
        let trimmed = query.trim();
        let char_count = trimmed.chars().count();

        // 语言检测：如果 ≥50% 的字母字符是 CJK，则判定为中文。
        let cjk_count = trimmed.chars().filter(|c| is_cjk(*c)).count();
        let alpha_count = trimmed.chars().filter(|c| c.is_alphabetic()).count();
        let is_chinese = alpha_count > 0 && cjk_count as f32 / alpha_count as f32 >= 0.5
            || cjk_count > 0 && alpha_count == 0;

        // 长度分类。
        let length_class = if char_count <= 3 {
            QueryLength::UltraShort
        } else if char_count <= 10 {
            QueryLength::Short
        } else {
            QueryLength::Normal
        };

        // 近似词项数量。
        let unique_terms = estimate_term_count(trimmed, is_chinese);

        // 实体检测：专有名词、ID、日期模式。
        let has_exact_entity = detect_entity(trimmed);

        // 意图分类。
        let intent = classify_intent(trimmed, is_chinese, length_class, has_exact_entity);

        QueryAnalysis {
            intent,
            length_class,
            is_chinese,
            unique_terms,
            has_exact_entity,
        }
    }
}

// ---------------------------------------------------------------------------
// query_adjusted_weights
// ---------------------------------------------------------------------------

/// 根据查询分析调整每条检索路径的权重。
///
/// 该纯函数将查询特征映射为按路径的权重修正因子。
/// 返回 `(vector_weight, text_weight, graph_weight)` 元组，
/// 调用方将其与基础策略权重相乘。
///
/// # 调整规则
///
/// | 特征 | 向量 | 全文本 | 图谱 | 理由 |
/// |---|---|---|---|---|
/// | UltraShort + 中文 | **0.3×** | **2.0×** | 1.0× | 太短，无法进行向量嵌入 |
/// | ShortKeyword | **0.5×** | **1.5×** | 1.0× | BM25 处理简短精确匹配效果更好 |
/// | FactLookup | 0.8× | **1.3×** | 1.0× | BM25 处理名称/ID 匹配效果更好 |
/// | Conceptual | **1.3×** | 0.8× | 1.0× | 向量处理释义效果更好 |
/// | Relational | 1.0× | 1.0× | **1.5×** | 图谱处理多跳效果更好 |
pub fn query_adjusted_weights(
    analysis: &QueryAnalysis,
    base_strategy: &SearchStrategy,
) -> (f32, f32, f32) {
    // 从策略中提取基础权重。
    let (base_vec, base_txt, base_grph) = match base_strategy {
        SearchStrategy::VectorOnly => (1.0, 0.0, 0.0),
        SearchStrategy::TextOnly => (0.0, 1.0, 0.0),
        SearchStrategy::Hybrid {
            vector_weight,
            text_weight,
        } => (*vector_weight, *text_weight, 0.0),
        SearchStrategy::FullHybrid {
            vector_weight,
            text_weight,
            graph_weight,
            ..
        } => (*vector_weight, *text_weight, *graph_weight),
        SearchStrategy::GraphOnly { .. } => (0.0, 0.0, 1.0),
        SearchStrategy::Auto => (0.4, 0.4, 0.2), // 默认 FullHybrid
    };

    let (mut vm, mut tm, mut gm) = (1.0f32, 1.0f32, 1.0f32);

    // 长度修正。
    match analysis.length_class {
        QueryLength::UltraShort => {
            vm *= 0.3;
            tm *= 2.0;
        }
        QueryLength::Short => {
            vm *= 0.7;
            tm *= 1.3;
        }
        QueryLength::Normal => {
            // 正常长度 — 不调整。
        }
    }

    // 意图修正（在长度修正基础上叠加）。
    match analysis.intent {
        QueryIntent::ShortKeyword => {
            vm *= 0.5;
            tm *= 1.5;
        }
        QueryIntent::FactLookup => {
            vm *= 0.8;
            tm *= 1.3;
        }
        QueryIntent::Conceptual => {
            vm *= 1.3;
            tm *= 0.8;
        }
        QueryIntent::Relational => {
            gm *= 1.5;
        }
        QueryIntent::Exploratory => {
            // 探索性 — 保持平衡，略微提升召回率。
            vm *= 1.1;
            tm *= 1.1;
        }
    }

    // 中文实体进一步提升文本权重（BM25 unigram 精确匹配）。
    if analysis.is_chinese && analysis.has_exact_entity {
        tm *= 1.2;
    }

    let adj_vec = (base_vec * vm * 100.0).round() / 100.0;
    let adj_txt = (base_txt * tm * 100.0).round() / 100.0;
    let adj_grph = (base_grph * gm * 100.0).round() / 100.0;

    (adj_vec, adj_txt, adj_grph)
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 检测 CJK 统一表意文字。
fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{4E00}'..='\u{9FFF}'   // CJK 统一表意文字
        | '\u{3400}'..='\u{4DBF}' // CJK 扩展 A
        | '\u{F900}'..='\u{FAFF}' // CJK 兼容表意文字
        | '\u{2F800}'..='\u{2FA1F}' // CJK 兼容补充
    )
}

/// 粗略估计不同词项的数量。
fn estimate_term_count(text: &str, is_chinese: bool) -> usize {
    if is_chinese {
        // 中文：每个字符为一个词项。
        text.chars().filter(|c| is_cjk(*c)).count()
    } else {
        // 英文：按空白字符分割。
        text.split_whitespace().count()
    }
}

/// 检测查询是否包含可能的特定实体（ID、名称、日期）。
fn detect_entity(text: &str) -> bool {
    // 纯 CJK 短名称检测（2–4 个表意文字 → 可能是人名、地名）。
    let cjk_count = text.chars().filter(|c| is_cjk(*c)).count();
    let total_chars = text.chars().count();
    if cjk_count >= 2 && cjk_count <= 4 && cjk_count == total_chars {
        return true;
    }

    // 日期模式。
    if text.chars().filter(|c| c.is_ascii_digit()).count() >= 4 {
        return true;
    }

    // 大写首字母词（专有名词）。
    let words: Vec<&str> = text.split_whitespace().collect();
    let proper_nouns = words
        .iter()
        .filter(|w| {
            w.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                && w.len() > 1
                && w.chars().skip(1).all(|c| c.is_lowercase())
        })
        .count();

    if proper_nouns >= 1 && words.len() <= 5 {
        return true;
    }

    // 模式：字母数字 ID（例如 "E0425"、"err404"）。
    let has_id_pattern = text
        .chars()
        .collect::<Vec<char>>()
        .windows(3)
        .any(|w| w[0].is_alphabetic() && w[1].is_ascii_digit());

    if has_id_pattern {
        return true;
    }

    false
}

/// 基于规则的意图分类。
fn classify_intent(
    text: &str,
    is_chinese: bool,
    length: QueryLength,
    has_entity: bool,
) -> QueryIntent {
    let lower = text.to_lowercase();

    // 关系关键词。
    let relational_kw = [
        "related",
        "connected",
        "linked",
        "references",
        "cites",
        "depends on",
        "belongs to",
        "containing",
        "what documents",
        "关联",
        "相关",
        "依赖",
        "引用",
        "包含",
    ];
    if relational_kw.iter().any(|kw| lower.contains(kw)) {
        return QueryIntent::Relational;
    }

    // 超短关键词（在事实/概念之前 — 1-3 字符无足够语义信号）。
    if length == QueryLength::UltraShort {
        return QueryIntent::ShortKeyword;
    }

    // 事实查找关键词（在概念之前检查 — "what is X" 是事实查找）。
    let fact_kw = [
        "what is",
        "who is",
        "define",
        "definition",
        "error",
        "port",
        "version",
        "config",
        "default",
        "定义",
        "错误",
        "端口",
        "版本",
        "配置",
        "默认",
    ];
    if has_entity || fact_kw.iter().any(|kw| lower.contains(kw)) {
        return QueryIntent::FactLookup;
    }

    // 概念/解释关键词（how-to、why、深层解释）。
    let conceptual_kw = [
        "how to",
        "how do",
        "explain",
        "describe",
        "why",
        "best practice",
        "guide",
        "tutorial",
        "如何",
        "怎么",
        "什么是",
        "为什么",
        "原理",
        "方法",
        "教程",
    ];
    if conceptual_kw.iter().any(|kw| lower.contains(kw)) {
        return QueryIntent::Conceptual;
    }

    // 探索性关键词（开放式、宽泛的提问）。
    let exploratory_kw = [
        "tell me about",
        "tell me",
        "what can you",
        "overview",
        "summary",
        "introduction to",
        "介绍",
        "概述",
        "总结",
    ];
    if exploratory_kw.iter().any(|kw| lower.contains(kw)) {
        return QueryIntent::Exploratory;
    }

    // 短中文查询 → 更偏向关键词。
    if is_chinese && length == QueryLength::Short {
        return QueryIntent::ShortKeyword;
    }

    // 默认：无特定信号 → 根据长度判定。
    if length == QueryLength::Normal {
        QueryIntent::Exploratory
    } else {
        QueryIntent::Conceptual
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze(q: &str) -> QueryAnalysis {
        RuleBasedAnalyzer::new().analyze(q)
    }

    // ── 意图分类 ──

    #[test]
    fn detect_short_chinese_keyword() {
        let a = analyze("苹果");
        assert_eq!(a.intent, QueryIntent::ShortKeyword);
        assert_eq!(a.length_class, QueryLength::UltraShort);
        assert!(a.is_chinese);
    }

    #[test]
    fn detect_chinese_name_lookup() {
        let a = analyze("彭琛");
        assert_eq!(a.intent, QueryIntent::ShortKeyword);
        assert_eq!(a.length_class, QueryLength::UltraShort);
        assert!(a.is_chinese);
    }

    #[test]
    fn detect_chinese_fact_lookup() {
        let a = analyze("武汉大学");
        assert_eq!(a.length_class, QueryLength::Short);
        assert!(a.is_chinese);
        // 4 字符纯 CJK → 实体名称 → FactLookup（BM25 精确匹配优于向量语义）。
        assert_eq!(a.intent, QueryIntent::FactLookup);
        assert!(a.has_exact_entity);
    }

    #[test]
    fn detect_conceptual_query() {
        let a = analyze("how to improve database performance");
        assert_eq!(a.intent, QueryIntent::Conceptual);
        assert_eq!(a.length_class, QueryLength::Normal);
    }

    #[test]
    fn detect_relational_query() {
        let a = analyze("what documents are related to Rust");
        assert_eq!(a.intent, QueryIntent::Relational);
    }

    #[test]
    fn detect_english_entity() {
        let a = analyze("Rust programming language");
        assert!(a.has_exact_entity);
    }

    #[test]
    fn detect_english_fact_lookup() {
        let a = analyze("what is the default port");
        assert_eq!(a.intent, QueryIntent::FactLookup);
    }

    // ── 语言检测 ──

    #[test]
    fn english_is_not_chinese() {
        let a = analyze("Hello world");
        assert!(!a.is_chinese);
    }

    #[test]
    fn pure_chinese_is_chinese() {
        let a = analyze("分布式系统设计");
        assert!(a.is_chinese);
    }

    #[test]
    fn mixed_chinese_english_is_chinese() {
        let a = analyze("Rust 编程语言");
        assert!(a.is_chinese);
    }

    // ── 长度分类 ──

    #[test]
    fn one_char_is_ultrashort() {
        let a = analyze("a");
        assert_eq!(a.length_class, QueryLength::UltraShort);
    }

    #[test]
    fn three_chars_is_ultrashort() {
        let a = analyze("abc");
        assert_eq!(a.length_class, QueryLength::UltraShort);
    }

    #[test]
    fn five_chars_is_short() {
        let a = analyze("hello");
        assert_eq!(a.length_class, QueryLength::Short);
    }

    #[test]
    fn long_query_is_normal() {
        let a = analyze("this is a fairly long query for testing purposes");
        assert_eq!(a.length_class, QueryLength::Normal);
    }

    // ── 权重调整 ──

    #[test]
    fn short_keyword_boosts_text() {
        let a = analyze("苹果");
        let s = SearchStrategy::Hybrid {
            vector_weight: 0.5,
            text_weight: 0.5,
        };
        let (v, t, _g) = query_adjusted_weights(&a, &s);
        // UltraShort (vm=0.3, tm=2.0) + ShortKeyword (vm=0.5, tm=1.5)
        // vec: 0.5 * 0.3 * 0.5 = 0.075 → round 0.08
        // txt: 0.5 * 2.0 * 1.5 = 1.5 → round 1.5
        assert!(t > v, "文本权重应高于向量权重，实际 v={v} t={t}");
        assert!(v < 0.2, "超短关键词的向量权重应非常低");
    }

    #[test]
    fn conceptual_boosts_vector() {
        let a = analyze("how to design distributed systems");
        let s = SearchStrategy::Hybrid {
            vector_weight: 0.5,
            text_weight: 0.5,
        };
        let (v, t, _) = query_adjusted_weights(&a, &s);
        assert!(v > t, "概念查询应提升向量权重");
    }

    #[test]
    fn relational_boosts_graph() {
        let a = analyze("what documents are related to this");
        let s = SearchStrategy::FullHybrid {
            vector_weight: 0.4,
            text_weight: 0.4,
            graph_weight: 0.2,
            graph_expansion_depth: 1,
        };
        let (_, _, g) = query_adjusted_weights(&a, &s);
        // 基础 0.2 * 1.5 = 0.3
        assert!(g >= 0.25, "关系查询应提升图谱权重");
    }

    #[test]
    fn chinese_entity_boosts_text_further() {
        let a = analyze("彭琛"); // 中文 + 实体
        assert!(a.is_chinese);
        assert!(a.has_exact_entity);
        let s = SearchStrategy::Hybrid {
            vector_weight: 0.5,
            text_weight: 0.5,
        };
        let (v, t, _) = query_adjusted_weights(&a, &s);
        // has_exact_entity + is_chinese → tm *= 1.2 额外
        assert!(t > 1.0, "中文实体应显著提升文本权重");
        assert!(t > v * 3.0, "文本权重应远高于向量权重");
    }

    #[test]
    fn exploratory_is_balanced() {
        let a = analyze("tell me about machine learning");
        let s = SearchStrategy::Hybrid {
            vector_weight: 0.5,
            text_weight: 0.5,
        };
        let (v, t, _) = query_adjusted_weights(&a, &s);
        // 两者都应接近 0.5 * 1.1 = 0.55
        assert!((v - 0.55).abs() < 0.1);
        assert!((t - 0.55).abs() < 0.1);
    }

    #[test]
    fn empty_query_is_handled() {
        let a = analyze("");
        assert_eq!(a.length_class, QueryLength::UltraShort);
        assert!(!a.has_exact_entity);
    }
}
