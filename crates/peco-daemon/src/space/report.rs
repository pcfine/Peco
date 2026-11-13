//! 派发任务的交付副本：终态报告落进目标空间的 work 目录。
//!
//! `d_dispatches` 仍是唤醒与对账的唯一事实源；本模块写的是给人看的副本 ——
//! 任务派到哪个空间跑，那个空间的「文件」页签里就该有一份结果，人脱离 Peco
//! 也读得到。
//!
//! 三个写入口（运行期 `on_terminal`、重启对账 `reconcile_row`、force 关闭时的
//! 批量 orphan）都必须走到这里，且必须算出同一个路径：文件名只由 task_id 与
//! 台账里的任务描述决定，不含时间，否则同一个任务的两次写盘会留下两份文件。
//!
//! 写文件失败只 warn —— 磁盘写不动不能拖垮台账、唤醒与事件广播。
//!
//! 报告里的 `retention` 是**相对台账**的留存口径，不是「完整无缺」：`full` 指
//! 台账留有正文（它自己可能已在上限处截断，那件事由 `output_truncated` 单独
//! 表达），`digest_only` 指只剩摘要，`none` 指两样都没有。别把它读成「全文都在
//! 这里」。

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use peco_core::agent::hooks::{DispatchObservation, MAX_DIGEST_CHARS};
use sqlx::SqlitePool;
use tracing::{debug, warn};

use crate::db::dispatches::{
    self, DispatchRow, MAX_PERSISTED_OUTPUT_CHARS, ReportSource, clamp_chars,
};
use crate::db::workspaces;

/// 报告目录，相对目标空间 work 目录。`output/` 是空间产物约定目录，前端文件
/// 页签浏览的就是这里。
const REPORT_SUBDIR: &str = "output/dispatch";
/// 文件名里任务描述的长度上限：够看清是什么任务，又不至于把目录撑乱。
const SLUG_MAX_CHARS: usize = 24;
/// 标题取任务描述第一行的长度上限。
const TITLE_MAX_CHARS: usize = 80;
/// 供后续迁移识别文件格式。
const REPORT_VERSION: u32 = 1;
/// 反查不到空间行时的占位值：交付物不因元数据缺失就不写。
const UNKNOWN: &str = "unknown";

// ============================================================================
// 报告的身份与正文
// ============================================================================

/// 写一份报告需要的全部身份信息。
///
/// 与台账行/观察相比这里持有所有权：两条终态路径都先落库、再写文件，中间隔着
/// `await`，借来的行活不到那个点。
pub struct ReportMeta {
    pub task_id: String,
    pub dispatch_id: Option<String>,
    pub target_dir: PathBuf,
    pub target_ws_id: String,
    pub team_id: String,
    pub origin_ws_id: String,
    pub agent: String,
    /// 派发入参里那段任务描述的前 512 字；旧行是 `None`。台账里有值时以台账
    /// 为准：运行的观察可能已经丢了它（重订阅），两侧各用各的就会算出两个文件名。
    pub prompt_digest: Option<String>,
    /// 台账 `created_at`；只有读不到台账行时才为 `None`，那时写「未知」。
    pub created_at: Option<String>,
}

/// 报告要记的那件事。
#[derive(Debug, Clone, Copy)]
pub enum ReportBody<'a> {
    Terminal {
        state: &'a str,
        terminal_at: Option<&'a str>,
        /// 台账留存的全文；没有正文时为 `None`。
        output: Option<&'a str>,
        /// 只剩摘要时的摘要（旧行）；不得拿它冒充全文。
        digest: Option<&'a str>,
        /// 这份正文是不是已经被截断过。
        ///
        /// 台账行拿得到时以台账为准（见 [`ReportBody::with_source`]）：台账在
        /// 上限处截过一刀后，报告里这段正文就不可能再是全文。
        output_truncated: bool,
        error: Option<&'a str>,
    },
    Orphan {
        reason: &'a str,
        /// 失联时刻无从得知：`None` 时按落盘时间估计并在 frontmatter 标注。
        terminal_at: Option<&'a str>,
    },
}

impl<'a> ReportBody<'a> {
    /// 用台账行里的事实替换手头这份正文。
    ///
    /// 台账是唯一事实源：`finish` 用 `COALESCE(?, output)` 保留旧正文，重复终态
    /// 时第二次快照可能已经没有 `output`，据快照写就会把台账里还在的全文抹成
    /// 「无输出」。两个入口都先落库再写文件，故拿到行的那一刻台账已是最终值。
    fn with_source<'s>(self, source: &'s ReportSource) -> ReportBody<'s>
    where
        'a: 's,
    {
        match self {
            ReportBody::Terminal {
                state,
                terminal_at,
                error,
                ..
            } => ReportBody::Terminal {
                state,
                terminal_at,
                output: source.output.as_deref(),
                digest: source.result_digest.as_deref(),
                output_truncated: source.output_truncated,
                error,
            },
            ReportBody::Orphan {
                reason,
                terminal_at,
            } => ReportBody::Orphan {
                reason,
                terminal_at,
            },
        }
    }
}

