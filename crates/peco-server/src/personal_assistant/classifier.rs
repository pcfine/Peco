// ============================================================================
// QueryClassifier — 零 LLM 成本的查询分类规则引擎
// ============================================================================
//
// 通过关键词匹配将用户 query 分为四类，决定检索策略：
//   - PersonalQuery  → 检索所有层
//   - TechnicalQuery → 检索 semantic + episodic
//   - CasualChat     → 仅注入 profile
//   - GeneralQuery   → 检索 semantic（默认）

use super::types::QueryType;

/// 查询分类器。
///
/// # 设计原则
///
/// - 零 LLM 成本：纯关键词正则匹配
/// - 从宽匹配：默认归类为 `GeneralQuery`，避免漏检索
/// - 降级策略：仅对明确的闲聊词降级为 `CasualChat`
pub struct QueryClassifier;

impl QueryClassifier {
    /// 创建新的 QueryClassifier。
    pub fn new() -> Self {
        Self
    }

    /// 对用户 query 进行分类。
    pub fn classify(&self, query: &str) -> QueryType {
        let query_lower = query.to_lowercase();

        // 1. 明确的闲聊 → CasualChat（不做语义检索，只注入 profile）
        if self.is_casual_chat(&query_lower) {
            return QueryType::CasualChat;
        }

        // 2. 个人相关 → PersonalQuery（检索所有层）
        if self.is_personal_query(&query_lower) {
            return QueryType::PersonalQuery;
        }

        // 3. 技术相关 → TechnicalQuery（检索 semantic + episodic）
        if self.is_technical_query(&query_lower) {
            return QueryType::TechnicalQuery;
        }

        // 4. 默认 → GeneralQuery（检索 semantic）
        QueryType::GeneralQuery
    }

    /// 检测是否为闲聊。
    fn is_casual_chat(&self, query: &str) -> bool {
        let casual_patterns = [
            // 中文问候/告别
            "你好", "您好", "嗨", "哈喽", "哈啰",
            "谢谢", "感谢", "多谢",
            "再见", "拜拜", "晚安", "早安",
            "嗯", "哦", "好的", "ok", "okay",
            // 英文简短问候
            "hello", "hi ", "hey", "thanks", "thank you", "bye",
            "good morning", "good night",
            // 无实质内容
            "测试", "test",
        ];

        // 极短消息也可能为闲聊
        if query.chars().count() <= 3 {
            return true;
        }

        casual_patterns.iter().any(|p| query.contains(p))
    }

    /// 检测是否为个人相关查询。
    fn is_personal_query(&self, query: &str) -> bool {
        let personal_keywords = [
            // 强个人信号
            "我叫", "我是",
            "我的偏好", "我的习惯", "我的风格", "我喜欢", "我不喜欢",
            "我之前", "我上次", "我以前", "我曾经",
            "记得吗", "还记得", "记住我", "别忘了", "忘记我",
            "告诉我关于我",
        ];

        personal_keywords.iter().any(|k| query.contains(k))
    }

    /// 检测是否为技术查询。
    fn is_technical_query(&self, query: &str) -> bool {
        let tech_keywords = [
            // 编程语言
            "rust", "python", "javascript", "typescript", "golang", "go",
            "java", "c++", "cpp", "c#", "csharp",
            // 框架
            "axum", "actix", "tokio", "react", "vue", "angular", "svelte",
            "next.js", "nuxt", "express", "fastify", "django", "flask",
            // 数据库
            "sql", "sqlite", "postgres", "mysql", "mongodb", "redis",
            "lancedb", "qdrant", "向量", "数据库",
            // 工具
            "docker", "k8s", "kubernetes", "git", "ci/cd", "github",
            "vscode", "ide", "cli",
            // 概念
            "api", "rest", "grpc", "graphql", "websocket",
            "函数", "方法", "类", "接口", "类型",
            "代码", "bug", "错误", "报错", "错误处理", "exception",
            "性能", "优化", "优化建议",
            "架构", "设计模式", "重构",
            "部署", "部署流程", "deploy",
            "配置", "config", "配置文件",
            "测试", "单元测试", "集成测试",
            "async", "await", "异步", "并发",
            "算法", "数据结构",
            "linux", "macos", "windows",
            "web server", "服务器",
            "ai", "llm", "agent", "rag",
            "怎么写", "实现一个",
            "compile", "编译",
        ];

        tech_keywords.iter().any(|k| query.contains(k))
    }
}

impl Default for QueryClassifier {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_casual_chat_greeting() {
        let c = QueryClassifier::new();
        assert_eq!(c.classify("你好"), QueryType::CasualChat);
        assert_eq!(c.classify("谢谢"), QueryType::CasualChat);
        assert_eq!(c.classify("嗯"), QueryType::CasualChat);
    }

    #[test]
    fn test_casual_chat_short() {
        let c = QueryClassifier::new();
        assert_eq!(c.classify("Hi"), QueryType::CasualChat);
    }

    #[test]
    fn test_personal_query() {
        let c = QueryClassifier::new();
        assert_eq!(c.classify("我之前问过什么"), QueryType::PersonalQuery);
        assert_eq!(c.classify("你还记得我的偏好吗"), QueryType::PersonalQuery);
        assert_eq!(c.classify("我叫张三"), QueryType::PersonalQuery);
    }

    #[test]
    fn test_technical_query() {
        let c = QueryClassifier::new();
        assert_eq!(
            c.classify("帮我用 Axum 写一个 web server"),
            QueryType::TechnicalQuery
        );
        assert_eq!(
            c.classify("Rust 的 async 怎么用"),
            QueryType::TechnicalQuery
        );
    }

    #[test]
    fn test_general_query_default() {
        let c = QueryClassifier::new();
        // 非个人、非技术、非闲聊 → 默认 GeneralQuery
        assert_eq!(
            c.classify("今天天气怎么样"),
            QueryType::GeneralQuery
        );
        assert_eq!(
            c.classify("介绍一下 peco 项目"),
            QueryType::GeneralQuery
        );
    }

    #[test]
    fn test_personal_takes_priority_over_tech() {
        let c = QueryClassifier::new();
        // 包含"我"优先归类为 PersonQuery（检测顺序：Casual → Personal → Tech → General）
        assert_eq!(
            c.classify("我之前写的 Rust 代码怎么报错"),
            QueryType::PersonalQuery
        );
    }
}
