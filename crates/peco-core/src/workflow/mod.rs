// ============================================================================
// workflow — 声明式 DAG 工作流编排模块
// ============================================================================

pub mod access;
pub mod condition;
pub mod dag;
pub mod definition;
pub mod engine;
pub mod error;
pub mod events;
pub mod handle;
pub mod manager;
pub mod persistence;
pub mod step_executor;
pub mod template;
pub mod tools;

// 便捷 re-export
pub use access::WorkflowAccess;
pub use condition::evaluate_condition;
pub use dag::DagGraph;
pub use definition::{
    OnFailure, RetryPolicy, StepConfig, StepOutcome, StepResult, StepType, WorkflowDefinition,
    WorkflowInput, WorkflowStep,
};
pub use engine::{SharedWorkflowTask, WorkflowConfig, WorkflowEngine};
pub use error::WorkflowError;
pub use events::{ApprovalDecision, ApprovalResponse, WorkflowEvent};
pub use handle::WorkflowHandle;
pub use manager::{WorkflowManager, WorkflowMeta};
pub use persistence::{
    NullWorkflowPersister, WorkflowPersister, WorkflowSnapshot, WorkflowSnapshotState,
};
pub use template::TemplateContext;
pub use tools::ExecuteWorkflow;