/// 写盘前的失败原因。都不该被上抛：调用方只 warn。
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("report path is not a flat file inside the target workspace: {0}")]
    UnsafePath(PathBuf),
    #[error("task id does not start with 8 hex characters: {0}")]
    UnsafeTaskId(String),
    #[error("target workspace directory is gone: {0}")]
    TargetMissing(PathBuf),
    #[error("cannot write the dispatch report: {0}")]
    Io(#[from] io::Error),
}

// ============================================================================
// 落点
// ============================================================================

/// 任务描述 → 文件名片段。
///
/// 这里是安全边界：文件名进的是真实文件系统，任务描述来自模型，`../` 一类字符
/// 必须在拼路径之前就消失。保留 `is_alphanumeric`（含中文，目录里已有中文命名
/// 的产物）与 `-` / `_`，其余一律换成 `-`。
pub fn slug_of(prompt_digest: Option<&str>) -> String {
    let line = first_line(prompt_digest).unwrap_or_default();
    let mut slug = String::new();
    for c in line.chars() {
        if c.is_alphanumeric() || c == '_' {
            slug.push(c);
        } else if c == '-' {
            // 连续 `-` 合并：`a -- b` 与 `a-b` 该是同一个名字。
            if !slug.ends_with('-') && !slug.is_empty() {
                slug.push('-');
            }
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    // 前导 `.` 已被换成 `-` 并去掉，这里再挡一次是为了不产生隐藏文件。
    let slug = slug.strip_prefix('.').unwrap_or(slug);
    let slug: String = slug.chars().take(SLUG_MAX_CHARS).collect();
    // 截断可能停在 `-` 上：留着只会让名字读起来像被咬掉一口。
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug.to_string()
    }
}

/// 报告路径。纯计算：不碰磁盘，故运行期与对账只要拿到同一份 meta 就是同一路径。
pub fn path_of(meta: &ReportMeta) -> Result<PathBuf, ReportError> {
    let path = meta.target_dir.join(REPORT_SUBDIR).join(file_name(meta)?);
    guard(&path, &meta.target_dir)?;
    Ok(path)
}

/// `<slug>__<task_id 前 8 位>.md`：小写 hex，可从 task_id 反查，且不含时间。
fn file_name(meta: &ReportMeta) -> Result<String, ReportError> {
    Ok(format!(
        "{}__{}.md",
        slug_of(meta.prompt_digest.as_deref()),
        task_id_prefix(&meta.task_id)?
    ))
}

/// 文件名的 id 段：前 8 位必须是 ASCII hex。
///
/// 这一段直接进文件名，而 task_id 不是我们能信的输入 —— `abcd/../../../etc/x`
/// 的前 8 位是 `abcd/...`，拼出来的路径会逃出报告目录。先把它规整成「只可能是
/// 十六进制的一小段」，注入面在拼路径之前就不存在了；不合法是调用方的错，返回
/// `Err` 而不是 panic。
fn task_id_prefix(task_id: &str) -> Result<String, ReportError> {
    let prefix: String = task_id.chars().take(8).collect();
    if prefix.len() == 8 && prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(prefix.to_ascii_lowercase())
    } else {
        Err(ReportError::UnsafeTaskId(task_id.to_string()))
    }
}

/// 运行期断言（不是 `debug_assert`）：release 构建下这条边界同样必须成立。
///
/// 判据是「恰好一层普通文件名，且就在 `<target_dir>/output/dispatch` 下」：
/// `starts_with(target_dir)` 恒真，`file_name()` 只看最后一段，两者都证明不了
/// 扁平 —— `<target>/output/dispatch/a/../../x.md` 既在目录里、最后一段也干净。
fn guard(path: &Path, target_dir: &Path) -> Result<(), ReportError> {
    let rel = path
        .strip_prefix(target_dir.join(REPORT_SUBDIR))
        .map_err(|_| ReportError::UnsafePath(path.to_path_buf()))?;
    let mut components = rel.components();
    let flat =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if flat {
        Ok(())
    } else {
        Err(ReportError::UnsafePath(path.to_path_buf()))
    }
}

// ============================================================================
// 内容
// ============================================================================

/// 由正文决定的那些事实：frontmatter 与正文段落共用一份，避免两处判据漂移。
///
/// `retention` 说的是「相对台账留了多少」，不是「这份正文完不完整」—— 两者的
/// 差别由 `truncated` 单独表达。
struct Facts<'a> {
    state: &'a str,
    /// 台账/快照给的终态时刻；孤儿与旧行没有，落盘时再补。
    terminal_at: Option<&'a str>,
    estimated: bool,
    retention: &'static str,
    chars: usize,
    truncated: bool,
    result: String,
    note: Option<String>,
    error: Option<&'a str>,
}

