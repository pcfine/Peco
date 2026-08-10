// ============================================================================
// Workflow SSE 事件类型 + 映射函数
// ============================================================================
//
// WorkflowSseEvent 是 WorkflowEvent 的传输层投影，与前端 WorkflowSSEEvent
// 类型（webui/src/types/workflow.ts:183-252）一一对应。
//
// map_event() 是纯函数 — 将领域层 WorkflowEvent 映射为传输层 WorkflowSseEvent。
// 不实现 From trait，因为 WorkflowEvent 属于 pec-core（领域层），
// WorkflowSseEvent 属于 peco-server（传输层），转换逻辑应由传输层拥有。

use axum::response::sse::Event;
use peco_core::workflow::WorkflowEvent;
use serde::Serialize;

// ============================================================================
// WorkflowSseEvent — SSE 传输层事件枚举
// ============================================================================

/// SSE 流中的单个事件，与前端 `WorkflowSSEEvent` 类型完全对齐。
///
/// 使用 `#[serde(tag = "type")]` 生成扁平 JSON 格式：
/// `{"type":"workflow_started","runId":"abc",...}`
///
/// 每个字段使用显式 `#[serde(rename)]` 确保 camelCase 输出。
/// `rename_all` 在 internally-tagged enum 上不会级联到 variant 内部字段，
/// 因此必须逐字段标注（与 ChatSseEvent 模式一致）。
///
/// 前端 `connectStream`（workflows.ts:214）通过 `parsed.data ?? parsed` 兼容
/// 扁平格式和 `{event, data}` 双层包装。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum WorkflowSseEvent {
    #[serde(rename = "workflow_started")]
    Started {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "workflowName")]
        workflow_name: String,
        #[serde(rename = "totalSteps")]
        total_steps: usize,
    },
    #[serde(rename = "step_started")]
    StepStarted {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "stepId")]
        step_id: String,
        #[serde(rename = "stepName")]
        step_name: String,
        #[serde(rename = "stepType")]
        step_type: String,
    },
    #[serde(rename = "step_completed")]
    StepCompleted {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "stepId")]
        step_id: String,
        #[serde(rename = "stepName")]
        step_name: String,
        #[serde(rename = "output")]
        output: String,
        #[serde(rename = "durationMs")]
        duration_ms: u64,
        #[serde(rename = "attempt")]
        attempt: usize,
    },
    #[serde(rename = "step_skipped")]
    StepSkipped {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "stepId")]
        step_id: String,
        #[serde(rename = "stepName")]
        step_name: String,
        #[serde(rename = "reason")]
        reason: String,
    },
    #[serde(rename = "step_failed")]
    StepFailed {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "stepId")]
        step_id: String,
        #[serde(rename = "stepName")]
        step_name: String,
        #[serde(rename = "error")]
        error: String,
        #[serde(rename = "durationMs")]
        duration_ms: u64,
        #[serde(rename = "attempt")]
        attempt: usize,
        /// "continue" | "abort" | "retry" | "pause"
        #[serde(rename = "failurePolicy")]
        failure_policy: String,
    },
    #[serde(rename = "workflow_paused")]
    Paused {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "reason")]
        reason: String,
        #[serde(rename = "pausedAtStep")]
        paused_at_step: Option<String>,
    },
    #[serde(rename = "workflow_resumed")]
    Resumed {
        #[serde(rename = "runId")]
        run_id: String,
    },
    #[serde(rename = "workflow_completed")]
    Completed {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "totalDurationMs")]
        total_duration_ms: u64,
        #[serde(rename = "stepsCompleted")]
        steps_completed: usize,
        #[serde(rename = "stepsFailed")]
        steps_failed: usize,
        #[serde(rename = "stepsSkipped")]
        steps_skipped: usize,
    },
    #[serde(rename = "workflow_failed")]
    Failed {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "error")]
        error: String,
        #[serde(rename = "failedAtStep")]
        failed_at_step: Option<String>,
        #[serde(rename = "totalDurationMs")]
        total_duration_ms: u64,
    },
    #[serde(rename = "workflow_cancelled")]
    Cancelled {
        #[serde(rename = "runId")]
        run_id: String,
    },
    /// 流终止信号 — 无对应 WorkflowEvent，由 handler 在终端事件后发送
    #[serde(rename = "done")]
    Done {
        #[serde(rename = "runId")]
        run_id: String,
    },
}

