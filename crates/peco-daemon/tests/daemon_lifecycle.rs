//! 真实二进制的生命周期：临时数据根 + 随机端口 + SIGTERM。
//!
//! 进程内夹具证不了信号路径 —— `SingleInstanceGuard::drop` 与 sqlite 收尾只在
//! `run()` 真的返回时才执行，因此这里 spawn 真正的 `peco-daemon`。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use peco_daemon::config::HOME_DIR_NAME;
use peco_daemon::single_instance::PID_FILE;

/// 与既有服务（9227 / 6969 / 6970 / 9233）以及默认地址 6971 都错开。
const RESERVED_PORTS: [u16; 5] = [6971, 6969, 6970, 9227, 9233];

const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// 端口由 `free_port()` 挑好再交给守护进程，中间存在抢跑窗口；撞车就换一个再来。
const PORT_ATTEMPTS: usize = 3;

struct Instance {
    child: Child,
    addr: String,
    data_home: PathBuf,
    log_path: PathBuf,
}

impl Instance {
    /// `command` 只负责给出决定数据根的那个环境变量，端口与日志由这里统一接管。
    ///
    /// 换端口要重建 `Command`（它被 `spawn` 消费），所以调用方给的是构造函数。
    fn launch(mut command: impl FnMut() -> Command, data_home: PathBuf, log_path: &Path) -> Self {
        for attempt in 1..=PORT_ATTEMPTS {
            let instance = Self::spawn(command(), data_home.clone(), log_path);
            match instance.wait_until_serving() {
                Ok(()) => return instance,
                Err(err)
                    if attempt < PORT_ATTEMPTS
                        && instance.logs().contains("Address already in use") =>
                {
                    eprintln!(
                        "端口 {} 被抢（第 {attempt} 次尝试），换端口重试：{err}",
                        instance.addr
                    );
                }
                Err(err) => panic!("{err}"),
            }
        }
        unreachable!("最后一次尝试要么返回实例，要么在上面 panic")
    }

    fn spawn(mut command: Command, data_home: PathBuf, log_path: &Path) -> Self {
        let port = free_port();
        let addr = format!("127.0.0.1:{port}");
        let log = std::fs::File::create(log_path).unwrap();
        let child = command
            .env("PECO_DAEMON_BIND_ADDR", &addr)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawn peco-daemon");
        Self {
            child,
            addr,
            data_home,
            log_path: log_path.to_path_buf(),
        }
    }

    fn daemon_binary() -> Command {
        Command::new(env!("CARGO_BIN_EXE_peco-daemon"))
    }

    fn pid_file(&self) -> PathBuf {
        self.data_home.join(PID_FILE)
    }

    /// 等 pid 文件写出且内容是本进程，再等 `/api/peco` 真的回 200。
    ///
    /// 就绪不能只看 TCP connect：socket 监听先于库迁移与 bootstrap，连接成功时
    /// 接口还可能 500 或长时间不答。
    fn wait_until_serving(&self) -> Result<(), String> {
        let pid = self.child.id();
        poll(STARTUP_TIMEOUT, || {
            std::fs::read_to_string(self.pid_file())
                .ok()
                .filter(|raw| raw.trim() == pid.to_string())
        })
        .ok_or_else(|| format!("60s 内没写出 {PID_FILE}（期望 pid={pid}）：{}", self.logs()))?;

        poll(STARTUP_TIMEOUT, || {
            self.try_get("/api/peco")
                .and_then(|raw| status_code(&raw))
                .filter(|code| *code == 200)
        })
        .map(|_| ())
        .ok_or_else(|| {
            format!(
                "60s 内 {} 的 /api/peco 没回 200：{}",
                self.addr,
                self.logs()
            )
        })
    }

    fn sigterm(&self) {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .expect("kill(1) 不可用");
        assert!(status.success(), "kill -TERM 失败：{}", self.logs());
    }

    fn wait_exit(&mut self) -> ExitStatus {
        match poll(SHUTDOWN_TIMEOUT, || self.child.try_wait().unwrap()) {
            Some(status) => status,
            None => {
                let _ = self.child.kill();
                panic!("SIGTERM 后 30s 未退出：{}", self.logs());
            }
        }
    }

    /// 收尾的 Drop 是否跑过：pid 文件消失即说明 `run()` 退了作用域。
    fn wait_pid_file_removed(&self) {
        poll(SHUTDOWN_TIMEOUT, || {
            (!self.pid_file().exists()).then_some(())
        })
        .unwrap_or_else(|| panic!("SIGTERM 后 30s pid 文件仍在：{}", self.logs()));
    }

    fn logs(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    fn get(&self, path: &str) -> String {
        self.try_get(path)
            .unwrap_or_else(|| panic!("GET {path} 没有响应：{}", self.logs()))
    }

    /// 就绪轮询要能在服务还没起来时安静地返回 `None`，所以这里不 panic。
    fn try_get(&self, path: &str) -> Option<String> {
        let mut stream = TcpStream::connect(&self.addr).ok()?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.addr
        )
        .ok()?;
        let mut raw = String::new();
        stream.read_to_string(&mut raw).ok()?;
        (!raw.is_empty()).then_some(raw)
    }
}

