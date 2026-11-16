//! `/api/brand` —— 左栏品牌图片：一张上传好的成品图，存 `server_config` 一行 JSON。
//!
//! 品牌图只有一张，不按空间或团队分层，也就不值得为它开一张表；裁剪与缩放
//! 在前端做完，这里只认成品 URL。

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::db::{get_server_config, set_server_config};
use crate::error::DaemonError;
use crate::state::AppState;

/// `server_config` 里这一行的 key。
const BRAND_KEY: &str = "brand";

/// 品牌图只能来自上传目录下的这一处：调用方交的是 URL，磁盘路径由服务端拼。
const BRAND_URL_PREFIX: &str = "/uploads/brand/";

/// URL 长度上限，与团队图标的校验同口径。
const MAX_URL_LEN: usize = 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BrandConfig {
    /// `/uploads/brand/...`；空串表示未设置，前端画默认标记。
    pub image: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRequest {
    /// 空串表示恢复默认。
    pub image: String,
}

/// 空串是「未设置」的合法写法；非空必须落在品牌图目录下。
fn validate_image_url(image: &str) -> Result<(), DaemonError> {
    let ok = image.is_empty()
        || (image.len() <= MAX_URL_LEN
            && image.starts_with(BRAND_URL_PREFIX)
            && !image.contains("..")
            && !image.chars().any(char::is_control));
    if ok {
        Ok(())
    } else {
        Err(DaemonError::InvalidArgument {
            message: format!("image must be an uploaded {BRAND_URL_PREFIX}... URL, or empty"),
        })
    }
}

/// 坏值报错而不是当成未设置：静默退回默认标记会让「存进去的图不见了」无从查起。
async fn load(state: &AppState) -> Result<BrandConfig, DaemonError> {
    let Some(raw) = get_server_config(&state.pool, BRAND_KEY).await? else {
        return Ok(BrandConfig::default());
    };
    serde_json::from_str(&raw)
        .map_err(|e| DaemonError::Config(format!("stored '{BRAND_KEY}' config is unparsable: {e}")))
}

pub async fn get(State(state): State<AppState>) -> Result<Json<BrandConfig>, DaemonError> {
    Ok(Json(load(&state).await?))
}

pub async fn update(
    State(state): State<AppState>,
    Json(request): Json<UpdateRequest>,
) -> Result<Json<BrandConfig>, DaemonError> {
    validate_image_url(&request.image)?;
    let config = BrandConfig {
        image: request.image,
    };
    let value = serde_json::to_string(&config).map_err(|e| DaemonError::Internal {
        message: format!("failed to serialize brand config: {e}"),
    })?;
    set_server_config(&state.pool, BRAND_KEY, &value).await?;
    Ok(Json(config))
}