impl<'a> Facts<'a> {
    fn of(body: &'a ReportBody<'a>) -> Self {
        match body {
            ReportBody::Terminal {
                state,
                terminal_at,
                output,
                digest,
                output_truncated,
                error,
            } => {
                // 空串等同于没有正事可说：写成空的「结果」段比说明「无输出」更误导。
                let output = output.filter(|text| !text.is_empty());
                let digest = digest.filter(|text| !text.is_empty());
                let (retention, chars, truncated, result, note) = match (output, digest) {
                    (Some(text), _) => {
                        let (shown, clamped) = clamp_chars(text);
                        // 台账在上限处截过一刀时，报告里这段正文同样不是全文：
                        // 两个来源任一为真，标志与提示都必须跟着为真。
                        let truncated = clamped || *output_truncated;
                        let result = if truncated {
                            format!("{shown}\n\n{}", truncated_notice())
                        } else {
                            shown.to_string()
                        };
                        ("full", shown.chars().count(), truncated, result, None)
                    }
                    // 摘要只是摘要：把「原文已不可考」写进报告，免得读者当成全文。
                    (None, Some(text)) => (
                        "digest_only",
                        text.chars().count(),
                        // 报告里的这段是不是原文的全部已不可知，只能标「不完整」。
                        true,
                        text.to_string(),
                        Some(format!(
                            "**全文未留存**：台账里只剩 {MAX_DIGEST_CHARS} 字摘要，\
                             原文长度不可考 —— 请勿把摘要当作全文。"
                        )),
                    ),
                    (None, None) => (
                        "none",
                        0,
                        false,
                        format!("无输出（state=`{state}`）；台账未留存正文"),
                        Some(format!(
                            "任务没有产出正文（state=`{state}`），台账也没有正文可查。"
                        )),
                    ),
                };
                Self {
                    state,
                    terminal_at: *terminal_at,
                    estimated: false,
                    retention,
                    chars,
                    truncated,
                    result,
                    note,
                    error: *error,
                }
            }
            ReportBody::Orphan {
                reason,
                terminal_at,
            } => {
                let estimated = terminal_at.is_none();
                Self {
                    state: "orphan",
                    terminal_at: *terminal_at,
                    estimated,
                    retention: "none",
                    chars: 0,
                    truncated: false,
                    result: format!("任务失联，无结果可回捞。原因：`{reason}`"),
                    note: Some(format!(
                        "失联原因：`{reason}`。任务随目标空间一起消失，没有结果可回捞。"
                    )),
                    error: None,
                }
            }
        }
    }
}

/// 截断提示：报告与台账用的是同一个上限，故不能说「完整正文见台账」。
fn truncated_notice() -> String {
    let limit = grouped(MAX_PERSISTED_OUTPUT_CHARS);
    format!("⚠️ 正文超过 {limit} 字符，台账同样只保留前 {limit} 字符，缺失部分不再可得")
}

/// 千分位分组：提示文案里的字数要一眼读得出是几位，`1000000` 读起来像 10 万。
fn grouped(count: usize) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

pub fn render(meta: &ReportMeta, body: &ReportBody<'_>) -> String {
    let facts = Facts::of(body);
    let terminal_at = resolved_terminal_at(&facts);

    let mut text = String::from("---\n");
    scalar(&mut text, "task_id", &meta.task_id);
    match meta.dispatch_id.as_deref() {
        Some(id) => scalar(&mut text, "dispatch_id", id),
        None => text.push_str("dispatch_id: null\n"),
    }
    scalar(&mut text, "space", &meta.target_ws_id);
    scalar(&mut text, "team", &meta.team_id);
    scalar(&mut text, "origin_space", &meta.origin_ws_id);
    scalar(&mut text, "agent", &meta.agent);
    scalar(&mut text, "state", facts.state);
    scalar(
        &mut text,
        "created_at",
        meta.created_at.as_deref().unwrap_or("未知"),
    );
    scalar(&mut text, "terminal_at", &terminal_at);
    text.push_str(&format!("retention: {}\n", facts.retention));
    text.push_str(&format!("output_chars: {}\n", facts.chars));
    text.push_str(&format!("output_truncated: {}\n", facts.truncated));
    text.push_str(&format!("report_version: {REPORT_VERSION}\n"));
    if facts.estimated {
        text.push_str("terminal_at_estimated: true\n");
    }
    text.push_str("---\n\n");

    text.push_str("# ");
    text.push_str(&title_of(meta));
    text.push('\n');
    text.push('\n');
    text.push_str(&format!(
        "> 空间 `{}` · 执行者 `{}` · 终态 `{}` · `{}`\n",
        meta.target_ws_id, meta.agent, facts.state, terminal_at
    ));
    text.push_str(&format!(
        "> 派发自 `{}` · task `{}` · dispatch `{}`\n\n",
        meta.origin_ws_id,
        meta.task_id,
        meta.dispatch_id.as_deref().unwrap_or("—")
    ));

    text.push_str("## 派发指令\n\n");
    match meta
        .prompt_digest
        .as_deref()
        .map(str::trim)
        .filter(|digest| !digest.is_empty())
    {
        Some(digest) => {
            text.push_str(&format!("派发摘要（最多 {MAX_DIGEST_CHARS} 字）：\n\n"));
            text.push_str(digest);
        }
        None => text.push_str("未记录"),
    }
    text.push_str("\n\n");

    text.push_str("## 结果\n\n");
    text.push_str(&facts.result);
    text.push('\n');
    text.push('\n');

    if let Some(error) = facts.error {
        text.push_str("## 错误\n\n");
        text.push_str(error.trim());
        text.push_str("\n\n");
    }

    if let Some(note) = &facts.note {
        text.push_str("## 说明\n\n");
        text.push_str(note);
        text.push('\n');
    }

    text
}