/// 只解析状态行里的状态码，不为一个断言引入 HTTP 解析依赖。
fn status_code(raw: &str) -> Option<u16> {
    let mut parts = raw.split_whitespace();
    let _version = parts.next()?;
    parts.next()?.parse().ok()
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    loop {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        if !RESERVED_PORTS.contains(&port) {
            return port;
        }
    }
}

fn poll<T>(timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(value) = probe() {
            return Some(value);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    probe()
}

/// 优雅退出的唯一端到端判据：退出码 0（不是被信号终止）+ `daemon.pid` 被 Drop 删掉。
#[test]
fn sigterm_exits_zero_and_removes_the_pid_file() {
    let home = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let data_home = home.path().to_path_buf();
    let mut instance = Instance::launch(
        || {
            let mut command = Instance::daemon_binary();
            command.env("PECO_HOME", &data_home);
            command
        },
        data_home.clone(),
        &logs.path().join("daemon.log"),
    );

    instance.wait_until_serving().expect("daemon 应开始接客");
    instance.sigterm();
    let status = instance.wait_exit();

    assert_eq!(
        status.code(),
        Some(0),
        "SIGTERM 必须走优雅退出（被信号终止时 code 为 None）。日志：\n{}",
        instance.logs()
    );
    assert!(
        !instance.pid_file().exists(),
        "pid 文件应由 Drop 删除。日志：\n{}",
        instance.logs()
    );
    let logs = instance.logs();
    assert!(
        logs.contains("shutdown signal received"),
        "缺少优雅退出日志：\n{logs}"
    );
    assert!(
        logs.contains("\"term\""),
        "日志未标出信号来源是 SIGTERM：\n{logs}"
    );
}

/// 开着永久 SSE 时 SIGTERM 仍须在 3s 内退出且 rc=0。
///
/// `/api/events` 与任务观察流都是「浏览器开着就永不结束」的流，靠它自然结束来等
/// 优雅退出等于永远等不到。变异方法：把 `shutdown.cancel()` 从信号闭包里删掉
/// （或让 `http::serve` 回到只等连接）⇒ 本用例在 [`wait_exit`] 处超时变红。
#[test]
fn sigterm_exits_zero_within_three_seconds_while_an_sse_stream_is_open() {
    let home = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let data_home = home.path().to_path_buf();
    let mut instance = Instance::launch(
        || {
            let mut command = Instance::daemon_binary();
            command.env("PECO_HOME", &data_home);
            command
        },
        data_home.clone(),
        &logs.path().join("daemon.log"),
    );

    instance.wait_until_serving().expect("daemon 应开始接客");

    // 这条连接活到用例结束：先确认握手成功，否则测的是「没有在途连接」的路径。
    let mut sse = TcpStream::connect(&instance.addr).expect("连上守护进程");
    write!(
        sse,
        "GET /api/events HTTP/1.1\r\nHost: {}\r\nAccept: text/event-stream\r\n\r\n",
        instance.addr
    )
    .expect("写出 SSE 请求");
    let mut header = [0u8; 64];
    let read = sse.read(&mut header).expect("SSE 响应头可读");
    let head = String::from_utf8_lossy(&header[..read]).to_string();
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "SSE 握手失败：{head}\n{}",
        instance.logs()
    );

    let started = Instant::now();
    instance.sigterm();
    let status = instance.wait_exit();
    let elapsed = started.elapsed();

    assert_eq!(
        status.code(),
        Some(0),
        "SIGTERM 必须走优雅退出。日志：\n{}",
        instance.logs()
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "SIGTERM 到进程退出用了 {elapsed:?}，超过 3s。日志：\n{}",
        instance.logs()
    );

    drop(sse);
}

/// 不设 `PECO_HOME` 时数据根是 `$HOME/<HOME_DIR_NAME>`；用临时 `HOME` 隔开真实家目录。
#[test]
fn without_peco_home_the_data_root_is_the_home_dir_name() {
    let fake_home = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let data_home = fake_home.path().join(HOME_DIR_NAME);
    let fake_home_path = fake_home.path().to_path_buf();
    let instance = Instance::launch(
        || {
            let mut command = Instance::daemon_binary();
            command.env("HOME", &fake_home_path).env_remove("PECO_HOME");
            command
        },
        data_home.clone(),
        &logs.path().join("daemon.log"),
    );

    instance.wait_until_serving().expect("daemon 应开始接客");
    let body = instance.get("/api/peco");
    let work_dir = json_string_field(&body, "work_dir");
    instance.sigterm();
    instance.wait_pid_file_removed();

    let root = std::fs::canonicalize(&data_home).unwrap();
    assert!(
        Path::new(&work_dir).starts_with(&root),
        "work_dir 应落在 {} 之下，实际 {work_dir}",
        root.display()
    );
    assert!(
        data_home.join("peco.db").is_file(),
        "首启应在数据根建出 peco.db：{}",
        root.display()
    );
}

/// 只解析本用例用到的字段，不为一个断言引入 json 依赖。
fn json_string_field(body: &str, key: &str) -> String {
    let needle = format!("\"{key}\":\"");
    let start = body
        .find(&needle)
        .unwrap_or_else(|| panic!("响应里没有 {key}：{body}"))
        + needle.len();
    let rest = &body[start..];
    let end = rest.find('"').unwrap();
    rest[..end].to_string()
}
