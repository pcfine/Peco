// ============================================================================
// main — peco CLI 聊天助手入口
// ============================================================================

use std::process;

mod app;
mod commands;
mod config;
mod display;
mod input;

use config::CliConfig;

#[tokio::main]
async fn main() {
    // 初始化 tracing（先于任何日志输出）
    init_tracing();

    // 加载 .env（失败不阻塞启动）
    let _ = dotenvy::dotenv();

    // 解析配置
    let config = match CliConfig::from_args_and_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("配置错误: {e}");
            process::exit(1);
        }
    };

    // --list-sessions 特殊路径
    if config.list_sessions {
        match app::list_sessions_and_exit(&config).await {
            Ok(()) => process::exit(0),
            Err(e) => {
                eprintln!("列出会话失败: {e:#}");
                process::exit(1);
            }
        }
    }

    // 启动 CLI
    let mut cli_app = match app::CliApp::new(config).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("初始化失败: {e:#}");
            process::exit(1);
        }
    };

    if let Err(e) = cli_app.run().await {
        eprintln!("运行错误: {e:#}");
        process::exit(1);
    }
}

/// 初始化 `tracing_subscriber`，日志级别由 `RUST_LOG` 环境变量控制。
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}
