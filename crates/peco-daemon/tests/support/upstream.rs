//! 测试用假上游：本地 OpenAI chat completions 兼容端点，按请求内容脚本化回复。
//!
//! 观察闭环的判据要求「真实一轮对话里调用工具」，用真 provider 不可判红
//! （不稳定、要 key）。本模块只在测试进程里装配，生产代码不认识它：
//! 走的是生产同一条 `provider_type = "openai"` + `base_url` 适配器路径。
//!
//! 只实现非流式：`AgentLooper` 走哪条路径由 `agent.md` 的 `llm.stream` 决定，
//! 测试夹具不设该字段，故走批量路径。

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type Responder = Arc<dyn Fn(Value) -> BoxFuture<Reply> + Send + Sync>;

/// 一次脚本化回复。
#[derive(Debug, Clone)]
pub enum Reply {
    /// 纯文本最终答复（`finish_reason: "stop"`）。
    Text(String),
    /// 一次工具调用（`finish_reason: "tool_calls"`）。
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    /// 正文 + 工具调用同帧返回：用来构造「这一轮已经产出一段、还要继续跑」的
    /// 中间态（纯文本回复是这一轮的最后一步，构造不出半程）。
    TextAndToolCall {
        text: String,
        id: String,
        name: String,
        arguments: Value,
    },
}

impl Reply {
    pub fn tool_call(name: &str, arguments: Value) -> Self {
        Self::ToolCall {
            id: format!("call_{name}"),
            name: name.to_string(),
            arguments,
        }
    }

    pub fn text_and_tool_call(text: &str, name: &str, arguments: Value) -> Self {
        Self::TextAndToolCall {
            text: text.to_string(),
            id: format!("call_{name}"),
            name: name.to_string(),
            arguments,
        }
    }
}

/// 一次往返：请求体与返回给模型的响应体，供测试打印「真的跑了一轮」的证据。
#[derive(Debug, Clone)]
pub struct Exchange {
    pub request: Value,
    pub response: Value,
}

struct Shared {
    responder: Responder,
    exchanges: Mutex<Vec<Exchange>>,
}

impl Shared {
    fn exchanges(&self) -> MutexGuard<'_, Vec<Exchange>> {
        self.exchanges
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub struct FakeUpstream {
    /// 填进 `providers.toml` 的 `base_url`（含 `/v1`）。
    pub base_url: String,
    shared: Arc<Shared>,
}

impl FakeUpstream {
    pub async fn start<F, Fut>(responder: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Reply> + Send + 'static,
    {
        let shared = Arc::new(Shared {
            responder: Arc::new(move |request| Box::pin(responder(request))),
            exchanges: Mutex::new(Vec::new()),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the fake upstream");
        let addr = listener.local_addr().expect("local addr");
        let router = Router::new()
            .route("/v1/chat/completions", post(chat_completions))
            .with_state(Arc::clone(&shared));
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Self {
            base_url: format!("http://{addr}/v1"),
            shared,
        }
    }

    pub fn exchanges(&self) -> Vec<Exchange> {
        self.shared.exchanges().clone()
    }

    /// 只取走 `model` 这一档的往返。
    ///
    /// 同一端点除了对话轮还会收到元任务请求（记忆提取跑在后台，用的是另一档
    /// 模型），数「这一轮问了几次模型」时不能把元任务算进来。
    pub fn exchanges_with_model(&self, model: &str) -> Vec<Exchange> {
        self.exchanges()
            .into_iter()
            .filter(|exchange| exchange.request["model"] == json!(model))
            .collect()
    }

    /// 本端点收到的工具名（请求体里 `tools[].function.name`），用于断言
    /// 「模型真的看到了这个工具」。
    pub fn offered_tools(&self) -> Vec<String> {
        self.shared
            .exchanges()
            .iter()
            .flat_map(|exchange| {
                exchange.request["tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

async fn chat_completions(
    State(shared): State<Arc<Shared>>,
    Json(request): Json<Value>,
) -> Response {
    let reply = (shared.responder)(request.clone()).await;
    let response = reply_body(reply);
    shared.exchanges().push(Exchange {
        request,
        response: response.clone(),
    });
    Json(response).into_response()
}

fn reply_body(reply: Reply) -> Value {
    let choice = match reply {
        Reply::Text(text) => json!({
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop",
        }),
        Reply::ToolCall {
            id,
            name,
            arguments,
        } => json!({
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments.to_string()},
                }],
            },
            "finish_reason": "tool_calls",
        }),
        Reply::TextAndToolCall {
            text,
            id,
            name,
            arguments,
        } => json!({
            "message": {
                "role": "assistant",
                "content": text,
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments.to_string()},
                }],
            },
            "finish_reason": "tool_calls",
        }),
    };
    json!({
        "id": "chatcmpl-fake",
        "choices": [choice],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    })
}

/// 请求里的模型名：测试用它把「哪个 agent 在说话」与脚本解耦。
pub fn model_of(request: &Value) -> &str {
    request["model"].as_str().unwrap_or_default()
}

/// 本请求是否已经带着工具结果（即这是工具执行后的续轮，而非一个新 turn）。
pub fn has_tool_result(request: &Value) -> bool {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|message| message["role"] == json!("tool"))
}
