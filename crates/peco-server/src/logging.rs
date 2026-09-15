// ============================================================================
// logging — tracing 初始化：stdout + 按大小轮转的日志文件双写
// ============================================================================

//! 全局日志初始化与按大小轮转的文件 writer。
//!
//! - stdout 层保持既有 `fmt()` 行为不变（systemd/journald 部署依赖 stdout）。
//! - 文件层默认开启：写入 `{data_dir}/logs/peco-server.log`，单文件超过上限后
//!   轮转为 `peco-server.log.1`（最新）→ `.N-1`（最旧），总数超出保留数即删除，
//!   因此磁盘占用有硬上界。
//! - 任何文件系统失败都不 panic、不向上传播：降级为仅 stdout + eprintln 兜底。
//! - 文件层不带 ANSI 颜色码，便于 grep 和归档。
//!
//! # 环境变量
//!
//! | 变量 | 默认值 | 说明 |
//! |------|--------|------|
//! | `PECO_LOG_TO_FILE` | `true` | `false`/`0`/`off`（大小写不敏感）时关闭文件层 |
//! | `PECO_LOG_DIR` | `{data_dir}/logs` | 日志目录，启动时自动创建 |
//! | `PECO_LOG_MAX_SIZE_MB` | `10` | 单文件字节上限（软上限，单条超大日志允许整体落入新文件） |
//! | `PECO_LOG_MAX_FILES` | `5` | 目录内日志文件总数上限（含当前文件），最小 2 |
//!
//! # 默认过滤器
//!
//! `model_provider` 必须显式列出：它不以 `peco` 开头，没有自己的 directive 时会落到
//! EnvFilter 的 ERROR 默认级别，而该 crate 只发 warn/debug —— 于是 LLM 调用层的
//! 所有异常（非 2xx、SSE 重连、丢弃工具调用）都会静默。
//! 排查 LLM 问题：`RUST_LOG=model_provider=debug`，看完整请求体用 `=trace`。
//! 日志时间戳用本机时区；offset 后缀让时间戳自描述，便于与 UTC 落库时间戳对账。

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::time::ChronoLocal;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::resolve_data_dir;

/// 两层共用的日志时间戳格式。
const TIMER_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.3f%:z";

/// 无 `RUST_LOG` 时的默认过滤器。
const DEFAULT_FILTER: &str = "peco=info,model_provider=info,tower_http=info";

const LOG_FILE_NAME: &str = "peco-server.log";
const DEFAULT_MAX_SIZE_MB: u64 = 10;
const DEFAULT_MAX_FILES: usize = 5;

/// 初始化全局 tracing：stdout 层 + 可选的按大小轮转文件层，共享同一 `EnvFilter`。
pub fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));

    // stdout 层：行为与既往单层 fmt() 完全一致。
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_timer(ChronoLocal::new(TIMER_FORMAT.into()))
        .with_writer(io::stdout)
        .with_filter(filter.clone());

    // 文件层：`PECO_LOG_TO_FILE=false` 或目录/文件初始化失败时不挂载
    //（`Option<Layer>` 为 no-op 层），降级为仅 stdout。
    let file_layer = if file_logging_enabled() {
        let config = LoggingConfig::from_env();
        match RotatingFileWriter::new(&config) {
            Some(writer) => Some(
                tracing_subscriber::fmt::layer()
                    .with_timer(ChronoLocal::new(TIMER_FORMAT.into()))
                    .with_ansi(false)
                    .with_writer(writer)
                    .with_filter(filter.clone()),
            ),
            None => {
                eprintln!(
                    "peco-server: 日志文件初始化失败（dir: {}），降级为仅 stdout",
                    config.dir.display()
                );
                None
            }
        }
    } else {
        None
    };

    tracing_subscriber::registry()
        .with(stdout_layer)
        .with(file_layer)
        .init();
}

// ── 配置 ─────────────────────────────────────────────────────────────────────

/// 日志文件层配置。纯数据，与 env 解耦，便于单测直接构造。
#[derive(Clone)]
pub(crate) struct LoggingConfig {
    /// 日志目录。
    dir: PathBuf,
    /// 当前日志文件名，轮转文件为其 `.N` 后缀。
    file_name: String,
    /// 单文件字节上限（软上限）。
    max_size: u64,
    /// 目录内日志文件总数上限（含当前文件）。
    max_files: usize,
}

