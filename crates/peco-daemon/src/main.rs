//! daemon 入口：配置 → 单用户 DB 与迁移 → 单实例检查 → 索引普查 → HTTP。

use std::process::ExitCode;
use std::sync::Arc;

use peco_core::tools::{WorkspaceHub, WorkspaceOpener, install_workspace_hub};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use peco_daemon::chat::ChatActiveRuns;
use peco_daemon::config::{self, DaemonConfig};
use peco_daemon::db;
use peco_daemon::error::DaemonError;
use peco_daemon::http;
use peco_daemon::single_instance;
use peco_daemon::space::SpaceEnv;
use peco_daemon::space::ledger::SqliteTaskLedger;
use peco_daemon::space::open::DaemonOpener;
use peco_daemon::space::peco;
use peco_daemon::space::registry::SpaceRegistry;
use peco_daemon::space::tasks::TaskObserver;
use peco_daemon::state::AppState;
use peco_daemon::wake::deliver::PecoDelivery;
use peco_daemon::wake::pump::WakePump;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("PECO_LOG")
                .unwrap_or_else(|_| EnvFilter::new("peco_daemon=debug,warn")),
        )
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!(code = e.code(), error = %e, "daemon stopped");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), DaemonError> {
    let config = Arc::new(DaemonConfig::load()?);
    std::fs::create_dir_all(&config.peco_home)?;

    let pool = db::connect(&config.db_url()).await?;
    db::run_migrations(&pool).await?;

    // 守卫持有到进程退出：pid 文件的存活期就是 daemon 的存活期。
    let _instance = single_instance::check_single_instance(&config.peco_home)?;

    // 基座为空：系统层是 `$PECO_HOME/providers.toml` 这一最低优先文件层，留一份
    // 启动快照当基座会让删掉的条目被快照复活。MCP 仍由 core 加载。
    let system_config = Arc::new(config::base_system_config());
    // 进程级配置只注入一次：工作空间工具经 `peco_core::config::system_config()` 读取。
    peco_core::config::install_system_config(system_config.clone());

    // 系统层文件的位置只出现在日志里（前端一句路径都不出现）。损坏它会让所有
    // 团队的空间都打不开，而打开期的错误不带路径 —— 这条 ERROR 是唯一的定位入口。
    // 只报不改：daemon 不替用户动既有文件。
    let system_providers = config.system_providers_root().join(config::PROVIDERS_FILE);
    info!(
        path = %system_providers.display(),
        exists = system_providers.exists(),
        "system providers file"
    );
    if let Err(e) = config::check_system_providers_file(&system_providers) {
        error!(
            path = %system_providers.display(),
            error = %e,
            "system providers file is broken; team spaces may fail to open"
        );
    }

    // Peco 身份在装配 Hub 之前解析一次：台账分组计数与 HTTP 视图必须看同一份。
    let peco = Arc::new(peco::resolve(&config.peco_home));
    info!(
        ws_id = %peco.ws_id,
        from_marker = peco.from_marker,
        "peco identity resolved"
    );
    if !peco.from_marker {
        // 台账分组与清单都按这个 id 认「谁是 Peco」：兜底值能用，但记录文件缺失
        // 说明这个 home 还没引导过，或记录被删了。
        warn!(
            ws_id = %peco.ws_id,
            work_dir = %peco.work_dir.display(),
            "peco workspace marker missing; using the seed id until bootstrap writes it"
        );
    }
    // 唤醒泵先于观察者建出：终态回调要在同一份泵上叫醒它，观察者因此持一个
    // 泵的引用；反过来泵若持 `AppState` 就成环了（见 `attach_delivery`）。
    let wake = Arc::new(WakePump::new(pool.clone(), peco.clone()));
    let tasks = Arc::new(TaskObserver::new(pool.clone(), wake.clone()));
    let env = Arc::new(SpaceEnv::new(
        &config,
        pool.clone(),
        (*system_config).clone(),
        peco,
        tasks.clone(),
    ));

    // 进程内唯一实例表：工具经 `workspace_hub()` 复用同一份 opener 与实例。
    // daemon 是单用户宿主，label 只作展示，不做隔离校验。
    let opener = Arc::new(DaemonOpener::new(env.clone()));
    let hub = Arc::new(
        WorkspaceHub::new()
            .with_opener(opener as Arc<dyn WorkspaceOpener>)
            .with_label_enforcement(false)
            // 重启后 Hub 里没有旧任务的槽：正文只能从台账取。
            .with_ledger(Arc::new(SqliteTaskLedger::new(pool.clone()))),
    );
    // 反向注入：opener 在打开实例后要能把文件变更扇出到同团队的其它空间，
    // 而 Hub 需要 opener 才能构造 —— 二者只有一方能事后拿到对方。
    env.attach_hub(&hub);
    if install_workspace_hub(hub.clone()).is_err() {
        // 启动期不应已有别的组件初始化过 Hub；出现即说明进程装配顺序被破坏。
        error!("process-level workspace hub already initialized; refusing to start");
        return Err(DaemonError::Internal {
            message: "workspace hub already installed".into(),
        });
    }

    // Peco 的团队与默认空间是 Peco 会话的宿主：先幂等装好，再普查 ——
    // 普查在引导之前跑会白扫一次刚建出来的目录。
    peco_daemon::space::bootstrap::bootstrap_peco(&env).await;

    let report = db::workspaces::rescan(&pool, &config.peco_home).await?;
    info!(
        teams = report.teams,
        spaces = report.spaces,
        "workspace index rescanned"
    );

    // 启动不预开用户空间：空间在被进入时打开（`POST /api/spaces/{ws_id}/open`
    // 由前端在进入时调用）。Peco 主空间是例外，它由 `bootstrap_peco` 打开 ——
    // 会话宿主与唤醒落点必须从第一个请求起就在。

    // 资源模块经这一个适配层取实例：Hub 仍是唯一真源，注册表只做 ws_id → 目录。
    let spaces = Arc::new(SpaceRegistry::new(hub.clone(), env.clone()));
    let state = AppState {
        pool,
        hub,
        env,
        config,
        tasks,
        spaces,
        chat_runs: Arc::new(ChatActiveRuns::new()),
        wake,
    };
    // 投递端在这里才拿到 `SpaceRegistry` 与 `AppState`：泵必须早于观察者存在，
    // 故只能晚绑定（`OnceLock`），否则 `pump ↔ AppState` 会成 Arc 环。
    state.wake.attach_delivery(Arc::new(PecoDelivery::new(
        state.pool.clone(),
        state.spaces.clone(),
        state.chat_runs.clone(),
        state.env.peco.clone(),
        Arc::downgrade(&state.env),
    )));

    // 观察者已随上一个进程消失，只有台账知道哪些任务在飞：在接客之前对一遍账。
    // 失败只 warn（`reconcile_ledger` 自己吞错），一个坏行不该拦住 daemon 启动。
    let report = peco_daemon::space::reconcile::reconcile_ledger(&state).await;
    info!(
        scanned = report.scanned,
        finished = report.finished,
        orphaned = report.orphaned,
        resubscribed = report.resubscribed,
        "dispatch ledger reconciled"
    );

    // 回收必须在 reconcile 之后：对账会把重启前的在飞行标成 orphan 并保留唤醒态，
    // 更早被认领、随上个进程一起停下的行则只有这一步能把它们退回队列。顺序反了，
    // 首个 tick 就看不到这些行。
    state.wake.startup_reclaim().await;
    state.wake.spawn();

    http::serve(state).await?;
    info!("http server stopped; daemon exit");
    Ok(())
}