/// 失联时刻不可考：按落盘时间估计，frontmatter 用 `terminal_at_estimated` 声明。
fn resolved_terminal_at(facts: &Facts<'_>) -> String {
    match (facts.estimated, facts.terminal_at) {
        (true, _) => chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        (false, Some(at)) => at.to_string(),
        (false, None) => "未知".to_string(),
    }
}

/// 标题取任务描述第一行；没有描述时退化成任务 id，标题不能空着。
fn title_of(meta: &ReportMeta) -> String {
    match first_line(meta.prompt_digest.as_deref()) {
        Some(line) => line.chars().take(TITLE_MAX_CHARS).collect::<String>(),
        None => format!("任务 {}", meta.task_id.chars().take(8).collect::<String>()),
    }
}

fn first_line(text: Option<&str>) -> Option<&str> {
    text?.lines().map(str::trim).find(|line| !line.is_empty())
}

/// 一个 YAML 标量。双引号包裹是必须的：`@master` 这类以指示符开头的值
/// 在裸标量里非法，报告就再也解析不回来了。
fn scalar(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(": \"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            // 换行会把 frontmatter 撑成多行，其余控制字符（`\x1b` 之类）会让
            // 整段不再是一个合法的 YAML 标量；一律折叠成空格。
            c if c.is_control() => out.push(' '),
            other => out.push(other),
        }
    }
    out.push_str("\"\n");
}

// ============================================================================
// 写盘
// ============================================================================

/// 同步写一份报告，返回落地路径。
///
/// 运行时的两个入口走 [`write_for_observation`] / [`write_for_row`]，把写盘挪进
/// 阻塞线程池；这一条留给需要原地落盘的场合（测试直接验证落点与失败语义）。
pub fn write(meta: &ReportMeta, body: &ReportBody<'_>) -> Result<PathBuf, ReportError> {
    let path = path_of(meta)?;
    let text = render(meta, body);
    write_at(&meta.target_dir, &path, &text)?;
    Ok(path)
}

/// 先写同目录临时文件再 `rename`：读者（文件页签、编辑器）不该看到写了一半的报告，
/// 同一任务的重复终态也只是覆盖同一路径，天然幂等。
fn write_at(target_dir: &Path, path: &Path, text: &str) -> Result<(), ReportError> {
    // `create_dir_all` 会把缺失的上层目录一并建出来 —— 空间目录已被删掉时，
    // 那等于用一份报告把它「恢复」成一个空目录。先确认它还在。
    if !target_dir.is_dir() {
        return Err(ReportError::TargetMissing(target_dir.to_path_buf()));
    }
    let dir = path
        .parent()
        .ok_or_else(|| ReportError::UnsafePath(path.to_path_buf()))?;
    fs::create_dir_all(dir)?;

    let tmp = tmp_path(path)?;
    if let Err(err) = fs::write(&tmp, text) {
        let _ = fs::remove_file(&tmp);
        return Err(err.into());
    }
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err.into());
    }
    Ok(())
}

fn tmp_path(path: &Path) -> Result<PathBuf, ReportError> {
    let name = path
        .file_name()
        .ok_or_else(|| ReportError::UnsafePath(path.to_path_buf()))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    Ok(path.with_file_name(format!(
        ".{}.tmp-{}-{nanos}",
        name.to_string_lossy(),
        std::process::id()
    )))
}

// ============================================================================
// 调用方入口：落库成功之后才写，失败只 warn
// ============================================================================

