use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::Stream;
use pin_project_lite::pin_project;
use serde::{Deserialize, Serialize};

use super::error::AgentError;
use model_provider::Usage;

use super::agent::ModelResponse;

/// Content of a streaming tool call delta.
///
/// Each delta carries either the function name (emitted once, first) or
/// a fragment of JSON-encoded arguments (emitted zero or more times
/// thereafter).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ToolCallDeltaContent {
    /// The tool function name has been resolved.
    Name(String),
    /// A fragment of JSON-encoded arguments.
    Arguments(String),
}

/// Token usage record for a single model completion call within an agent run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[allow(dead_code)]
pub struct ModelUsageRecord {
    /// Zero-based index of this completion call within the agent run.
    pub call_index: usize,
    /// Token usage for this specific completion request. Fields are zero when
    /// the provider does not report that metric.
    pub usage: Usage,
}

/// Events emitted during a streaming agent ReAct loop.
///
/// # Event lifecycle
///
/// ```text
/// ┌─ Turn 1 ──────────────────────────────────────────┐
/// │ TextDelta* | ReasoningDelta*                       │  ← model response
/// │ ToolCallDelta* | ToolCallStart                     │  ← tool construction
/// │ ToolResult*                                        │  ← tool execution
/// │ ModelUsage                                    │  ← turn usage snapshot
/// └────────────────────────────────────────────────────┘
/// ┌─ Turn 2 ──────────────────────────────────────────┐
/// │ TextDelta*                                         │
/// │ ModelUsage                                    │
/// └────────────────────────────────────────────────────┘
/// Done { response }                                    │  ← terminal
/// ```
///
/// For providers that do not support streaming tool call construction,
/// `ToolCallStart` is emitted directly without preceding `ToolCallDelta`
/// events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
#[non_exhaustive]
pub enum ModelStreamEvent {
    /// A text content delta from the assistant.
    ///
    /// Multiple `TextDelta` events may be emitted within a single turn.
    /// Concatenating them yields the full assistant text for that turn.
    TextDelta {
        /// The text delta content.
        delta: String,
    },

    /// A reasoning / thinking content delta.
    ///
    /// Emitted when the model produces internal reasoning (e.g. DeepSeek-R1
    /// thinking mode, Anthropic extended thinking).
    ReasoningDelta {
        /// The reasoning delta content.
        delta: String,
    },

    /// A streaming tool call delta.
    ///
    /// Emitted progressively as the model constructs a tool call. A typical
    /// sequence for one tool call is:
    ///
    /// ```text
    /// ToolCallDelta { id: "call_1", content: Name("get_weather") }
    /// ToolCallDelta { id: "call_1", content: Arguments("{\"city\"") }
    /// ToolCallDelta { id: "call_1", content: Arguments(": \"SF\"}") }
    /// ToolCallStart  { id: "call_1", name: "get_weather", arguments: "..." }
    /// ```
    ToolCallDelta {
        /// The tool call identifier (provider-assigned).
        id: String,
        /// The delta content: either the function name or an argument fragment.
        content: ToolCallDeltaContent,
    },

    /// A fully-assembled tool call, ready for execution.
    ///
    /// Always emitted before the corresponding [`ToolResult`](ModelStreamEvent::ToolResult).
    /// When streaming tool call deltas are supported, this follows the final
    /// `ToolCallDelta` for the same `id`.
    ToolCallStart {
        /// The tool call identifier (provider-assigned).
        id: String,
        /// The function name.
        name: String,
        /// Complete JSON-encoded arguments.
        arguments: String,
    },

    /// A tool execution result.
    ///
    /// Emitted after the tool executor has processed a `ToolCallStart`.
    /// The `id` matches the corresponding `ToolCallStart` event.
    ToolResult {
        /// The tool call identifier — matches the `id` from the corresponding
        /// `ToolCallStart`.
        id: String,
        /// The function name that was executed.
        name: String,
        /// The result content returned by the tool.
        result: String,
    },

    /// Token usage snapshot for a single model completion call.
    ///
    /// Emitted immediately after each provider call completes, before
    /// tool execution or the next turn begins. This lets consumers track
    /// token consumption in real time across multi-turn runs.
    ///
    /// The aggregated usage is also available via
    /// [`ModelResponse::usage`](super::ModelResponse::usage) and
    /// the terminal [`Done`](ModelStreamEvent::Done) event.
    ModelUsage {
        /// Zero-based index of this call within the agent run (0, 1, 2, …).
        call_index: usize,
        /// Token usage for this specific request. Zero-valued fields indicate
        /// the provider did not report those metrics.
        usage: Usage,
    },

    /// The agent run completed successfully.
    ///
    /// This is the **terminal event** — no further events will be emitted
    /// after `Done`. The enclosed [`ModelResponse`] carries aggregate usage
    /// and turn count. Per-turn text output arrives via
    /// the `outcome` field of [`TurnComplete`](super::LooperEvent::TurnComplete) events;
    /// message history is managed by [`Session`].
    Done {
        /// The final agent response.
        response: ModelResponse,
    },
}

impl ModelStreamEvent {
    /// Returns `true` if this is a [`TextDelta`](ModelStreamEvent::TextDelta) event.
    pub fn is_text_delta(&self) -> bool {
        matches!(self, Self::TextDelta { .. })
    }

    /// Returns `true` if this is a terminal [`Done`](ModelStreamEvent::Done) event.
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done { .. })
    }

    /// Returns `true` if this event carries tool-related information.
    pub fn is_tool_related(&self) -> bool {
        matches!(
            self,
            Self::ToolCallDelta { .. } | Self::ToolCallStart { .. } | Self::ToolResult { .. }
        )
    }

    /// Extract the text delta, if this is a [`TextDelta`](ModelStreamEvent::TextDelta) event.
    pub fn as_text_delta(&self) -> Option<&str> {
        match self {
            Self::TextDelta { delta } => Some(delta),
            _ => None,
        }
    }

    /// Extract the [`Done`](ModelStreamEvent::Done) response data, if this is the terminal event.
    pub fn as_done(&self) -> Option<&ModelResponse> {
        match self {
            Self::Done { response } => Some(response),
            _ => None,
        }
    }
}

pin_project! {
    /// A stream of agent events produced during a streaming ReAct loop.
    ///
    /// Implements [`futures::Stream`] yielding [`ModelStreamEvent`] items.
    /// Created by [`super::Agent::stream_run`].
    pub struct ModelStream {
        #[pin]
        inner: Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, AgentError>> + Send>>,
    }
}

impl ModelStream {
    /// Create a new `ModelStream` from a boxed stream of agent events.
    pub fn new(
        inner: Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, AgentError>> + Send>>,
    ) -> Self {
        Self { inner }
    }

    /// Poll the next event from the stream.
    ///
    /// Returns `None` when the stream is exhausted.
    pub async fn next_event(&mut self) -> Option<Result<ModelStreamEvent, AgentError>> {
        futures::StreamExt::next(self).await
    }

    /// Consume the stream and return the inner boxed stream.
    pub fn into_inner(
        self,
    ) -> Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, AgentError>> + Send>> {
        self.inner
    }
}

impl Stream for ModelStream {
    type Item = Result<ModelStreamEvent, AgentError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().inner.as_mut().poll_next(cx)
    }
}