impl LoggingConfig {
    /// 从环境变量解析配置，风格对齐 `config.rs` 的 `parse_common_env`：
    /// 非法值静默回退默认。
    fn from_env() -> Self {
        let dir = std::env::var("PECO_LOG_DIR")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| resolve_data_dir().join("logs"));
        let max_size = std::env::var("PECO_LOG_MAX_SIZE_MB")
            .ok()
            .and_then(|s| parse_max_size_mb(&s))
            .unwrap_or(DEFAULT_MAX_SIZE_MB * 1024 * 1024);
        let max_files = std::env::var("PECO_LOG_MAX_FILES")
            .ok()
            .and_then(|s| parse_max_files(&s))
            .unwrap_or(DEFAULT_MAX_FILES);
        Self {
            dir,
            file_name: LOG_FILE_NAME.to_string(),
            max_size,
            max_files,
        }
    }

    /// 第 `seq` 个轮转文件路径（`peco-server.log.1` … `peco-server.log.N-1`）。
    fn suffixed(&self, seq: usize) -> PathBuf {
        self.dir.join(format!("{}.{}", self.file_name, seq))
    }

    fn base_path(&self) -> PathBuf {
        self.dir.join(&self.file_name)
    }
}

/// 文件层开关：仅精确匹配 `false`/`0`/`off`（大小写不敏感）时关闭，其余一律开启。
fn file_logging_enabled() -> bool {
    std::env::var("PECO_LOG_TO_FILE")
        .map(|s| parse_to_file(&s))
        .unwrap_or(true)
}

/// 解析单文件大小上限（MB）。0 或非法值返回 `None`（调用方回退默认；
/// 0 会导致每条日志轮转一次，视同非法）。
fn parse_max_size_mb(s: &str) -> Option<u64> {
    let mb = s.trim().parse::<u64>().ok()?;
    (mb > 0).then(|| mb * 1024 * 1024)
}

/// 解析日志文件保留数（含当前文件）。小于 2 或非法值返回 `None`
///（=1 会使轮转后没有当前文件，视同非法）。
fn parse_max_files(s: &str) -> Option<usize> {
    let n = s.trim().parse::<usize>().ok()?;
    (n >= 2).then_some(n)
}

fn parse_to_file(s: &str) -> bool {
    !matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "false" | "0" | "off"
    )
}

// ── 按大小轮转的文件 writer ──────────────────────────────────────────────────

/// 轮转 writer 的共享状态。锁覆盖"轮转判定 + 轮转 + 写入"全过程，
/// 并发写串行化，单条事件天然原子。
struct Inner {
    config: LoggingConfig,
    /// 当前文件句柄。`None` = 文件层已降级（新开文件失败），后续写入静默丢弃。
    file: Option<File>,
    /// 当前文件已写入字节数。
    written: u64,
    /// 运行期问题只 eprintln 一次（stdout 层照常工作，避免刷屏）。
    warn_emitted: bool,
}

/// 按大小轮转的日志文件 writer，作为 tracing fmt 层的 `MakeWriter` 使用。
pub(crate) struct RotatingFileWriter {
    inner: Arc<Mutex<Inner>>,
}

impl RotatingFileWriter {
    /// 创建日志目录并打开当前日志文件（append + create），任一步失败返回 `None`。
    ///
    /// `written` 取自现有文件长度：跨重启续写同一文件，不截断历史；
    /// 若旧文件已超限，下一条事件会触发一次轮转。
    fn new(config: &LoggingConfig) -> Option<Self> {
        if let Err(e) = std::fs::create_dir_all(&config.dir) {
            eprintln!(
                "peco-server: 无法创建日志目录 {}: {e}",
                config.dir.display()
            );
            return None;
        }
        Self::clean_stale_files(config);

        match OpenOptions::new()
            .append(true)
            .create(true)
            .open(config.base_path())
        {
            Ok(file) => {
                let written = file.metadata().map(|m| m.len()).unwrap_or(0);
                Some(Self {
                    inner: Arc::new(Mutex::new(Inner {
                        config: config.clone(),
                        file: Some(file),
                        written,
                        warn_emitted: false,
                    })),
                })
            }
            Err(e) => {
                eprintln!(
                    "peco-server: 无法打开日志文件 {}: {e}",
                    config.base_path().display()
                );
                None
            }
        }
    }

