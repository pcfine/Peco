// ============================================================================
// ServerConfig — 从环境变量加载服务配置
// ============================================================================

use std::path::PathBuf;

use sqlx::SqlitePool;

/// 服务运行配置。
///
/// 所有字段均可通过环境变量覆盖，未设置时使用合理默认值。
pub struct ServerConfig {
    /// 绑定地址，默认 `0.0.0.0`。
    pub host: String,
    /// 监听端口，默认 `9227`。
    pub port: u16,
    /// SQLite 数据库连接串，默认 `sqlite:~/.peco/server.db?mode=rwc`。
    pub database_url: String,
    /// JWT 签名密钥（三层降级：环境变量 → DB 持久化 → 随机生成+持久化）。
    pub jwt_secret: String,
    /// 数据存储根目录，默认 `~/.peco/`。
    pub data_dir: PathBuf,
}

impl ServerConfig {
    /// 从环境变量加载配置（不含 DB 持久化支持）。
    ///
    /// 等价于 `from_env_with_db(None)`，JWT 密钥不会持久化。
    pub fn from_env() -> Result<Self, anyhow::Error> {
        // 解析通用字段
        let (host, port, database_url, data_dir) = parse_common_env();

        // JWT 密钥：环境变量或随机生成（不持久化）
        let jwt_secret = resolve_jwt_secret_no_db();

        Ok(Self {
            host,
            port,
            database_url,
            jwt_secret,
            data_dir,
        })
    }

    /// 从环境变量加载配置，支持 JWT 密钥 DB 持久化。
    ///
    /// # JWT 密钥三层降级策略
    ///
    /// 1. 环境变量 `PECO_JWT_SECRET` → 直接使用（生产推荐）
    /// 2. 环境变量未设置 → 从 `server_config` 表读取已持久化的密钥
    /// 3. DB 中也无记录 → 随机生成 UUID → 持久化到 DB
    ///
    /// # 环境变量
    ///
    /// | 变量 | 默认值 |
    /// |------|--------|
    /// | `PECO_SERVER_HOST` | `0.0.0.0` |
    /// | `PECO_SERVER_PORT` | `9227` |
    /// | `PECO_DATABASE_URL` | `sqlite:~/.peco/server.db?mode=rwc` |
    /// | `PECO_JWT_SECRET` | 环境变量 → DB → 随机 UUID（重启后不失效） |
    /// | `PECO_DATA_DIR` | `~/.peco/` |
    pub async fn from_env_with_db(pool: &SqlitePool) -> Result<Self, anyhow::Error> {
        // 解析通用字段
        let (host, port, database_url, data_dir) = parse_common_env();

        // JWT 密钥三层降级：环境变量 → DB → 随机生成+持久化
        let jwt_secret = resolve_jwt_secret_with_db(pool).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to resolve JWT secret from DB, falling back");
            resolve_jwt_secret_no_db()
        });

        Ok(Self {
            host,
            port,
            database_url,
            jwt_secret,
            data_dir,
        })
    }
}

// ── 内部辅助函数 ──────────────────────────────────────────────────────────────

/// 解析通用环境变量（与 JWT 无关的部分）。
fn parse_common_env() -> (String, u16, String, PathBuf) {
    let host = std::env::var("PECO_SERVER_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

    let port = std::env::var("PECO_SERVER_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9227u16);

    let database_url = std::env::var("PECO_DATABASE_URL").unwrap_or_else(|_| {
        let home = home_dir();
        let db_path = home.join(".peco").join("server.db");
        format!("sqlite:{}?mode=rwc", db_path.display())
    });

    let data_dir = std::env::var("PECO_DATA_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".peco"));

    (host, port, database_url, data_dir)
}

/// JWT 密钥解析（无 DB 持久化）：环境变量 > 随机生成。
fn resolve_jwt_secret_no_db() -> String {
    if let Ok(secret) = std::env::var("PECO_JWT_SECRET")
        && !secret.is_empty()
    {
        tracing::info!("Using JWT secret from PECO_JWT_SECRET environment variable");
        return secret;
    }

    let secret = uuid::Uuid::new_v4().to_string();
    tracing::warn!(
        "PECO_JWT_SECRET not set and no DB pool available; using random secret. \
         All tokens will be invalidated on next restart."
    );
    secret
}

/// JWT 密钥解析（含 DB 持久化）：环境变量 > DB 读取 > 随机生成+持久化到 DB。
async fn resolve_jwt_secret_with_db(pool: &SqlitePool) -> Result<String, anyhow::Error> {
    // 第一层：环境变量 PECO_JWT_SECRET（生产推荐方式）
    if let Ok(secret) = std::env::var("PECO_JWT_SECRET")
        && !secret.is_empty()
    {
        tracing::info!("Using JWT secret from PECO_JWT_SECRET environment variable");
        return Ok(secret);
    }

    // 第二层：从 DB server_config 表读取持久化的密钥
    let stored = crate::db::get_server_config(pool, "jwt_secret").await?;
    if let Some(secret) = stored {
        tracing::info!("JWT secret loaded from database (persisted across restarts)");
        return Ok(secret);
    }

    // 第三层：随机生成并持久化到 DB
    let secret = uuid::Uuid::new_v4().to_string();
    crate::db::set_server_config(pool, "jwt_secret", &secret).await?;
    tracing::warn!(
        "PECO_JWT_SECRET not set; generated and persisted JWT secret to database. \
         Tokens will survive restarts as long as the same database is used."
    );
    Ok(secret)
}

/// 获取用户主目录路径。
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}
