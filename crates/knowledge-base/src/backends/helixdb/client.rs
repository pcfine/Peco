//! HelixDB HTTP 客户端。
//!
//! 封装 `reqwest::Client`，通过 `POST /v1/query` 与 HelixDB 通信。
//! 适配自 `knowledge-helixdb` 的 `HelixDbClient`，错误类型映射到
//! `KnowledgeError`。

use serde_json::Value;
use tracing::{debug, info};

use crate::error::KnowledgeError;

/// HelixDB HTTP 客户端 — 与 `/v1/query` 端点通信。
///
/// `reqwest::Client` 内部已 `Clone + Send + Sync`，天然线程安全。
#[derive(Clone)]
pub(crate) struct HelixDbClient {
    /// 内部 reqwest HTTP 客户端。
    http: reqwest::Client,
    /// HelixDB 服务器基础 URL（例如 `http://localhost:6969`）。
    base_url: String,
}

impl HelixDbClient {
    /// 连接到 HelixDB 服务器。
    pub async fn connect(base_url: &str) -> Result<Self, KnowledgeError> {
        info!(%base_url, "正在连接 HelixDB");

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| KnowledgeError::Internal(format!("构建 HTTP 客户端失败: {e}")))?;

        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    /// 返回 HelixDB 服务器的基础 URL。
    #[allow(dead_code)]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 发送写查询并返回 JSON 响应。
    pub async fn execute_write(&self, query: Value) -> Result<Value, KnowledgeError> {
        debug!("执行写查询");
        self.send_query(query).await
    }

    /// 发送读查询并返回 JSON 响应。
    pub async fn execute_read(&self, query: Value) -> Result<Value, KnowledgeError> {
        debug!("执行读查询");
        self.send_query(query).await
    }

    /// 检查 HelixDB 服务器是否可达。
    #[allow(dead_code)]
    pub async fn health_check(&self) -> Result<bool, KnowledgeError> {
        let url = format!("{}/health", self.base_url);
        match self.http.get(&url).send().await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    // ── 内部方法 ──────────────────────────────────────────────────────

    /// 序列化查询并 POST 到 HelixDB `/v1/query`。
    async fn send_query(&self, query: Value) -> Result<Value, KnowledgeError> {
        let url = format!("{}/v1/query", self.base_url);

        let response = self
            .http
            .post(&url)
            .json(&query)
            .send()
            .await
            .map_err(|e| KnowledgeError::Internal(format!("HelixDB HTTP 请求失败: {e}")))?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| KnowledgeError::Internal(format!("读取 HelixDB 响应 body 失败: {e}")))?;

        let response_body: Value = serde_json::from_str(&response_text).map_err(|e| {
            KnowledgeError::Internal(format!("解析 HelixDB 响应失败: {e}: {response_text}"))
        })?;

        if !status.is_success() {
            let err_msg = response_body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("未知错误");
            return Err(KnowledgeError::Internal(format!(
                "HelixDB 返回 {}: {}",
                status.as_u16(),
                err_msg
            )));
        }

        Ok(response_body)
    }
}