/// 运行期路径（`TaskObserver`）：手里只有观察，空间身份按目标目录反查。
///
/// 反查不到也照样写 —— 报告是交付物，不能因为台账元数据缺失就丢。
pub async fn write_for_observation(
    pool: &SqlitePool,
    obs: &DispatchObservation,
    body: ReportBody<'_>,
) {
    let (target_ws_id, team_id) = space_identity(pool, &obs.target_dir).await;
    // `prompt_digest` 先用观察自带的：台账行读得到时下面会用它覆盖，读不到才
    // 用这份兜底。两条路径若各用各的来源，同一个任务就会得到两个文件名。
    let meta = ReportMeta {
        task_id: obs.task_id.to_string(),
        dispatch_id: obs.dispatch_id.clone(),
        target_dir: obs.target_dir.clone(),
        target_ws_id,
        team_id,
        origin_ws_id: obs.origin.ws_id.clone(),
        agent: obs.agent_name.clone(),
        prompt_digest: obs.prompt_digest.clone(),
        created_at: None,
    };
    persist_from_ledger(pool, meta, body).await;
}

/// 对账与 force 关闭路径：台账行的身份列都在手里，正文与时刻仍要回读 —
/// 调用方手里的快照可能比台账旧（见 [`ReportBody::with_source`]）。
pub async fn write_for_row(pool: &SqlitePool, row: &DispatchRow, body: ReportBody<'_>) {
    let meta = ReportMeta {
        task_id: row.task_id.clone(),
        dispatch_id: row.dispatch_id.clone(),
        target_dir: PathBuf::from(&row.target_work_dir),
        target_ws_id: row.target_ws_id.clone(),
        team_id: row.target_team_id.clone(),
        origin_ws_id: row.origin_ws_id.clone(),
        agent: row.agent_name.clone(),
        prompt_digest: row.prompt_digest.clone(),
        created_at: Some(row.created_at.clone()),
    };
    persist_from_ledger(pool, meta, body).await;
}

/// 两条入口的收尾：台账有行就以台账为准，没有才退回调用方入参。
///
/// 查不到行只 warn、照写：报告是交付物，台账异常不该让它消失。
async fn persist_from_ledger(pool: &SqlitePool, meta: ReportMeta, body: ReportBody<'_>) {
    let source = match dispatches::find_report_source(pool, &meta.task_id).await {
        Ok(Some(source)) => Some(source),
        Ok(None) => {
            warn!(
                task_id = %meta.task_id,
                "dispatch ledger row is gone; the report falls back to the caller's values"
            );
            None
        }
        Err(e) => {
            warn!(
                task_id = %meta.task_id,
                error = %e,
                "cannot read the dispatch ledger row; the report falls back to the caller's values"
            );
            None
        }
    };
    let body = match &source {
        Some(source) => body.with_source(source),
        None => body,
    };
    let meta = match &source {
        Some(source) => ReportMeta {
            created_at: Some(source.created_at.clone()),
            prompt_digest: source.prompt_digest.clone(),
            ..meta
        },
        None => meta,
    };
    persist(&meta, &body).await;
}

async fn space_identity(pool: &SqlitePool, target_dir: &Path) -> (String, String) {
    let dir = target_dir.to_string_lossy().into_owned();
    let found = match workspaces::by_work_dir(pool, &dir).await {
        Ok(found) => found,
        Err(e) => {
            warn!(target_dir = %dir, error = %e, "workspace lookup failed; the report will say unknown");
            return (UNKNOWN.to_string(), UNKNOWN.to_string());
        }
    };
    match found {
        Some(row) => (row.id, row.team_id),
        None => {
            warn!(
                target_dir = %dir,
                "dispatch target has no workspace row; the report will say unknown"
            );
            (UNKNOWN.to_string(), UNKNOWN.to_string())
        }
    }
}