    /// 清理编号 >= max_files 的陈旧轮转文件（此前可能用过更大的保留数）。
    /// 清理失败不影响本轮轮转。
    fn clean_stale_files(config: &LoggingConfig) {
        let Ok(entries) = std::fs::read_dir(&config.dir) else {
            return;
        };
        let prefix = format!("{}.", config.file_name);
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some(seq) = name
                .strip_prefix(&prefix)
                .and_then(|s| s.parse::<usize>().ok())
            else {
                continue;
            };
            if seq >= config.max_files {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

impl<'a> MakeWriter<'a> for RotatingFileWriter {
    type Writer = RotatingWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        RotatingWriter { inner: &self.inner }
    }
}

/// 每次 `make_writer()` 返回的写入 guard，对应一条日志事件的写入。
pub(crate) struct RotatingWriter<'a> {
    inner: &'a Mutex<Inner>,
}

impl Write for RotatingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // 持锁线程 panic 后日志通道不能永久死掉，恢复内部数据继续工作。
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        // `File` 无用户态缓冲，write 即落内核 page cache，无需额外 flush。
        Ok(())
    }
}

impl Inner {
    /// 写入一条日志。失败静默吞掉（stdout 层仍有该事件），绝不向上传播 `Err`。
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.file.is_none() {
            return Ok(buf.len()); // 已降级：静默丢弃，stdout 层仍有该事件
        }

        // 单文件软上限：跨过阈值即轮转，单条超大日志允许整体落入新文件。
        // 轮转中途失败但旧句柄仍在时，宁超限不丢日志，继续写入旧文件。
        if self.written + buf.len() as u64 > self.config.max_size {
            self.rotate();
        }