impl WorkflowSseEvent {
    /// 将事件序列化为 axum SSE Event。
    pub fn to_sse_event(&self) -> Result<Event, serde_json::Error> {
        let data = serde_json::to_string(self)?;
        Ok(Event::default().data(data))
    }
}

// ============================================================================
// map_event — WorkflowEvent → WorkflowSseEvent 映射
// ============================================================================

/// 将领域层 WorkflowEvent 映射为传输层 WorkflowSseEvent。
///
/// # 设计决策
///
/// - **单参数**：所有 `WorkflowEvent` 变体均自包含 `run_id: String`
///   （见 events.rs 头部注释），无需外部注入
/// - **返回 Option**：`StepDelta` / `StepRetrying`（Phase 4 预留）返回 `None`，
///   调用方跳过该事件，不 panic
/// - **不写 `_` 通配符**：显式匹配全部 12 个变体，编译器在新增变体时产生
///   non-exhaustive match 错误，开发者必须显式决定处理方式
pub fn map_event(event: &WorkflowEvent) -> Option<WorkflowSseEvent> {
    match event {
        WorkflowEvent::Started {
            run_id,
            workflow_name,
            total_steps,
        } => Some(WorkflowSseEvent::Started {
            run_id: run_id.clone(),
            workflow_name: workflow_name.clone(),
            total_steps: *total_steps,
        }),
        WorkflowEvent::StepStarted {
            run_id,
            step_id,
            step_name,
            step_type,
        } => Some(WorkflowSseEvent::StepStarted {
            run_id: run_id.clone(),
            step_id: step_id.clone(),
            step_name: step_name.clone(),
            step_type: step_type.clone(),
        }),
        WorkflowEvent::StepDelta {
            run_id,
            step_id,
            text: _text,
        } => {
            // Phase 4 预留 — 前端暂无对应类型，静默跳过
            tracing::debug!(%run_id, %step_id, "StepDelta not mapped to SSE");
            None
        }
        WorkflowEvent::StepCompleted {
            run_id,
            step_id,
            step_name,
            output,
            duration_ms,
            attempt,
        } => Some(WorkflowSseEvent::StepCompleted {
            run_id: run_id.clone(),
            step_id: step_id.clone(),
            step_name: step_name.clone(),
            output: output.clone(),
            duration_ms: *duration_ms,
            attempt: *attempt,
        }),
        WorkflowEvent::StepSkipped {
            run_id,
            step_id,
            step_name,
            reason,
        } => Some(WorkflowSseEvent::StepSkipped {
            run_id: run_id.clone(),
            step_id: step_id.clone(),
            step_name: step_name.clone(),
            reason: reason.clone(),
        }),
        WorkflowEvent::StepFailed {
            run_id,
            step_id,
            step_name,
            error,
            duration_ms,
            attempt,
            failure_policy,
        } => Some(WorkflowSseEvent::StepFailed {
            run_id: run_id.clone(),
            step_id: step_id.clone(),
            step_name: step_name.clone(),
            error: error.clone(),
            duration_ms: *duration_ms,
            attempt: *attempt,
            failure_policy: failure_policy.clone(),
        }),
        WorkflowEvent::StepRetrying {
            run_id, step_id, ..
        } => {
            // Phase 4 预留 — 前端暂无对应类型，静默跳过
            tracing::debug!(%run_id, %step_id, "StepRetrying not mapped to SSE");
            None
        }
        WorkflowEvent::Paused {
            run_id,
            reason,
            paused_at_step,
        } => Some(WorkflowSseEvent::Paused {
            run_id: run_id.clone(),
            reason: reason.clone(),
            paused_at_step: paused_at_step.clone(),
        }),
        WorkflowEvent::Resumed { run_id } => Some(WorkflowSseEvent::Resumed {
            run_id: run_id.clone(),
        }),
        WorkflowEvent::Completed {
            run_id,
            total_duration_ms,
            steps_completed,
            steps_failed,
            steps_skipped,
        } => Some(WorkflowSseEvent::Completed {
            run_id: run_id.clone(),
            total_duration_ms: *total_duration_ms,
            steps_completed: *steps_completed,
            steps_failed: *steps_failed,
            steps_skipped: *steps_skipped,
        }),
        WorkflowEvent::Failed {
            run_id,
            error,
            failed_at_step,
            total_duration_ms,
        } => Some(WorkflowSseEvent::Failed {
            run_id: run_id.clone(),
            error: error.clone(),
            failed_at_step: failed_at_step.clone(),
            total_duration_ms: *total_duration_ms,
        }),
        WorkflowEvent::Cancelled { run_id } => Some(WorkflowSseEvent::Cancelled {
            run_id: run_id.clone(),
        }),
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use peco_core::workflow::WorkflowEvent;

    fn test_run_id() -> String {
        "test-run-1".into()
    }

    #[test]
    fn test_serialize_started() {
        let ev = WorkflowSseEvent::Started {
            run_id: "abc".into(),
            workflow_name: "test".into(),
            total_steps: 3,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(
            json,
            r#"{"type":"workflow_started","runId":"abc","workflowName":"test","totalSteps":3}"#
        );
    }

    #[test]
    fn test_serialize_all_variants_have_type_field() {
        let events: Vec<WorkflowSseEvent> = vec![
            WorkflowSseEvent::Started {
                run_id: "r1".into(),
                workflow_name: "w".into(),
                total_steps: 1,
            },
            WorkflowSseEvent::StepStarted {
                run_id: "r1".into(),
                step_id: "s1".into(),
                step_name: "n".into(),
                step_type: "shell".into(),
            },
            WorkflowSseEvent::StepCompleted {
                run_id: "r1".into(),
                step_id: "s1".into(),
                step_name: "n".into(),
                output: "ok".into(),
                duration_ms: 100,
                attempt: 1,
            },
            WorkflowSseEvent::StepSkipped {
                run_id: "r1".into(),
                step_id: "s1".into(),
                step_name: "n".into(),
                reason: "condition false".into(),
            },
            WorkflowSseEvent::StepFailed {
                run_id: "r1".into(),
                step_id: "s1".into(),
                step_name: "n".into(),
                error: "failed".into(),
                duration_ms: 100,
                attempt: 1,
                failure_policy: "abort".into(),
            },
            WorkflowSseEvent::Paused {
                run_id: "r1".into(),
                reason: "waiting".into(),
                paused_at_step: Some("s1".into()),
            },
            WorkflowSseEvent::Resumed {
                run_id: "r1".into(),
            },
            WorkflowSseEvent::Completed {
                run_id: "r1".into(),
                total_duration_ms: 1000,
                steps_completed: 2,
                steps_failed: 0,
                steps_skipped: 1,
            },
            WorkflowSseEvent::Failed {
                run_id: "r1".into(),
                error: "error".into(),
                failed_at_step: Some("s2".into()),
                total_duration_ms: 500,
            },
            WorkflowSseEvent::Cancelled {
                run_id: "r1".into(),
            },
            WorkflowSseEvent::Done {
                run_id: "r1".into(),
            },
        ];

        for ev in &events {
            let json = serde_json::to_string(ev).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert!(
                parsed.get("type").is_some(),
                "missing 'type' field in: {json}"
            );
            assert!(
                parsed.get("runId").is_some(),
                "missing 'runId' field in: {json}"
            );
        }
    }

    #[test]
    fn test_map_event_active_variants_return_some() {
        let rid = test_run_id();

        // Started
        let ev = WorkflowEvent::Started {
            run_id: rid.clone(),
            workflow_name: "test".into(),
            total_steps: 2,
        };
        let sse = map_event(&ev).unwrap();
        assert!(matches!(sse, WorkflowSseEvent::Started { .. }));

        // StepStarted
        let ev = WorkflowEvent::StepStarted {
            run_id: rid.clone(),
            step_id: "s1".into(),
            step_name: "Lint".into(),
            step_type: "shell".into(),
        };
        let sse = map_event(&ev).unwrap();
        assert!(matches!(sse, WorkflowSseEvent::StepStarted { .. }));

        // StepCompleted
        let ev = WorkflowEvent::StepCompleted {
            run_id: rid.clone(),
            step_id: "s1".into(),
            step_name: "Lint".into(),
            output: "ok".into(),
            duration_ms: 100,
            attempt: 1,
        };
        let sse = map_event(&ev).unwrap();
        assert!(matches!(sse, WorkflowSseEvent::StepCompleted { .. }));

        // StepSkipped
        let ev = WorkflowEvent::StepSkipped {
            run_id: rid.clone(),
            step_id: "s1".into(),
            step_name: "Lint".into(),
            reason: "condition false".into(),
        };
        let sse = map_event(&ev).unwrap();
        assert!(matches!(sse, WorkflowSseEvent::StepSkipped { .. }));

        // StepFailed
        let ev = WorkflowEvent::StepFailed {
            run_id: rid.clone(),
            step_id: "s1".into(),
            step_name: "Lint".into(),
            error: "fail".into(),
            duration_ms: 100,
            attempt: 1,
            failure_policy: "abort".into(),
        };
        let sse = map_event(&ev).unwrap();
        assert!(matches!(sse, WorkflowSseEvent::StepFailed { .. }));

        // Paused
        let ev = WorkflowEvent::Paused {
            run_id: rid.clone(),
            reason: "wait".into(),
            paused_at_step: None,
        };
        let sse = map_event(&ev).unwrap();
        assert!(matches!(sse, WorkflowSseEvent::Paused { .. }));

        // Resumed
        let ev = WorkflowEvent::Resumed {
            run_id: rid.clone(),
        };
        let sse = map_event(&ev).unwrap();
        assert!(matches!(sse, WorkflowSseEvent::Resumed { .. }));

        // Completed
        let ev = WorkflowEvent::Completed {
            run_id: rid.clone(),
            total_duration_ms: 1000,
            steps_completed: 2,
            steps_failed: 0,
            steps_skipped: 0,
        };
        let sse = map_event(&ev).unwrap();
        assert!(matches!(sse, WorkflowSseEvent::Completed { .. }));

        // Failed
        let ev = WorkflowEvent::Failed {
            run_id: rid.clone(),
            error: "error".into(),
            failed_at_step: None,
            total_duration_ms: 500,
        };
        let sse = map_event(&ev).unwrap();
        assert!(matches!(sse, WorkflowSseEvent::Failed { .. }));

        // Cancelled
        let ev = WorkflowEvent::Cancelled {
            run_id: rid.clone(),
        };
        let sse = map_event(&ev).unwrap();
        assert!(matches!(sse, WorkflowSseEvent::Cancelled { .. }));
    }

    #[test]
    fn test_map_event_run_id_preserved() {
        let rid = "my-run-id".to_string();

        let ev = WorkflowEvent::Started {
            run_id: rid.clone(),
            workflow_name: "test".into(),
            total_steps: 1,
        };
        let sse = map_event(&ev).unwrap();
        match sse {
            WorkflowSseEvent::Started { run_id, .. } => assert_eq!(run_id, rid),
            _ => panic!("expected Started"),
        }

        let ev = WorkflowEvent::Completed {
            run_id: rid.clone(),
            total_duration_ms: 100,
            steps_completed: 1,
            steps_failed: 0,
            steps_skipped: 0,
        };
        let sse = map_event(&ev).unwrap();
        match sse {
            WorkflowSseEvent::Completed { run_id, .. } => assert_eq!(run_id, rid),
            _ => panic!("expected Completed"),
        }
    }

    #[test]
    fn test_map_event_phase4_variants_return_none() {
        let rid = test_run_id();

        // StepDelta — Phase 4 预留，不 panic
        let ev = WorkflowEvent::StepDelta {
            run_id: rid.clone(),
            step_id: "s1".into(),
            text: "hello".into(),
        };
        assert!(map_event(&ev).is_none());

        // StepRetrying — Phase 4 预留，不 panic
        let ev = WorkflowEvent::StepRetrying {
            run_id: rid.clone(),
            step_id: "s1".into(),
            attempt: 1,
            max_attempts: 3,
            backoff_seconds: 5,
        };
        assert!(map_event(&ev).is_none());
    }

    #[test]
    fn test_to_sse_event_serializes_correctly() {
        let ev = WorkflowSseEvent::Done {
            run_id: "abc".into(),
        };
        // 直接序列化验证 JSON 产出
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains(r#""type":"done""#));
        assert!(json.contains(r#""runId":"abc""#));
    }
}
