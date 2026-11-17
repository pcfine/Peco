#!/usr/bin/env bash
# ============================================================================
# peco-daemon 部署（systemd --user）
# ============================================================================
# 用法:
#   bash scripts/deploy-daemon.sh check [--takeover] [--allow-home-change]
#   bash scripts/deploy-daemon.sh install [--takeover] [--no-build] [--allow-home-change]
#   bash scripts/deploy-daemon.sh status
#   bash scripts/deploy-daemon.sh restart
#   bash scripts/deploy-daemon.sh refresh-env
#   bash scripts/deploy-daemon.sh uninstall [--purge-bin] [--purge-data]
#
# 全部路径都是「环境变量覆盖 + 默认值」，仓库里不存任何绝对家目录常量：
#   PECO_DAEMON_HOME       数据根（默认 $HOME/<HOME_DIR_NAME>，目录名从源码常量解析）
#   PECO_DAEMON_BIN        二进制（默认 $HOME/.local/bin/peco-daemon）
#   PECO_WEBUI_DIST        前端产物（默认 $REPO_DIR/peco-webui/dist；显式置空 = 关掉静态层）
#   PECO_DAEMON_LOG_DIR    日志目录（默认空 = journal）
#   PECO_DAEMON_UNIT_DIR   unit 目录（默认 ${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user）
#   PECO_DAEMON_BIND_ADDR  监听地址（默认空 = 二进制内置 127.0.0.1:6971）
#                          只认 host:port 且 host 必须是 loopback；IPv6 写成 [::1]:6971。
#                          该 daemon 只服务本机单用户、接口无鉴权，故不做 0.0.0.0 监听
#   PECO_LOG               日志级别（默认空 = 二进制内置默认）
#   PECO_DAEMON_WORKDIR    工作目录（默认 $REPO_DIR）
#   PECO_DAEMON_SERVICE    unit 名（默认 peco-daemon）
#   PECO_DAEMON_SKIP_BUILD 1 = 跳过构建
#   PECO_DAEMON_ALLOW_PUBLIC 1 = 允许非 loopback 监听（默认拒绝，需要时自己承担风险）
#   PECO_DAEMON_CONFIRM_PURGE 1 = 允许 uninstall --purge-data
#
# 调用者的代理环境（HTTP_PROXY / HTTPS_PROXY / ALL_PROXY / NO_PROXY，大小写两种）
# 会像 PATH 一样被照抄进 unit：systemd --user 的环境是干净的，而本机出网往往要
# 走登录 shell 里才有的本地代理；缺了它 daemon 只会得到
# 「error sending request for url (.../v1/messages)」——没有状态码，也没有原因。
#
# install 重跑时以已装 unit 里的 Environment 行为基线：当前 shell 没显式给对应
# 变量就沿用基线值；数据根真的变了必须显式 --allow-home-change，否则拒绝。
# ============================================================================

set -euo pipefail

trap 'printf "[FATAL] deploy-daemon.sh failed at line %s\n" "$LINENO" >&2' ERR

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; BOLD='\033[1m'; NC='\033[0m'
log_info()  { printf "  ${GREEN}[INFO]${NC} %s\n" "$*"; }
log_warn()  { printf "  ${YELLOW}[WARN]${NC} %s\n" "$*"; }
log_error() { printf "  ${RED}[ERROR]${NC} %s\n" "$*"; }
die()       { log_error "$*"; exit 1; }
step()      { printf "\n${BOLD}${BLUE}[%s]${NC}\n" "$*"; }

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_SRC="$REPO_DIR/crates/peco-daemon/src/config.rs"

# 与 crates/peco-daemon/src/config.rs 的 DEFAULT_BIND_ADDR 同步。只在
# PECO_DAEMON_BIND_ADDR 未给时用作探针与保护端口判定的地址，故这份小重复比
# 再引入一条解析规则划算。
DEFAULT_BIND="127.0.0.1:6971"

# 既有服务占着的端口：任何时候都不接管，--takeover 也不例外。
PROTECTED_PORTS=(9227 6969 6970 9233)

# ── 子命令与参数 ──────────────────────────────────────────────────────────
usage() {
  awk 'NR < 2 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "${BASH_SOURCE[0]}"
}

COMMAND="${1:-}"
[[ -n "$COMMAND" ]] || { usage >&2; exit 2; }
case "$COMMAND" in
  -h|--help|help) usage; exit 0 ;;
esac
shift

TAKEOVER=0
NO_BUILD=0
PURGE_BIN=0
PURGE_DATA=0
ALLOW_HOME_CHANGE=0
PARSE_TARGET=""

