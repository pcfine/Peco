//! 多工作空间 daemon 的地基：单用户 DB、marker、空间打开/关闭、任务台账与 HTTP 层。
//!
//! 已打开实例表复用 `peco_core::tools::WorkspaceHub`，不在本层另存一份。
//! agent / skill / knowledge / mcp 四个资源模块与 chat 会话都按空间隔离，
//! 身份固定为 [`LOCAL_USER_ID`]；派发观察闭环与 Peco 专属会话不在本层。

pub mod agent;
pub mod api_error;
pub mod artifact;
pub mod auth;
pub mod chat;
pub mod config;
pub mod db;
pub mod dynamic;
pub mod error;
pub mod file_watcher;
pub mod http;
pub mod knowledge;
pub mod mcp_config;
pub mod session_dto;
pub mod session_store;
pub mod single_instance;
pub mod skill;
pub mod space;
pub mod state;
pub mod team;
pub mod wake;

/// 单用户身份：DB 各表的 `user_id` 列恒写这个值。
///
/// 与知识库隔离维度（`kb_scope` = `team_id`）是两个概念：把 team 写进
/// `user_id` 列会让两套语义在同一个列名上混住，反之把 `local` 传给
/// `WorkSpace::open` 会让不同 team 的同名 KB 撞进同一命名空间。
pub const LOCAL_USER_ID: &str = "local";
