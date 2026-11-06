//! `/api/peco` —— Peco 默认空间的身份。
//!
//! 前端要靠它定位「总管空间」再去读 Peco 的会话与派发中的任务。`ws_id` 由
//! 记录文件里的会话 id 派生，用户手改过就与种子常量不同，因此这里回的是
//! 启动时解析出的值，不是字面量。

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct PecoView {
    pub ws_id: String,
    pub work_dir: String,
    pub team_id: String,
    /// 这条路由描述的就是保留空间本身，故恒为 true；前端据此隐藏删除入口。
    pub reserved: bool,
    /// `false` 表示记录文件缺失或损坏，`ws_id` 是种子兜底值。
    pub from_marker: bool,
}

pub async fn detail(State(state): State<AppState>) -> Json<PecoView> {
    let peco = &state.env.peco;
    Json(PecoView {
        ws_id: peco.ws_id.clone(),
        work_dir: peco.work_dir.to_string_lossy().into_owned(),
        team_id: peco.team_id.clone(),
        reserved: true,
        from_marker: peco.from_marker,
    })
}