case "$COMMAND" in
  install)
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --takeover) TAKEOVER=1; shift ;;
        --no-build) NO_BUILD=1; shift ;;
        --allow-home-change) ALLOW_HOME_CHANGE=1; shift ;;
        *) die "unknown option for install: $1" ;;
      esac
    done
    ;;
  check)
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --takeover) TAKEOVER=1; shift ;;
        --allow-home-change) ALLOW_HOME_CHANGE=1; shift ;;
        *) die "unknown option for check: $1" ;;
      esac
    done
    ;;
  uninstall)
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --purge-bin) PURGE_BIN=1; shift ;;
        --purge-data) PURGE_DATA=1; shift ;;
        *) die "unknown option for uninstall: $1" ;;
      esac
    done
    ;;
  status|restart|refresh-env)
    [[ $# -eq 0 ]] || die "unknown option for $COMMAND: $1"
    ;;
  _parse-home)
    [[ $# -eq 1 ]] || die "usage: _parse-home <file>"
    PARSE_TARGET="$1"
    ;;
  *)
    die "unknown command: $COMMAND (try --help)"
    ;;
esac

# ── 从源码常量解析数据根目录名 ────────────────────────────────────────────
# 允许 pub / pub(...) / 行尾注释。用 `|` 作分隔符：行尾注释里的 `//` 会打断
# `/` 分隔的 s 命令。
home_dir_name_hits() {
  sed -nE \
    's|^[[:space:]]*(pub(\([a-z:]+\))?[[:space:]]+)?const[[:space:]]+HOME_DIR_NAME[[:space:]]*:[[:space:]]*&str[[:space:]]*=[[:space:]]*"([^"]*)";[[:space:]]*(//.*)?$|\3|p' \
    "$1"
}

# 命中必须恰好一处：0 处是格式契约被破坏，2 处是两份真源 —— 两种都不猜，直接失败。
require_home_dir_name() {
  local file="$1" hits count
  [[ -f "$file" ]] || die "config source not found: $file"
  hits="$(home_dir_name_hits "$file")"
  [[ -n "$hits" ]] || die "HOME_DIR_NAME not found in $file"
  count="$(printf '%s\n' "$hits" | wc -l)"
  [[ "$count" -eq 1 ]] || die "ambiguous HOME_DIR_NAME: $count matches in $file"
  printf '%s\n' "$hits"
}

if [[ "$COMMAND" == "_parse-home" ]]; then
  require_home_dir_name "$PARSE_TARGET"
  exit 0
fi

# ── 生效变量 ──────────────────────────────────────────────────────────────
# $1 = 1：先问运行中的 daemon（status/uninstall）
# $2 = 1：已装 unit 的 Environment 行作为基线（install/check/refresh-env/restart）
resolve_paths() {
  local api_first="${1:-0}" use_baseline="${2:-0}"
  [[ -n "${HOME:-}" ]] || die "HOME is unset"

  SERVICE="${PECO_DAEMON_SERVICE:-peco-daemon}"
  BIN="${PECO_DAEMON_BIN:-$HOME/.local/bin/peco-daemon}"
  UNIT_DIR="${PECO_DAEMON_UNIT_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user}"
  UNIT="$UNIT_DIR/$SERVICE.service"
  WORK_DIR="${PECO_DAEMON_WORKDIR:-$REPO_DIR}"
  LOG_DIR="${PECO_DAEMON_LOG_DIR:-}"

  # 已装 unit 是「上一次 install 的决定」的唯一记录。裸着跑 refresh-env 或重跑
  # install 时读不到基线，脚本就会拿自己的默认值重算：数据根静默换人、静态层
  # 被悄悄打开，而健康检查与数据根校验用的还是同一套「新」输入，全绿无红灯。
  UNIT_BASELINE_HOME=""
  UNIT_BASELINE_DATA_ROOT=""
  BASELINE_FROM_UNIT=0
  local baseline_bind="" baseline_log="" baseline_dist=""
  if [[ "$use_baseline" == "1" && -f "$UNIT" ]]; then
    BASELINE_FROM_UNIT=1
    UNIT_BASELINE_HOME="$(unit_env_value PECO_HOME)"
    baseline_bind="$(unit_env_value PECO_DAEMON_BIND_ADDR)"
    baseline_log="$(unit_env_value PECO_LOG)"
    baseline_dist="$(unit_env_value PECO_WEBUI_DIST)"
  fi

  # 常量是数据根的唯一真源，也是脚本的格式契约，故默认必解析；观测优先的
  # status/uninstall 例外 —— 它们不该因为仓库搬走就卸不掉服务。
  HOME_DIR_NAME=""
  if [[ "$api_first" != "1" ]]; then
    HOME_DIR_NAME="$(require_home_dir_name "$CONFIG_SRC")"
  fi

  WRITE_BIND=0
  if [[ -n "${PECO_DAEMON_BIND_ADDR+set}" ]]; then
    if [[ -n "${PECO_DAEMON_BIND_ADDR}" ]]; then
      BIND_ADDR="$PECO_DAEMON_BIND_ADDR"
      WRITE_BIND=1
    else
      BIND_ADDR="$DEFAULT_BIND"
    fi
  elif [[ -n "$baseline_bind" ]]; then
    BIND_ADDR="$baseline_bind"
    WRITE_BIND=1
  else
    BIND_ADDR="$DEFAULT_BIND"
  fi
  PROBE_URL="http://$BIND_ADDR/api/peco"

  if [[ -n "${PECO_LOG+set}" ]]; then
    LOG_LEVEL="${PECO_LOG}"
  elif [[ -n "$baseline_log" ]]; then
    LOG_LEVEL="$baseline_log"
  else
    LOG_LEVEL=""
  fi

  # 显式置空是唯一的「关掉静态层」方式；基线里本来就没有这行时同样保持关闭，
  # 免得一次裸 install 把它打开。
  if [[ -n "${PECO_WEBUI_DIST+set}" ]]; then
    if [[ -z "${PECO_WEBUI_DIST}" ]]; then
      WEBUI_DIST=""
      WEBUI_DIST_DISABLED=1
    else
      WEBUI_DIST="$PECO_WEBUI_DIST"
      WEBUI_DIST_DISABLED=0
    fi
  elif [[ "$BASELINE_FROM_UNIT" == "1" ]]; then
    if [[ -n "$baseline_dist" ]]; then
      WEBUI_DIST="$baseline_dist"
      WEBUI_DIST_DISABLED=0
    else
      WEBUI_DIST=""
      WEBUI_DIST_DISABLED=1
    fi
  else
    WEBUI_DIST="$REPO_DIR/peco-webui/dist"
    WEBUI_DIST_DISABLED=0
  fi

  # 数据根：运行态观测 > PECO_DAEMON_HOME > 已装 unit 的基线 > 源码常量默认值。
  OBSERVED_WORK_DIR=""
  local observed="" observed_ok=0
  if [[ "$api_first" == "1" ]]; then
    if OBSERVED_WORK_DIR="$(api_work_dir)"; then
      observed="$(home_from_work_dir "$OBSERVED_WORK_DIR" || true)"
      observed_ok=1
    fi
  fi

  WRITE_PECO_HOME=0
  if [[ "$observed_ok" == "1" ]]; then
    DATA_HOME="$observed"
    DATA_HOME_SOURCE="/api/peco work_dir"
    if [[ -z "$observed" ]]; then
      log_warn "/api/peco answered work_dir '$OBSERVED_WORK_DIR', which is not <data-root>/spaces/<team>/work"
    fi
  elif [[ -n "${PECO_DAEMON_HOME:-}" ]]; then
    DATA_HOME="$PECO_DAEMON_HOME"
    DATA_HOME_SOURCE="PECO_DAEMON_HOME"
    WRITE_PECO_HOME=1
  elif [[ -n "$UNIT_BASELINE_HOME" ]]; then
    DATA_HOME="$UNIT_BASELINE_HOME"
    DATA_HOME_SOURCE="installed unit PECO_HOME"
    WRITE_PECO_HOME=1
  else
    # 观测优先的路径（status/uninstall）只在真的要用默认值时才解析常量：
    # 仓库搬走之后它们仍要能把服务卸掉。
    if [[ -z "$HOME_DIR_NAME" ]]; then
      HOME_DIR_NAME="$(require_home_dir_name "$CONFIG_SRC")"
    fi
    DATA_HOME="$HOME/$HOME_DIR_NAME"
    DATA_HOME_SOURCE="HOME_DIR_NAME=$HOME_DIR_NAME"
  fi

  if [[ "$BASELINE_FROM_UNIT" == "1" ]]; then
    # 基线里的数据根：没有 PECO_HOME 行时就是 daemon 自己的默认值。
    UNIT_BASELINE_DATA_ROOT="$(realpath -m -- "${UNIT_BASELINE_HOME:-$HOME/$HOME_DIR_NAME}")"
  fi

  if [[ "$NO_BUILD" == "1" ]]; then
    SKIP_BUILD=1
  else
    SKIP_BUILD="${PECO_DAEMON_SKIP_BUILD:-0}"
  fi

  require_absolute "unit directory" "$UNIT_DIR"
  require_absolute "binary path" "$BIN"
  require_absolute "work directory" "$WORK_DIR"
  if [[ "$WEBUI_DIST_DISABLED" != "1" ]]; then
    require_absolute "PECO_WEBUI_DIST" "$WEBUI_DIST"
  fi
  if [[ -n "$LOG_DIR" ]]; then
    require_absolute "PECO_DAEMON_LOG_DIR" "$LOG_DIR"
  fi

  PATH_SNAPSHOT="$(snapshot_path)"
  PROXY_ENV_SNAPSHOT="$(snapshot_proxy_env)"
}

# unit 里某个 Environment= 的值。systemd 语义是后写的覆盖先写的，故取最后一行。
unit_env_value() {
  local key="$1"
  sed -nE "s/^Environment=\"${key}=(.*)\"$/\1/p" "$UNIT" 2>/dev/null | tail -n 1
}

print_vars() {
  printf '  Repo:          %s\n' "$REPO_DIR"
  printf '  Script:        %s\n' "${BASH_SOURCE[0]}"
  printf '  Service:       %s\n' "$SERVICE"
  printf '  Unit:          %s\n' "$UNIT"
  printf '  Binary:        %s\n' "$BIN"
  printf '  Data root:     %s   (from %s)\n' "$DATA_HOME" "$DATA_HOME_SOURCE"
  if [[ "$WEBUI_DIST_DISABLED" == "1" ]]; then
    printf '  Webui dist:    <disabled>\n'
  else
    printf '  Webui dist:    %s\n' "$WEBUI_DIST"
  fi
  printf '  Bind:          %s\n' "$BIND_ADDR"
  printf '  Work dir:      %s\n' "$WORK_DIR"
  printf '  Logs:          %s\n' "${LOG_DIR:-journal}"
  printf '  Log level:     %s\n' "${LOG_LEVEL:-<binary default>}"
  printf '  PATH items:    %s\n' "$(tr ':' '\n' <<<"$PATH_SNAPSHOT" | wc -l)"
  printf '  Proxy vars:    %s\n' "$(grep -c '^Environment=' <<<"${PROXY_ENV_SNAPSHOT:-}" || true)"
  printf '  Skip build:    %s\n' "$SKIP_BUILD"
}

# ── PATH 快照 ─────────────────────────────────────────────────────────────
# systemd --user 的默认 PATH 不含 ~/.cargo/bin、~/.local/bin、~/.npm-global/bin，
# 而 agent 的 sh -c 与 MCP 子进程都继承 daemon 的 PATH，所以照抄当前 shell。
# `Environment=` 不做变量/specifier 展开：这里必须写展开后的绝对路径，`%` 也不转义。
snapshot_path() {
  local raw="${PATH:-}" item out=""
  local -A already=()
  [[ -n "$raw" ]] || die "PATH is empty"
  local IFS=':'
  for item in $raw; do
    [[ -n "$item" ]] || continue
    case "$item" in
      *'"'*|*'\'*|*$'\n'*) die "PATH item would break the unit file syntax: $item" ;;
    esac
    [[ -n "${already[$item]:-}" ]] && continue
    already[$item]=1
    out="${out:+$out:}$item"
  done
  unset IFS
  [[ -n "$out" ]] || die "PATH is empty"
  printf '%s' "$out"
}

# ── 代理环境快照 ───────────────────────────────────────────────────────────
# 与 PATH 同一性质：值只在调用者的 shell 里，systemd --user 的 unit 环境没有。
# 用白名单而不是整个 env —— `Environment=` 会把值原样落进 unit 文件，
# 无关的凭据与路径不该被顺手复制进去。
PROXY_ENV_KEYS=(
  HTTP_PROXY HTTPS_PROXY ALL_PROXY NO_PROXY
  http_proxy https_proxy all_proxy no_proxy
)

# 每行一条 `Environment="KEY=VALUE"`。调用者没给该变量时沿用已装 unit 的基线，
# 与 PATH 一致：重跑 install 不带代理变量，不该把已装好的代理抹掉。
snapshot_proxy_env() {
  local key value out=""
  for key in "${PROXY_ENV_KEYS[@]}"; do
    value="$(printenv "$key" 2>/dev/null || true)"
    [[ -n "$value" ]] || value="$(unit_env_value "$key")"
    [[ -n "$value" ]] || continue
    case "$value" in
      *'"'*|*'\\'*|*$'\n'*) die "$key would break the unit file syntax: $value" ;;
    esac
    out+="Environment=\"$key=$value\""$'\n'
  done
  printf '%s' "$out"
}

# unit 里现有的代理行（原样、按出现顺序），用于 refresh-env 的「无变化」判定。
proxy_lines_of_unit() {
  grep -E '^Environment="(HTTPS?_PROXY|ALL_PROXY|NO_PROXY|https?_proxy|all_proxy|no_proxy)=' \
    "$UNIT" 2>/dev/null || true
}

# ── 端口 ──────────────────────────────────────────────────────────────────
port_of() { printf '%s' "${1##*:}"; }

# bind 的 host 部分（IPv6 形如 [::1]）。
host_of() { printf '%s' "${1%:*}"; }

# 按端口判定，不按地址字符串：「占着 0.0.0.0:6971 / *:6971 / [::]:6971」与
# 「占着 127.0.0.1:6971」对本进程是同一件事，按地址比对会漏判成「端口空闲」。
port_line() {
  local port="$1"
  ss -ltnp 2>/dev/null | awk -v p="$port" '
    NR > 1 { n = split($4, field, ":"); if (field[n] == p) { print; exit } }'
}

port_owner_pid() {
  local line
  line="$(port_line "$1")"
  [[ -n "$line" ]] || return 0
  sed -nE 's/.*pid=([0-9]+).*/\1/p' <<<"$line"
}

port_is_listening() { [[ -n "$(port_line "$1")" ]]; }

# 端口号要能被 ss 与保护端口清单比对：格式不对就现在失败，别在准入阶段假装「端口空闲」。
# IPv6 只接受带方括号的写法（[::1]:6971）—— 与 ss 本地地址列的写法一致，也没有
# 「哪个冒号是端口分隔符」的歧义。
validate_bind_addr() {
  local bind="$1" port
  [[ "$bind" =~ ^(\[[0-9A-Fa-f:.]+\]|[^:]+):[0-9]+$ ]] \
    || die "invalid bind address '$bind' (expected host:port; write IPv6 as [::1]:6971)"
  port="$(port_of "$bind")"
  (( port >= 1 && port <= 65535 )) || die "invalid port in bind address '$bind'"
}

# 只允许 loopback：这个 daemon 是单用户本机服务，/api 没有远程鉴权。
is_loopback_host() {
  case "$1" in
    localhost|127.*|\[::1\]|::1) return 0 ;;
    *) return 1 ;;
  esac
}

