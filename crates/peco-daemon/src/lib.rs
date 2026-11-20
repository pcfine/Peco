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
pub mod memory;
pub mod provider;
pub mod schedule;
pub mod session_dto;
pub mod session_store;
pub mod shutdown;
pub mod single_instance;
pub mod skill;
pub mod space;
pub mod state;
pub mod team;
pub mod wake;
pub mod web_auth;

/// 单用户身份：DB 各表的 `user_id` 列恒写这个值。
///
/// 与知识库隔离维度（`kb_scope` = `team_id`）是两个概念：把 team 写进
/// `user_id` 列会让两套语义在同一个列名上混住，反之把 `local` 传给
/// `WorkSpace::open` 会让不同 team 的同名 KB 撞进同一命名空间。
pub const LOCAL_USER_ID: &str = "local";

/// 测试专用：进程级环境变量的作用域守卫。
///
/// 环境变量是进程状态：读它的用例必须彼此串行（共用 [`ENV_LOCK`]），否则结果
/// 取决于执行顺序。守卫负责还原成进入前的样子，包括「原本未设置」。
#[cfg(test)]
pub(crate) mod test_env {
    use std::ffi::{OsStr, OsString};
    use std::sync::Mutex;

    pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) struct EnvVar {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVar {
        pub(crate) fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let previous = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(raw) => unsafe { std::env::set_var(self.key, raw) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }
}
