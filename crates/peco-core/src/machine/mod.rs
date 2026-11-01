pub mod echo;
pub mod hub;
// 模块按职责命名，契约文件就叫 machine.rs；重名只是路径重复，不表示分层。
#[allow(clippy::module_inception)]
pub mod machine;
pub mod model;
pub mod sink;
pub mod state;
pub mod store;

pub use crate::machine::machine::{Machine, MachineDescriptor, MachineError};
pub use echo::EchoMachine;
pub use hub::{HubConfig, ObsStream, TaskHub};
pub use model::{
    Artifact, ArtifactId, OutputView, Progress, TaggedJson, Task, TaskContext, TaskError,
    TaskFilter, TaskId, TaskOutcome, TaskSnapshot, TaskSource,
};
pub use sink::{SinkError, TaskSink};
pub use state::{ObsEvent, OutputChunk, TaskState, Update, UpdateBody, UpdateOrigin};
pub use store::{InMemoryStore, StoreError, TaskStore};