/// 写盘放在阻塞线程池里：一份百万字符的报告不该占住 async runtime。
async fn persist(meta: &ReportMeta, body: &ReportBody<'_>) {
    let path = match path_of(meta) {
        Ok(path) => path,
        Err(e) => {
            warn!(task_id = %meta.task_id, error = %e, "cannot resolve the dispatch report path");
            return;
        }
    };
    let text = render(meta, body);
    let shown = path.clone();
    let task_id = meta.task_id.clone();
    let target_dir = meta.target_dir.clone();
    let joined = tokio::task::spawn_blocking(move || write_at(&target_dir, &path, &text)).await;
    match joined {
        Ok(Ok(())) => {
            debug!(task_id = %task_id, path = %shown.display(), "dispatch report written")
        }
        Ok(Err(e)) => {
            warn!(task_id = %task_id, path = %shown.display(), error = %e, "cannot write the dispatch report")
        }
        Err(e) => {
            warn!(task_id = %task_id, path = %shown.display(), error = %e, "dispatch report writer did not finish")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(dir: &str, digest: Option<&str>) -> ReportMeta {
        ReportMeta {
            task_id: "aabbccdd-1111-2222-3333-444455556666".to_string(),
            dispatch_id: Some("disp-1".to_string()),
            target_dir: PathBuf::from(dir),
            target_ws_id: "ws_target".to_string(),
            team_id: "research".to_string(),
            origin_ws_id: "ws_peco".to_string(),
            agent: "@master".to_string(),
            prompt_digest: digest.map(str::to_string),
            created_at: Some("2026-11-13T08:00:00Z".to_string()),
        }
    }

    /// 只关心 frontmatter 的用例用它：正文内容与那条断言无关。
    fn no_body() -> ReportBody<'static> {
        ReportBody::Terminal {
            state: "completed",
            terminal_at: None,
            output: None,
            digest: None,
            output_truncated: false,
            error: None,
        }
    }

    /// frontmatter 的键值行；逐行断言就够用，不为此引入 YAML 解析依赖。
    fn frontmatter(text: &str) -> Vec<&str> {
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("---"), "frontmatter 必须由 `---` 开围");
        let mut fields = Vec::new();
        for line in lines {
            if line == "---" {
                return fields;
            }
            fields.push(line);
        }
        panic!("frontmatter 没有闭合：{text}");
    }

    fn field<'a>(fields: &[&'a str], key: &str) -> &'a str {
        let prefix = format!("{key}: ");
        fields
            .iter()
            .find_map(|line| line.strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("缺少字段 {key}：{fields:?}"))
    }

    // ---- slug_of ----------------------------------------------------------

    #[test]
    fn slug_keeps_chinese() {
        assert_eq!(slug_of(Some("重构用户登录流程")), "重构用户登录流程");
    }

    /// `../` 与 `/` 是这条清洗规则存在的全部理由：文件名进的是真实文件系统。
    #[test]
    fn slug_never_carries_path_separators() {
        let slug = slug_of(Some("../../etc/passwd"));
        assert!(!slug.contains('/'), "{slug}");
        assert!(!slug.contains('\\'), "{slug}");
        assert!(!slug.contains(".."), "{slug}");
        assert_eq!(slug, "etc-passwd");
    }

    #[test]
    fn slug_truncates_to_24_chars() {
        let slug = slug_of(Some(&"长".repeat(60)));
        assert_eq!(slug.chars().count(), SLUG_MAX_CHARS);
    }

    #[test]
    fn slug_of_punctuation_is_task() {
        assert_eq!(slug_of(Some("!!! ??? …")), "task");
    }

    #[test]
    fn slug_of_nothing_is_task() {
        assert_eq!(slug_of(None), "task");
        assert_eq!(slug_of(Some("")), "task");
        assert_eq!(slug_of(Some("   \n\t\n")), "task");
    }

    #[test]
    fn slug_never_starts_with_a_dot() {
        assert_eq!(slug_of(Some(".hidden")), "hidden");
    }

    #[test]
    fn slug_merges_and_trims_dashes() {
        assert_eq!(slug_of(Some("  a -- b__c  ")), "a-b__c");
    }

    /// 描述是多行的：取第一行，后面的「细节」不该出现在文件名里。
    #[test]
    fn slug_uses_the_first_non_empty_line() {
        assert_eq!(slug_of(Some("\n\n 修复 bug\n请先跑测试")), "修复-bug");
    }

    // ---- path_of ----------------------------------------------------------

    #[test]
    fn report_paths_land_in_the_output_dispatch_directory() {
        let meta = meta("/tmp/spaces/research/work", Some("修复登录"));
        let path = path_of(&meta).unwrap();

        assert_eq!(
            path.parent().unwrap(),
            Path::new("/tmp/spaces/research/work/output/dispatch")
        );
        assert!(
            path.starts_with(&meta.target_dir),
            "报告必须留在目标空间目录里：{}",
            path.display()
        );
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "修复登录__aabbccdd.md",
            "文件名只由任务描述与 task_id 决定"
        );
    }

    /// task_id 不是我们能信的输入：前 8 位不是十六进制就必须拒绝，而不是截一小段
    /// 就拼进文件名 —— `abcd/...` 里的 `..` 足以让路径逃出报告目录。
    #[test]
    fn a_task_id_whose_prefix_is_not_hex_is_refused() {
        for task_id in [
            "../../../etc/passwd",
            "abcd/../../../etc/passwd",
            "not-a-task-id",
            "abcd",
            "ＡＢＣＤＥＦＧＨ",
        ] {
            let mut meta = meta("/tmp/spaces/research/work", Some("任务"));
            meta.task_id = task_id.to_string();

            assert!(
                matches!(path_of(&meta), Err(ReportError::UnsafeTaskId(_))),
                "{task_id} 不该被当成合法文件名：{:?}",
                path_of(&meta)
            );
        }
    }

    /// 落点必须是「`output/dispatch` 下恰好一层普通文件」：`starts_with` 恒真、
    /// `file_name()` 只看最后一段，两者都拦不住 `a/../../x.md` 这种写法。
    #[test]
    fn the_guard_only_accepts_a_flat_file_under_the_report_directory() {
        let target = Path::new("/tmp/spaces/research/work");

        for ok in [
            "/tmp/spaces/research/work/output/dispatch/任务__aabbccdd.md",
            "/tmp/spaces/research/work/output/dispatch/task.md",
        ] {
            assert!(guard(Path::new(ok), target).is_ok(), "{ok} 应当合法");
        }
        for bad in [
            // 报告目录之外
            "/tmp/spaces/research/work/task.md",
            // 深一层
            "/tmp/spaces/research/work/output/dispatch/sub/task.md",
            // 用 `..` 回到目录内，最后一段依然干净
            "/tmp/spaces/research/work/output/dispatch/a/../../task.md",
            // 完全是另一个目录
            "/tmp/elsewhere/output/dispatch/task.md",
        ] {
            assert!(
                matches!(
                    guard(Path::new(bad), target),
                    Err(ReportError::UnsafePath(_))
                ),
                "{bad} 应当被拒绝"
            );
        }
    }

    // ---- render ----------------------------------------------------------

    #[test]
    fn a_full_body_report_says_it_is_complete() {
        let meta = meta(
            "/tmp/spaces/research/work",
            Some("重构登录流程\n细节不进文件名"),
        );
        let body = format!("{}{}", "正文".repeat(400), "TAIL");
        let digest: String = body.chars().take(10).collect();

        let text = render(
            &meta,
            &ReportBody::Terminal {
                state: "completed",
                terminal_at: Some("2026-11-13T08:05:00Z"),
                output: Some(&body),
                digest: Some(&digest),
                output_truncated: false,
                error: None,
            },
        );

        let fields = frontmatter(&text);
        assert_eq!(
            field(&fields, "task_id"),
            "\"aabbccdd-1111-2222-3333-444455556666\""
        );
        assert_eq!(field(&fields, "dispatch_id"), "\"disp-1\"");
        assert_eq!(field(&fields, "space"), "\"ws_target\"");
        assert_eq!(field(&fields, "team"), "\"research\"");
        assert_eq!(field(&fields, "origin_space"), "\"ws_peco\"");
        assert_eq!(field(&fields, "agent"), "\"@master\"");
        assert_eq!(field(&fields, "state"), "\"completed\"");
        assert_eq!(field(&fields, "created_at"), "\"2026-11-13T08:00:00Z\"");
        assert_eq!(field(&fields, "terminal_at"), "\"2026-11-13T08:05:00Z\"");
        assert_eq!(field(&fields, "retention"), "full");
        assert_eq!(
            field(&fields, "output_chars"),
            body.chars().count().to_string()
        );
        assert_eq!(field(&fields, "output_truncated"), "false");
        assert_eq!(field(&fields, "report_version"), "1");
        assert!(
            !fields
                .iter()
                .any(|line| line.starts_with("terminal_at_estimated")),
            "终态时刻是台账给的，不该标估计：{fields:?}"
        );

        let title = text
            .lines()
            .find(|line| line.starts_with("# "))
            .expect("报告必须有标题");
        assert_eq!(title, "# 重构登录流程", "标题只取描述第一行");
        assert!(text.contains(&body), "正文必须逐字留下");
        assert!(text.contains("## 派发指令"), "派发内容要能追溯");
        assert!(!text.contains("全文未留存"), "有全文就不得声明缺全文");
        assert!(!text.contains("## 错误"), "没有错误就不该有空段落");
    }

    /// 摘要不是全文：这一段的存在意义就是把「原文已不可考」写在读者眼前。
    #[test]
    fn a_digest_only_report_admits_the_full_body_is_gone() {
        let meta = meta("/tmp/spaces/research/work", None);
        let text = render(
            &meta,
            &ReportBody::Terminal {
                state: "completed",
                terminal_at: None,
                output: None,
                digest: Some("12 个测试通过"),
                output_truncated: false,
                error: None,
            },
        );

        let fields = frontmatter(&text);
        assert_eq!(field(&fields, "retention"), "digest_only");
        assert_eq!(field(&fields, "output_truncated"), "true");
        assert_eq!(field(&fields, "terminal_at"), "\"未知\"");
        assert!(text.contains("12 个测试通过"));
        assert!(text.contains("全文未留存"), "{text}");
        assert!(text.contains("原文长度不可考"), "{text}");
    }

    #[test]
    fn a_report_without_any_body_says_so() {
        let meta = meta("/tmp/spaces/research/work", Some("跑一下测试"));
        let text = render(
            &meta,
            &ReportBody::Terminal {
                state: "failed",
                terminal_at: Some("2026-11-13T08:05:00Z"),
                output: None,
                digest: None,
                output_truncated: false,
                error: Some("model provider returned 500"),
            },
        );

        let fields = frontmatter(&text);
        assert_eq!(field(&fields, "retention"), "none");
        assert_eq!(field(&fields, "output_chars"), "0");
        assert!(text.contains("无输出（state=`failed`）"), "{text}");
        assert!(text.contains("## 错误"), "错误段必须写出来");
        assert!(text.contains("model provider returned 500"));
        assert!(!text.contains("全文未留存"), "空正文不是「摘要」");
    }

    #[test]
    fn an_orphan_report_states_the_reason_and_nothing_was_recovered() {
        let meta = meta("/tmp/spaces/research/work", Some("跑一下测试"));
        let text = render(
            &meta,
            &ReportBody::Orphan {
                reason: "target space is not open in this process",
                terminal_at: None,
            },
        );

        let fields = frontmatter(&text);
        assert_eq!(field(&fields, "state"), "\"orphan\"");
        assert_eq!(field(&fields, "retention"), "none");
        assert_eq!(field(&fields, "output_truncated"), "false");
        assert_eq!(
            field(&fields, "terminal_at_estimated"),
            "true",
            "失联时刻是落盘时估计的，必须标出来"
        );
        assert!(text.contains("任务失联，无结果可回捞"), "{text}");
        assert!(
            text.contains("target space is not open in this process"),
            "{text}"
        );
    }

    /// 超上限时只留头部并显式标注：读者得知道长度不是原文总长。
    ///
    /// 提示语不能说「完整正文见台账」—— 台账用的是同一个上限，它也没有全文。
    #[test]
    fn an_over_long_body_is_truncated_and_labelled() {
        let meta = meta("/tmp/spaces/research/work", None);
        let body = format!("{}尾", "正".repeat(MAX_PERSISTED_OUTPUT_CHARS));

        let text = render(
            &meta,
            &ReportBody::Terminal {
                state: "completed",
                terminal_at: Some("2026-11-13T08:05:00Z"),
                output: Some(&body),
                digest: None,
                output_truncated: false,
                error: None,
            },
        );

        let fields = frontmatter(&text);
        assert_eq!(
            field(&fields, "output_chars"),
            MAX_PERSISTED_OUTPUT_CHARS.to_string()
        );
        assert_eq!(field(&fields, "output_truncated"), "true");
        assert!(
            text.contains("台账同样只保留前 1,000,000 字符，缺失部分不再可得"),
            "提示必须说清台账也没有全文：{text}"
        );
        assert!(!text.contains("完整正文见台账"), "台账里没有完整正文");
        assert!(!text.contains('尾'), "超限的尾巴不得写进报告");
    }

    /// 台账在上限处截过的那一刀同样要标出来：正文正好等于上限时，报告里这段
    /// 已经不是原文全部，而单看长度看不出来。
    #[test]
    fn a_body_truncated_by_the_ledger_is_labelled_even_at_the_limit() {
        let meta = meta("/tmp/spaces/research/work", None);
        let body = "正".repeat(MAX_PERSISTED_OUTPUT_CHARS);

        let text = render(
            &meta,
            &ReportBody::Terminal {
                state: "completed",
                terminal_at: None,
                output: Some(&body),
                digest: None,
                output_truncated: true,
                error: None,
            },
        );

        let fields = frontmatter(&text);
        assert_eq!(
            field(&fields, "output_chars"),
            MAX_PERSISTED_OUTPUT_CHARS.to_string()
        );
        assert_eq!(
            field(&fields, "output_truncated"),
            "true",
            "台账标志必须传下来"
        );
        assert!(text.contains("缺失部分不再可得"), "{text}");
    }

    /// 提示文案里的字数要读得出来：`1000000` 数不清是几位。
    #[test]
    fn the_truncation_notice_groups_the_ledger_limit() {
        assert_eq!(grouped(MAX_PERSISTED_OUTPUT_CHARS), "1,000,000");
        assert_eq!(grouped(512), "512");
        assert_eq!(grouped(1234567), "1,234,567");
    }

    /// 值里的换行会把 frontmatter 撑成多行：折叠成空格，引号必须转义。
    ///
    /// 这条只断言「一个字段占一行、引号自洽」，不断言整份文件能被 YAML 解析
    /// —— 那件事没有测过，测试名也就不该声称。
    #[test]
    fn frontmatter_values_fold_newlines_and_escape_quotes() {
        let mut meta = meta("/tmp/spaces/research/work", None);
        meta.agent = "@master \"quoted\"\nnext-line".to_string();

        let text = render(&meta, &no_body());

        let fields = frontmatter(&text);
        assert_eq!(
            field(&fields, "agent"),
            "\"@master \\\"quoted\\\" next-line\""
        );
    }

    /// 控制字符会让整段 frontmatter 不再是合法 YAML（`\x1b` 甚至能改写终端），
    /// 一律折叠成空格。
    #[test]
    fn frontmatter_scalars_drop_control_characters() {
        let mut meta = meta("/tmp/spaces/research/work", None);
        meta.agent = "@master\u{1b}[31mred\u{7}bell\u{0}nul".to_string();

        let text = render(&meta, &no_body());

        let agent = field(&frontmatter(&text), "agent");
        assert!(
            !agent.chars().any(char::is_control),
            "frontmatter 不得留下控制字符：{agent:?}"
        );
        assert_eq!(agent, "\"@master [31mred bell nul\"");
    }
}