        let Some(file) = self.file.as_mut() else {
            return Ok(buf.len());
        };
        match file.write_all(buf) {
            Ok(()) => {
                self.written += buf.len() as u64;
                Ok(buf.len())
            }
            Err(e) => {
                // 不推进 written：下一条事件重新触发轮转尝试，磁盘恢复后自愈。
                self.warn_once(format!("日志写入失败: {e}"));
                Ok(buf.len())
            }
        }
    }

    /// 轮转日志文件。
    ///
    /// 失败语义：中途 rename 失败时保留现有句柄继续写旧文件（fd 跟随 inode，
    /// 宁超限不丢日志）；仅新开文件失败时置 `file = None`，此后仅 stdout。
    fn rotate(&mut self) {
        RotatingFileWriter::clean_stale_files(&self.config);

        // 依次后移：`.N-2 → .N-1` … `.1 → .2`，为 `.1` 腾位。
        for i in (2..self.config.max_files).rev() {
            let from = self.config.suffixed(i - 1);
            let to = self.config.suffixed(i);
            if from.exists()
                && let Err(e) = std::fs::rename(&from, &to)
            {
                self.warn_once(format!(
                    "日志轮转失败（{} → {}）: {e}",
                    from.display(),
                    to.display()
                ));
                return;
            }
        }

        // 当前文件 → `.1`。
        let base = self.config.base_path();
        if let Err(e) = std::fs::rename(&base, self.config.suffixed(1)) {
            self.warn_once(format!("日志轮转失败（{} → .1）: {e}", base.display()));
            return;
        }

        // 新开当前文件。
        match OpenOptions::new().append(true).create(true).open(&base) {
            Ok(file) => {
                self.file = Some(file);
                self.written = 0;
            }
            Err(e) => {
                self.warn_once(format!("日志轮转后无法重新打开 {}: {e}", base.display()));
                self.file = None;
            }
        }
    }

    fn warn_once(&mut self, msg: String) {
        if !self.warn_emitted {
            self.warn_emitted = true;
            eprintln!("peco-server: {msg}（后续同类问题不再重复提示）");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(dir: &std::path::Path, max_size: u64, max_files: usize) -> LoggingConfig {
        LoggingConfig {
            dir: dir.to_path_buf(),
            file_name: LOG_FILE_NAME.to_string(),
            max_size,
            max_files,
        }
    }

    fn write_via_writer(writer: &RotatingFileWriter, data: &[u8]) {
        writer.make_writer().write_all(data).unwrap();
    }

    fn read(dir: &std::path::Path, name: &str) -> Vec<u8> {
        std::fs::read(dir.join(name)).unwrap_or_default()
    }

    #[test]
    fn writes_within_threshold_stay_in_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path(), 100, 5);
        let writer = RotatingFileWriter::new(&config).unwrap();
        write_via_writer(&writer, b"hello ");
        write_via_writer(&writer, b"world");
        assert_eq!(read(dir.path(), LOG_FILE_NAME), b"hello world");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn crossing_threshold_rotates_current_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path(), 8, 5);
        let writer = RotatingFileWriter::new(&config).unwrap();
        write_via_writer(&writer, b"01234567"); // 写满但未超
        write_via_writer(&writer, b"abc"); // 触发轮转，落入新文件
        assert_eq!(read(dir.path(), LOG_FILE_NAME), b"abc");
        assert_eq!(read(dir.path(), "peco-server.log.1"), b"01234567");
    }

    #[test]
    fn rotation_respects_max_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path(), 4, 3);
        let writer = RotatingFileWriter::new(&config).unwrap();
        for chunk in [b"aaaa".as_slice(), b"bbbb", b"cccc", b"dddd"] {
            write_via_writer(&writer, chunk);
        }
        // base = "dddd"，.1 = "cccc"，.2 = "bbbb"；最老的 "aaaa" 被淘汰
        assert_eq!(read(dir.path(), LOG_FILE_NAME), b"dddd");
        assert_eq!(read(dir.path(), "peco-server.log.1"), b"cccc");
        assert_eq!(read(dir.path(), "peco-server.log.2"), b"bbbb");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[test]
    fn reopening_appends_and_resumes_size_tracking() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path(), 100, 5);
        {
            let writer = RotatingFileWriter::new(&config).unwrap();
            write_via_writer(&writer, b"first;");
        }
        let writer = RotatingFileWriter::new(&config).unwrap();
        write_via_writer(&writer, b"second");
        assert_eq!(read(dir.path(), LOG_FILE_NAME), b"first;second");

        // 旧文件已超限时，续写后下一条事件触发轮转而不是无限增长
        let config_small = test_config(dir.path(), 4, 5);
        let writer = RotatingFileWriter::new(&config_small).unwrap();
        write_via_writer(&writer, b"x");
        assert_eq!(read(dir.path(), LOG_FILE_NAME), b"x");
        assert_eq!(read(dir.path(), "peco-server.log.1"), b"first;second");
    }

    #[test]
    fn oversized_single_event_goes_to_new_file_whole() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path(), 4, 5);
        let writer = RotatingFileWriter::new(&config).unwrap();
        write_via_writer(&writer, b"tiny");
        write_via_writer(&writer, b"one-very-large-event");
        // 软上限：单条事件整体落入新文件，不切割
        assert_eq!(read(dir.path(), LOG_FILE_NAME), b"one-very-large-event");
        assert_eq!(read(dir.path(), "peco-server.log.1"), b"tiny");
    }

    #[test]
    fn stale_high_numbered_files_are_cleaned() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path(), 4, 3);

        // new() 启动即清理
        std::fs::write(dir.path().join("peco-server.log.4"), b"stale").unwrap();
        let writer = RotatingFileWriter::new(&config).unwrap();
        assert!(!dir.path().join("peco-server.log.4").exists());

        // 轮转时也清理
        std::fs::write(dir.path().join("peco-server.log.4"), b"stale").unwrap();
        write_via_writer(&writer, b"aaaa");
        write_via_writer(&writer, b"bbbb"); // 触发轮转
        assert!(!dir.path().join("peco-server.log.4").exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn env_value_parsers_reject_invalid_values() {
        assert_eq!(parse_max_size_mb("10"), Some(10 * 1024 * 1024));
        assert_eq!(parse_max_size_mb(" 2 "), Some(2 * 1024 * 1024));
        assert_eq!(parse_max_size_mb("0"), None);
        assert_eq!(parse_max_size_mb("abc"), None);
        assert_eq!(parse_max_size_mb(""), None);

        assert_eq!(parse_max_files("5"), Some(5));
        assert_eq!(parse_max_files("2"), Some(2));
        assert_eq!(parse_max_files("1"), None);
        assert_eq!(parse_max_files("0"), None);
        assert_eq!(parse_max_files("abc"), None);

        assert!(parse_to_file("true"));
        assert!(parse_to_file("TRUE"));
        assert!(parse_to_file("yes"));
        assert!(!parse_to_file("false"));
        assert!(!parse_to_file("Off"));
        assert!(!parse_to_file("0"));
    }
}
