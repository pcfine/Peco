// ============================================================================
// main — peco CLI 聊天助手入口
// ============================================================================

use std::process;
use std::sync::Arc;

mod app;
mod commands;
mod config;
mod display;
mod input;

use config::CliConfig;
use peco_core::config::SystemConfig;
use peco_core::workspace::WorkSpace;

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

    // --template 模板初始化路径
    if let Some(ref template_name) = config.init_template {
        match init_template_workspace(&config, template_name).await {
            Ok(()) => process::exit(0),
            Err(e) => {
                eprintln!("模板初始化失败: {e:#}");
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

/// 从内置模板初始化 workspace 并退出。
async fn init_template_workspace(config: &CliConfig, template_name: &str) -> anyhow::Result<()> {
    let template = peco_agents::BuiltinTemplate::by_name(template_name).ok_or_else(|| {
        let available: Vec<&str> = peco_agents::BuiltinTemplate::all()
            .iter()
            .map(|t| t.name)
            .collect();
        anyhow::anyhow!(
            "未知模板 '{}'。可用模板: {}",
            template_name,
            available.join(", "),
        )
    })?;

    let tmp = template.materialize()?;

    // 打开 WorkSpace（与 CliApp::new 相同的初始化逻辑）
    let system_config = SystemConfig::load();
    let user_id = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "cli-user".to_string());

    let workspace = Arc::new(
        WorkSpace::open(config.workspace_root.clone(), user_id, &system_config)
            .map_err(|e| anyhow::anyhow!("WorkSpace 创建失败: {e}"))?,
    );

    let report = workspace
        .init_from_template(tmp.path())
        .await
        .map_err(|e| anyhow::anyhow!("模板初始化失败: {e}"))?;

    eprintln!(
        "模板 '{}' 已应用: agents +{}/skipped {}, kbs +{}/skipped {}, errors {}",
        template_name,
        report.agents_installed.len(),
        report.agents_skipped.len(),
        report.kbs_created.len(),
        report.kbs_skipped.len(),
        report.errors.len(),
    );
    for (name, err) in &report.errors {
        eprintln!("  ⚠ {name}: {err}");
    }

    Ok(())
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
