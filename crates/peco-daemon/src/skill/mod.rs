//! Skill 管理：路由与处理函数。
//!
//! Skill 落在团队能力根的 `skills/<name>/`，读写都经 `SkillRegister`，
//! 与 Agent 工具路径共用同一套校验与原子写入。

mod handler;

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

/// # 路由
/// - `GET /` — 列表
/// - `POST /import` — 导入
/// - `GET /{name}` — 读取 SKILL.md
/// - `PUT /{name}` — 保存
/// - `DELETE /{name}` — 删除
/// - `GET /{name}/export` — 导出
/// - `GET /{name}/files` — 资源文件清单 / 单文件内容
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(handler::list))
        .route("/import", post(handler::import_skill))
        .route(
            "/{name}",
            get(handler::get)
                .put(handler::upsert)
                .delete(handler::delete_skill),
        )
        .route("/{name}/export", get(handler::export_skill))
        .route("/{name}/files", get(handler::files))
}