refuse_public_bind() {
  local bind="$1"
  if is_loopback_host "$(host_of "$bind")"; then
    return 0
  fi
  if [[ "${PECO_DAEMON_ALLOW_PUBLIC:-0}" == "1" ]]; then
    log_warn "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
    log_warn " PECO_DAEMON_BIND_ADDR=$bind is NOT loopback: /api has no remote auth,"
    log_warn " anything that can reach this port can drive the daemon and its files."
    log_warn " Accepted only because PECO_DAEMON_ALLOW_PUBLIC=1."
    log_warn "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
    return 0
  fi
  die "refusing to bind $bind: only loopback addresses are supported (use 127.0.0.1:<port> or [::1]:<port>); set PECO_DAEMON_ALLOW_PUBLIC=1 if you really need a public bind"
}

# 写进 unit 的路径必须已经是绝对路径：`Environment=` 与 `StandardOutput=append:` 都不做
# 变量/specifier 展开，相对路径要么原样变成字面量，要么被 systemd 拒启动。
require_absolute() {
  local name="$1" value="$2"
  [[ "$value" == /* ]] || die "$name must be an absolute path: $value"
}

refuse_protected_port() {
  local bind="$1" port
  port="$(port_of "$bind")"
  local p
  for p in "${PROTECTED_PORTS[@]}"; do
    if [[ "$port" == "$p" ]]; then
      die "refusing to bind protected port $p (owned by an existing service); pick another PECO_DAEMON_BIND_ADDR"
    fi
  done
}

# 与 bind 端口无关的第二道保险：待接管的 pid 若同时监听保护端口，说明认错了人。
refuse_protected_owner() {
  local pid="$1" p
  local listeners
  listeners="$(ss -ltnp 2>/dev/null | grep -F "pid=$pid," || true)"
  [[ -n "$listeners" ]] || return 0
  for p in "${PROTECTED_PORTS[@]}"; do
    local pattern=":${p} "
    if grep -qF -- "$pattern" <<<"$listeners"; then
      die "refusing to take over pid $pid: it also listens on protected port $p"
    fi
  done
}

# 三项同时成立才认作可接管的 daemon：exe 名、cmdline、端口属主。
# 任一不符就交回人工 —— 只发 SIGTERM，从不 pkill（会命中 peco-server）、从不 SIGKILL。
verify_takeover_victim() {
  local bind="$1" pid="$2" exe owner
  [[ -n "$pid" ]] || die "port $bind is occupied but the owner pid is not visible; inspect with: sudo ss -ltnp"
  exe="$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)"
  # 升级场景：二进制被替换后，内核在 exe 链接尾巴加 " (deleted)"。
  # 这正是「换掉旧二进制再接管旧进程」的常态，必须剥掉后缀再比对。
  local exe_name
  exe_name="$(basename "${exe:-missing}" | sed 's/ (deleted)$//')"
  if [[ "$exe_name" != "peco-daemon" ]]; then
    die "refusing to take over pid $pid on $bind: executable is '${exe:-<unknown>}', not peco-daemon. Stop it yourself if it is yours."
  fi
  if ! tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null | grep -q 'peco-daemon'; then
    die "refusing to take over pid $pid on $bind: its command line does not mention peco-daemon. Stop it yourself if it is yours."
  fi
  owner="$(port_owner_pid "$(port_of "$bind")")"
  if [[ "$owner" != "$pid" ]]; then
    die "refusing to take over pid $pid: $bind is owned by '${owner:-<unknown>}'. Stop it yourself if it is yours."
  fi
  refuse_protected_owner "$pid"
}

# ── unit 渲染 ─────────────────────────────────────────────────────────────
# 只有 install 用它整篇渲染；refresh-env 只换 PATH 那一行，绝不重算别的字段。
render_unit() {
  local exec_start="$BIN"
  if [[ "$BIN" == "$HOME"/* ]]; then
    # `%h` 只在 ExecStart 这类会被展开的字段里用，Environment= 不展开。
    exec_start="%h/${BIN#"$HOME"/}"
  fi

  printf '[Unit]\n'
  printf 'Description=Peco daemon (multi-workspace, local single user)\n'
  printf '\n[Service]\n'
  printf 'Type=simple\n'
  printf 'ExecStart=%s\n' "$exec_start"
  printf 'WorkingDirectory=%s\n' "$WORK_DIR"
  printf 'Restart=on-failure\n'
  printf 'RestartSec=3\n'
  printf 'TimeoutStopSec=30\n'
  printf 'KillSignal=SIGTERM\n'
  printf 'Environment="PATH=%s"\n' "$PATH_SNAPSHOT"
  # 命令替换会剥掉末尾换行，末行必须自己补一个，否则会和下一条 Environment 粘连。
  if [[ -n "$PROXY_ENV_SNAPSHOT" ]]; then
    printf '%s\n' "$PROXY_ENV_SNAPSHOT"
  fi
  if [[ "$WEBUI_DIST_DISABLED" != "1" ]]; then
    printf 'Environment="PECO_WEBUI_DIST=%s"\n' "$WEBUI_DIST"
  fi
  # 数据根的唯一真源是源码常量，故默认不写 PECO_HOME；只有显式指到别处才落一行。
  if [[ "$WRITE_PECO_HOME" == "1" ]]; then
    printf 'Environment="PECO_HOME=%s"\n' "$DATA_HOME"
  fi
  if [[ "$WRITE_BIND" == "1" ]]; then
    printf 'Environment="PECO_DAEMON_BIND_ADDR=%s"\n' "$BIND_ADDR"
  fi
  if [[ -n "$LOG_LEVEL" ]]; then
    printf 'Environment="PECO_LOG=%s"\n' "$LOG_LEVEL"
  fi
  if [[ -n "$LOG_DIR" ]]; then
    # append:<路径> 里的变量不会展开，故写到 unit 的必须是展开后的绝对路径。
    printf 'StandardOutput=append:%s/daemon.log\n' "$LOG_DIR"
    printf 'StandardError=append:%s/daemon.err.log\n' "$LOG_DIR"
  else
    printf 'StandardOutput=journal\n'
    printf 'StandardError=journal\n'
  fi
  printf '\n[Install]\n'
  printf 'WantedBy=default.target\n'
}

# ── 健康等待与绑定校验（install / refresh-env / restart 共用） ─────────────
wait_healthy() {
  local deadline=$((SECONDS + 30))
  while [[ "$SECONDS" -lt "$deadline" ]]; do
    if curl -fsS --max-time 2 "$PROBE_URL" >/dev/null 2>&1; then
      log_info "daemon answers $PROBE_URL"
      return 0
    fi
    sleep 1
  done
  systemctl --user status --no-pager "$SERVICE" || true
  journalctl --user -u "$SERVICE" -n 50 --no-pager || true
  die "daemon did not become healthy in 30s"
}

api_work_dir() {
  local body work_dir
  body="$(curl -fsS --max-time 2 "$PROBE_URL")" || return 1
  work_dir="$(sed -nE 's/.*"work_dir":"([^"]*)".*/\1/p' <<<"$body")"
  [[ -n "$work_dir" ]] || return 1
  printf '%s' "$work_dir"
}

# work_dir 形如 <home>/spaces/<team>/work，去掉这三段即数据根。
home_from_work_dir() {
  local work_dir="$1"
  if [[ "$work_dir" =~ ^(.*)/spaces/[^/]+/work$ ]]; then
    printf '%s' "${BASH_REMATCH[1]}"
    return 0
  fi
  return 1
}

assert_data_root() {
  local work_dir home
  work_dir="$(api_work_dir)" || die "cannot read $PROBE_URL; is the daemon up?"
  home="$(home_from_work_dir "$work_dir")"     || die "cannot derive the data root from work_dir '$work_dir'"
  # 最后一道闸：跑起来的 daemon 承认的数据根必须与脚本认定的那个一致。
  if [[ "$work_dir" != "$DATA_HOME"/* || "$home" != "$DATA_HOME" ]]; then
    die "work_dir '$work_dir' (data root '$home') does not match the script's '$DATA_HOME'"
  fi
  log_info "work_dir derived from /api/peco: $work_dir"
}

# ── 前置检查 ──────────────────────────────────────────────────────────────
check_tools_for_build() {
  command -v cargo >/dev/null || die "cargo not found in PATH (use --no-build to skip building)"
  [[ -f "$REPO_DIR/crates/peco-daemon/Cargo.toml" ]] || die "crates/peco-daemon/Cargo.toml is missing; is REPO_DIR right? ($REPO_DIR)"
}

check_dist() {
  if [[ "$WEBUI_DIST_DISABLED" == "1" ]]; then
    log_warn "static layer disabled: PECO_WEBUI_DIST is explicitly empty; GET / will 404"
    return 0
  fi
  if [[ ! -d "$WEBUI_DIST" || ! -f "$WEBUI_DIST/index.html" ]]; then
    die "webui dist not usable: $WEBUI_DIST/index.html is missing (run: npm --prefix peco-webui run build)"
  fi
}

check_data_root() {
  # 目录由 daemon 自己建（main.rs 的 create_dir_all），脚本只报告将要使用的路径。
  printf '  data root: %s (created by the daemon on first start)\n' "$DATA_HOME"
  if [[ -d "$DATA_HOME" && ! -f "$DATA_HOME/peco.db" ]]; then
    log_info "data root exists but has no peco.db yet: first boot will bootstrap it"
  fi
}

# 换数据根 = daemon 在一份全新的空库上起来，而 wait_healthy / assert_data_root
# 用的还是同一套「新」输入，能自证清白。故必须显式承认。
guard_data_root_change() {
  [[ "$BASELINE_FROM_UNIT" == "1" ]] || return 0
  local current baseline
  current="$(realpath -m -- "$DATA_HOME")"
  baseline="$UNIT_BASELINE_DATA_ROOT"
  [[ "$current" != "$baseline" ]] || return 0
  if [[ "$ALLOW_HOME_CHANGE" == "1" ]]; then
    log_warn "data root change accepted (--allow-home-change): $baseline -> $current"
    return 0
  fi
  die "refusing to change the data root: installed unit says '$baseline', this run would use '$current' (the daemon would start on a brand-new empty database). Re-run with --allow-home-change to accept it."
}

# ── 端口准入（check 只判定，install 才动手） ──────────────────────────────
# $1 = 1 表示允许接管，$2 = 1 表示只打印不杀（dry run）。
admit_port() {
  local takeover="$1" dry_run="$2" pid main_pid port
  port="$(port_of "$BIND_ADDR")"
  validate_bind_addr "$BIND_ADDR"
  refuse_public_bind "$BIND_ADDR"
  refuse_protected_port "$BIND_ADDR"

  main_pid="$(systemctl --user show -p MainPID --value "$SERVICE" 2>/dev/null || true)"
  pid="$(port_owner_pid "$port")"

  if [[ -z "$pid" ]]; then
    if port_is_listening "$port"; then
      die "port :$port is in use but the owner is not visible; inspect with: sudo ss -ltnp | grep :$port"
    fi
    log_info "port $BIND_ADDR is free"
    return 0
  fi

  if [[ -n "$main_pid" && "$pid" == "$main_pid" ]]; then
    log_info "port $BIND_ADDR is held by this unit (pid $pid); restart will reuse it"
    return 0
  fi

  log_warn "port $BIND_ADDR is held by pid $pid: $(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null || echo '<unknown>')"
  if [[ "$takeover" != "1" ]]; then
    die "port $BIND_ADDR is taken. Either: kill -TERM $pid (then rerun), or rerun with --takeover"
  fi

  verify_takeover_victim "$BIND_ADDR" "$pid"
  if [[ "$dry_run" == "1" ]]; then
    log_info "--takeover would SIGTERM pid $pid (verified: exe/cmdline/owner) and wait up to 10s"
    return 0
  fi

  log_info "sending SIGTERM to verified peco-daemon pid $pid"
  kill -TERM "$pid"
  local deadline=$((SECONDS + 10))
  while [[ "$SECONDS" -lt "$deadline" ]]; do
    port_is_listening "$port" || { log_info "port $BIND_ADDR released"; return 0; }
    sleep 1
  done
  die "port $BIND_ADDR still held 10s after SIGTERM $pid; not sending SIGKILL, investigate manually"
}

# ── 构建与安装 ────────────────────────────────────────────────────────────
build_all() {
  if [[ "$SKIP_BUILD" == "1" ]]; then
    [[ -x "$REPO_DIR/target/release/peco-daemon" ]] || die "--no-build requires $REPO_DIR/target/release/peco-daemon to exist"
    log_info "skipping build (--no-build / PECO_DAEMON_SKIP_BUILD=1)"
    return 0
  fi
  step "Building peco-daemon (release)"
  ( cd "$REPO_DIR" && cargo build --release -p peco-daemon )
  if [[ "$WEBUI_DIST_DISABLED" != "1" ]]; then
    step "Building peco-webui"
    npm --prefix "$REPO_DIR/peco-webui" run build
  fi
}

install_binary() {
  local src="$REPO_DIR/target/release/peco-daemon" err holder
  [[ -f "$src" ]] || die "no release binary at $src"
  if systemctl --user is-active --quiet "$SERVICE"; then
    # 覆盖运行中的可执行文件会 ETXTBSY。
    systemctl --user stop "$SERVICE"
  fi
  mkdir -p "$(dirname "$BIN")"
  if ! err="$(install -Dm755 "$src" "$BIN" 2>&1)"; then
    log_error "cannot install $src -> $BIN: $err"
    holder="$(port_owner_pid "$(port_of "$BIND_ADDR")")"
    if [[ -n "$holder" ]]; then
      die "a daemon still holds $BIND_ADDR (pid $holder): stop it first with 'kill -TERM $holder' or rerun with --takeover, then install again"
    fi
    die "a running peco-daemon may still hold $BIN (Text file busy): 'systemctl --user stop $SERVICE' or stop it manually, then install again"
  fi
  log_info "binary installed: $BIN"
}

write_unit() {
  local rendered
  rendered="$(render_unit)"
  mkdir -p "$UNIT_DIR"

  if [[ -f "$UNIT" ]] && [[ "$rendered" == "$(cat "$UNIT")" ]]; then
    log_info "unit unchanged: $UNIT"
    rm -f "$UNIT.new"
    return 0
  fi
  if [[ -f "$UNIT" ]]; then
    local backup
    backup="$UNIT.bak.$(date +%Y%m%d%H%M%S)"
    cp "$UNIT" "$backup"
    log_info "previous unit backed up: $backup"
  fi
  printf '%s\n' "$rendered" >"$UNIT.new"
  # 固定 0644：unit 是服务定义，权限不该随调用者的 umask 变。
  install -m 0644 "$UNIT.new" "$UNIT"
  rm -f "$UNIT.new"
  log_info "unit written: $UNIT"
}

check_linger() {
  local linger
  linger="$(loginctl show-user "$USER" -p Linger --value 2>/dev/null || true)"
  if [[ "$linger" != "yes" ]]; then
    log_warn "Linger=$linger: the unit will not start at boot. Run: loginctl enable-linger $USER"
  fi
}

print_summary() {
  step "Summary"
  local enabled active
  enabled="$(systemctl --user is-enabled "$SERVICE" 2>/dev/null || true)"
  active="$(systemctl --user is-active "$SERVICE" 2>/dev/null || true)"
  printf '  unit:      %s (%s, %s)\n' "$UNIT" "${enabled:-not-found}" "${active:-unknown}"
  printf '  binary:    %s\n' "$BIN"
  printf '  data root: %s\n' "$DATA_HOME"
  printf '  cwd:       %s\n' "$(readlink "/proc/$(systemctl --user show -p MainPID --value "$SERVICE")/cwd" 2>/dev/null || echo '<unknown>')"
  printf '  url:       http://%s/\n' "$BIND_ADDR"
  printf '  logs:      journalctl --user -u %s -f\n' "$SERVICE"
}

# ── 子命令：check ─────────────────────────────────────────────────────────
cmd_check() {
  resolve_paths 0 1
  step "Effective configuration"
  print_vars
  step "Unit that install would write"
  render_unit
  step "Pre-flight"
  [[ -f "$REPO_DIR/crates/peco-daemon/Cargo.toml" ]] || die "crates/peco-daemon/Cargo.toml is missing"
  guard_data_root_change
  check_dist
  check_data_root
  admit_port "$TAKEOVER" 1
  log_info "check only: nothing was built, written or restarted"
}

# ── 子命令：install ───────────────────────────────────────────────────────
cmd_install() {
  resolve_paths 0 1
  step "Effective configuration"
  print_vars

  step "Pre-flight"
  if [[ "$SKIP_BUILD" != "1" ]]; then
    check_tools_for_build
  else
    [[ -f "$REPO_DIR/crates/peco-daemon/Cargo.toml" ]] || die "crates/peco-daemon/Cargo.toml is missing"
  fi
  # 保护端口、非 loopback、数据根变化都在构建之前拒：省掉一次几分钟的构建才发现不能用。
  validate_bind_addr "$BIND_ADDR"
  refuse_public_bind "$BIND_ADDR"
  refuse_protected_port "$BIND_ADDR"
  guard_data_root_change
  # 干净 clone 上 dist/ 还不存在，dist 校验必须等构建之后；--no-build 没有构建，
  # 只能在动手之前校验。
  if [[ "$SKIP_BUILD" == "1" ]]; then
    check_dist
  fi
  admit_port "$TAKEOVER" 1
  check_data_root

  build_all
  check_dist

  step "Installing binary and unit"
  # 先腾端口再覆盖二进制：占着端口的老 daemon 往往正是从 $BIN 起的进程，
  # 反过来做会 ETXTBSY。
  admit_port "$TAKEOVER" 0
  install_binary
  write_unit
  systemctl --user daemon-reload

  step "Starting service"
  systemctl --user enable "$SERVICE"
  systemctl --user restart "$SERVICE"
  wait_healthy
  assert_data_root
  check_linger
  print_summary
  log_info "deployed: http://$BIND_ADDR/"
}

# ── 子命令：status ────────────────────────────────────────────────────────
cmd_status() {
  resolve_paths 1

  local state pid
  state="$(systemctl --user is-active "$SERVICE" 2>/dev/null || true)"
  pid="$(systemctl --user show -p MainPID --value "$SERVICE" 2>/dev/null || true)"

  log_info "data root in use: $DATA_HOME (from $DATA_HOME_SOURCE)"
  if [[ "$DATA_HOME_SOURCE" != "/api/peco work_dir" ]]; then
    log_warn "/api/peco unreachable; falling back to $DATA_HOME_SOURCE"
  fi

  step "Service"
  printf '  is-active:  %s\n' "${state:-unknown}"
  local enabled
  enabled="$(systemctl --user is-enabled "$SERVICE" 2>/dev/null || true)"
  printf '  is-enabled: %s\n' "${enabled:-not-found}"
  systemctl --user show -p MainPID,ActiveState,SubState,NRestarts "$SERVICE" 2>/dev/null | sed 's/^/  /' || true
  printf '  listen:     %s\n' "$(port_line "$(port_of "$BIND_ADDR")")"
  printf '  work_dir:   %s\n' "${OBSERVED_WORK_DIR:-<unavailable>}"

  step "Process"
  local pid_file="$DATA_HOME/daemon.pid" recorded="" pid_ok=0
  if [[ -f "$pid_file" ]]; then
    recorded="$(tr -dc '0-9' <"$pid_file")"
  fi
  printf '  pid file:   %s -> %s\n' "$pid_file" "${recorded:-<missing>}"
  printf '  MainPID:    %s\n' "${pid:-0}"
  if [[ -n "$recorded" && -n "$pid" && "$pid" != "0" && "$recorded" == "$pid" ]]; then
    pid_ok=1
  else
    log_error "pid disagreement: pid file says '${recorded:-<missing>}', MainPID is '${pid:-0}'"
  fi
  if [[ -d "$DATA_HOME" ]]; then
    du -sh "$DATA_HOME" 2>/dev/null | sed 's/^/  size:       /' || true
  else
    log_warn "data root missing: $DATA_HOME"
  fi

  step "Static layer"
  local dist_ok=1
  local unit_dist
  unit_dist="$(unit_env_value PECO_WEBUI_DIST)"
  if [[ -z "$unit_dist" ]]; then
    log_warn "unit has no PECO_WEBUI_DIST: GET / serves 404"
  elif [[ -f "$unit_dist/index.html" ]]; then
    log_info "dist ok: $unit_dist"
  else
    dist_ok=0
    log_error "dist from the unit is gone: $unit_dist/index.html is missing (rerun install)"
  fi

  step "Recent journal"
  journalctl --user -u "$SERVICE" -n 20 --no-pager 2>/dev/null | sed 's/^/  /' || true

  # 判据与设计一致：active 且 pid 一致且 dist 存在才算健康；pid 不一致是失败，不是提示。
  if [[ "$state" == "active" && "$dist_ok" == "1" && "$pid_ok" == "1" ]]; then
    exit 0
  fi
  exit 1
}

# ── 子命令：restart ───────────────────────────────────────────────────────
cmd_restart() {
  resolve_paths 0 1
  [[ -f "$UNIT" ]] || die "unit not installed: $UNIT (run install first)"
  systemctl --user restart "$SERVICE"
  wait_healthy
  assert_data_root
  print_summary
}

# ── 子命令：refresh-env ───────────────────────────────────────────────────
cmd_refresh_env() {
  # 只重取 PATH。unit 的其余行是 install 时的决定（数据根 / dist / bind / 日志），
  # refresh-env 不知道当时的意图，重算就会静默搬数据根、关掉又打开的静态层。
  resolve_paths 0 1
  [[ -f "$UNIT" ]] || die "unit not installed: $UNIT (run install first)"

  local lines current current_proxy
  lines="$(grep -c '^Environment="PATH=' "$UNIT" || true)"
  if [[ "$lines" != "1" ]]; then
    die "expected exactly one Environment=\"PATH=\" line in $UNIT, found ${lines:-0} (rerun install)"
  fi
  current="$(sed -nE 's/^Environment="PATH=(.*)"$/\1/p' "$UNIT")"
  # 代理行与 PATH 同源（都来自调用者环境，不是 install 的配置决定），一并重取。
  current_proxy="$(proxy_lines_of_unit)"
  if [[ "$current" == "$PATH_SNAPSHOT" && "$current_proxy" == "${PROXY_ENV_SNAPSHOT%$'\n'}" ]]; then
    log_info "no change: PATH and proxy snapshot already in $UNIT"
    exit 0
  fi

  local backup="$UNIT.bak.$(date +%Y%m%d%H%M%S)"
  cp "$UNIT" "$backup"
  log_info "previous unit backed up: $backup"

  # 用 awk 而不是 sed 替换：插入多行块时 sed 要么转义地狱，要么依赖 `r` 的文件名。
  # 值都经 ENVIRON 传入，`-v` 会把反斜杠当转义序列处理。
  NEW_PATH="$PATH_SNAPSHOT" NEW_PROXY="$PROXY_ENV_SNAPSHOT" awk '
    BEGIN {
      path = ENVIRON["NEW_PATH"]
      proxy = ENVIRON["NEW_PROXY"]
    }
    /^Environment="(HTTPS?_PROXY|ALL_PROXY|NO_PROXY|https?_proxy|all_proxy|no_proxy)=/ { next }
    /^Environment="PATH=/ {
      print "Environment=\"PATH=" path "\""
      if (proxy != "") printf "%s\n", proxy
      next
    }
    { print }
  ' "$UNIT" >"$UNIT.new"
  install -m 0644 "$UNIT.new" "$UNIT"
  rm -f "$UNIT.new"
  log_info "PATH and proxy lines refreshed in $UNIT"

  systemctl --user daemon-reload
  systemctl --user restart "$SERVICE"
  wait_healthy
  assert_data_root
  print_summary
}

# ── 删除路径的守门（--purge-data 专用） ───────────────────────────────────
# 删除不可逆，而数据根本身可以由 /api/peco 反推得到 —— 也就是说「用户没给错
# 路径」并不能保证目标安全。所有拒绝都发生在 rm -rf 之前。
guard_purge_target() {
  local raw="$1" resolved home_real home_dir_name base
  PURGE_TARGET=""

  [[ -n "$raw" ]] || die "refusing to purge an empty data root"
  case "$raw" in
    .|..|/) die "refusing to purge '$raw'" ;;
  esac

  resolved="$(realpath -m -- "$raw")" || die "cannot resolve '$raw'"
  home_real="$(realpath -m -- "$HOME")" || die "cannot resolve HOME"
  home_dir_name="$(require_home_dir_name "$CONFIG_SRC")"

  if [[ "$resolved" == "/" ]]; then
    die "refusing to purge '/'"
  fi
  if [[ "$resolved" == "$home_real" ]]; then
    die "refusing to purge HOME itself: $resolved"
  fi
  if [[ "$home_real" == "$resolved"/* ]]; then
    die "refusing to purge an ancestor of HOME: $resolved"
  fi
  case "$resolved" in
    "$home_real/.peco"|"$home_real/.peco2")
      die "refusing to purge '$resolved': it belongs to peco-server (9227), not to this daemon"
      ;;
  esac

  base="$(basename "$resolved")"
  if [[ "$base" != "$home_dir_name" && "$(dirname "$resolved")" != "$home_real" ]]; then
    die "refusing to purge '$resolved': expected '$HOME/$home_dir_name' or a direct child of $HOME"
  fi

  # 互锁：unit 停着、pid 文件里的 pid 也不在跑，才允许删。
  local state pid_file recorded
  state="$(systemctl --user is-active "$SERVICE" 2>/dev/null || true)"
  if [[ "$state" == "active" ]]; then
    die "refusing to purge: $SERVICE is still active (stop it first: systemctl --user stop $SERVICE)"
  fi
  pid_file="$resolved/daemon.pid"
  if [[ -f "$pid_file" ]]; then
    recorded="$(tr -dc '0-9' <"$pid_file")"
    if [[ -n "$recorded" ]] && [[ -d "/proc/$recorded" ]] \
      && tr '\0' ' ' <"/proc/$recorded/cmdline" 2>/dev/null | grep -q 'peco-daemon'; then
      die "refusing to purge: $pid_file records a live peco-daemon (pid $recorded)"
    fi
  fi

  PURGE_TARGET="$resolved"
}

report_purge_plan() {
  local target="$1" size
  size="$(du -sh -- "$target" 2>/dev/null | cut -f1 || true)"
  log_warn "purging data root: $target (${size:-size unknown})"
  find "$target" -maxdepth 1 -mindepth 1 2>/dev/null | head -n 20 | sed 's/^/    /' || true
}

# ── 子命令：uninstall ─────────────────────────────────────────────────────
cmd_uninstall() {
  resolve_paths 1
  log_info "data root in use: $DATA_HOME (from $DATA_HOME_SOURCE)"

  systemctl --user disable --now "$SERVICE" 2>/dev/null || log_warn "service $SERVICE is not enabled/running"
  # $UNIT.new 是 install/refresh-env 中途失败留下的残骸，一并清掉。
  rm -f "$UNIT" "$UNIT.new"
  systemctl --user daemon-reload 2>/dev/null || true
  systemctl --user reset-failed "$SERVICE" 2>/dev/null || true
  log_info "unit removed: $UNIT"

  if [[ "$PURGE_BIN" == "1" ]]; then
    rm -f "$BIN"
    log_info "binary removed: $BIN"
  else
    log_info "binary kept: $BIN (use --purge-bin to remove)"
  fi

  if [[ "$PURGE_DATA" == "1" ]]; then
    [[ "${PECO_DAEMON_CONFIRM_PURGE:-}" == "1" ]] || die "--purge-data requires PECO_DAEMON_CONFIRM_PURGE=1"
    guard_purge_target "$DATA_HOME"
    if [[ ! -d "$PURGE_TARGET" ]]; then
      log_warn "data root already gone: $PURGE_TARGET"
      exit 0
    fi
    report_purge_plan "$PURGE_TARGET"
    rm -rf "${PURGE_TARGET:?}"
    log_info "data root removed: $PURGE_TARGET"
  else
    log_info "data root kept: $DATA_HOME (use --purge-data with PECO_DAEMON_CONFIRM_PURGE=1 to remove)"
  fi
}

case "$COMMAND" in
  check) cmd_check ;;
  install) cmd_install ;;
  status) cmd_status ;;
  restart) cmd_restart ;;
  refresh-env) cmd_refresh_env ;;
  uninstall) cmd_uninstall ;;
esac
