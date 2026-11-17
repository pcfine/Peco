#!/usr/bin/env bash
# ============================================================================
# scripts/deploy-daemon.sh 的行为测试
# ============================================================================
# 全部用例跑在「假 HOME + 假 repo + PATH shim」里：
#   - 真实的 systemd / ss / curl / cargo / npm / loginctl 都被 shim 顶掉，
#     一条 systemctl 也不会漏到机器上；
#   - HOME 指向临时目录，repo 是临时目录里的一份脚本副本，任何 rm -rf 都落在
#     临时目录内；
#   - 用例只断言脚本的可判定行为（退出码、unit 内容、文件是否还在）。
#
# 用法: bash scripts/tests/deploy-daemon.sh [用例名过滤子串]
# ============================================================================

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_SRC="$(cd "$HERE/../.." && pwd)/scripts/deploy-daemon.sh"
[[ -f "$SCRIPT_SRC" ]] || { printf 'not found: %s\n' "$SCRIPT_SRC" >&2; exit 2; }

FILTER="${1:-}"
TESTS_OK=0
TESTS_FAILED=0
FAILED_NAMES=()
CURRENT=""
CASE_FAILS=0
SANDBOX=""
FAKE_HOME=""
FAKE_REPO=""
SHIM_DIR=""
UNIT=""

cleanup_sandbox() {
  if [[ -n "$SANDBOX" && -d "$SANDBOX" ]]; then
    rm -rf "$SANDBOX"
  fi
  SANDBOX=""
}

trap cleanup_sandbox EXIT

# ── 断言 ──────────────────────────────────────────────────────────────────
ok()   { CASE_FAILS=$((CASE_FAILS + 1)); printf '    x %s: %s\n' "$CURRENT" "$1"; }
check_eq() { [[ "$1" == "$2" ]] || ok "$3 (expected [$1], got [$2])"; }
check_ne() { [[ "$1" != "$2" ]] || ok "$3 (both [$1])"; }
check_contains() {
  case "$2" in
    *"$1"*) return 0 ;;
  esac
  ok "$3 (missing [$1] in: $2)"
}
check_not_contains() {
  case "$2" in
    *"$1"*) ok "$3 (unexpected [$1] in: $2)" ; return 0 ;;
  esac
  return 0
}
check_file()    { [[ -e "$1" ]] || ok "$2 (missing $1)"; }
check_no_file() { [[ ! -e "$1" ]] || ok "$2 (still there: $1)"; }

# ── 沙箱 ──────────────────────────────────────────────────────────────────
init_shims() {
  cat >"$SHIM_DIR/systemctl" <<'SHIM'
#!/usr/bin/env bash
set -uo pipefail
state="${FAKE_STATE:?}"
args=()
for a in "$@"; do
  [[ "$a" == "--user" ]] || args+=("$a")
done
verb="${args[0]:-}"
has_value=0
for a in "${args[@]:1}"; do
  [[ "$a" == "--value" ]] && has_value=1
done
case "$verb" in
  is-active)
    if [[ "$(cat "$state/active" 2>/dev/null)" == "active" ]]; then
      echo active; exit 0
    fi
    echo inactive; exit 3 ;;
  is-enabled)
    if [[ "$(cat "$state/enabled" 2>/dev/null)" == "enabled" ]]; then
      echo enabled; exit 0
    fi
    echo disabled; exit 1 ;;
  show)
    if [[ "$has_value" == "1" ]]; then
      cat "$state/mainpid"; exit 0
    fi
    printf 'MainPID=%s\nActiveState=%s\nSubState=running\nNRestarts=0\n' \
      "$(cat "$state/mainpid")" "$(cat "$state/active")"
    exit 0 ;;
  disable|stop)
    echo inactive >"$state/active"
    echo 0 >"$state/mainpid"
    exit 0 ;;
  enable)
    echo enabled >"$state/enabled"; exit 0 ;;
  start|restart)
    echo enabled >"$state/enabled"
    echo active >"$state/active"
    echo 4242 >"$state/mainpid"
    exit 0 ;;
  status)
    echo "fake systemctl status"; exit 0 ;;
  *)
    exit 0 ;;
esac
SHIM

  cat >"$SHIM_DIR/loginctl" <<'SHIM'
#!/usr/bin/env bash
set -uo pipefail
echo yes
SHIM

  cat >"$SHIM_DIR/journalctl" <<'SHIM'
#!/usr/bin/env bash
set -uo pipefail
exit 0
SHIM

  cat >"$SHIM_DIR/ss" <<'SHIM'
#!/usr/bin/env bash
set -uo pipefail
cat "${FAKE_SS:?}"
SHIM

  # 假 daemon：数据根只由 unit 里的 PECO_HOME 决定 —— 与真 daemon 一致，
  # 于是「脚本偷偷换了数据根」与「daemon 真的在新空库上起来」分得开。
  cat >"$SHIM_DIR/curl" <<'SHIM'
#!/usr/bin/env bash
set -uo pipefail
state="${FAKE_STATE:?}"
[[ "$(cat "$state/active" 2>/dev/null)" == "active" ]] || exit 7
home=""
if [[ -f "${FAKE_UNIT:?}" ]]; then
  home="$(sed -nE 's/^Environment="PECO_HOME=(.*)"$/\1/p' "$FAKE_UNIT" | tail -n 1)"
fi
if [[ -n "${FAKE_WORK_DIR_OVERRIDE:-}" ]]; then
  work_dir="$FAKE_WORK_DIR_OVERRIDE"
elif [[ -n "$home" ]]; then
  work_dir="$home/spaces/peco/work"
else
  work_dir="$HOME/.peco-daemon/spaces/peco/work"
fi
printf '{"work_dir":"%s"}\n' "$work_dir"
SHIM

  cat >"$SHIM_DIR/cargo" <<'SHIM'
#!/usr/bin/env bash
set -uo pipefail
mkdir -p "${FAKE_REPO:?}/target/release"
printf '#!/bin/sh\nexit 0\n' >"$FAKE_REPO/target/release/peco-daemon"
chmod +x "$FAKE_REPO/target/release/peco-daemon"
: >"${FAKE_STATE:?}/cargo-ran"
exit 0
SHIM

  cat >"$SHIM_DIR/npm" <<'SHIM'
#!/usr/bin/env bash
set -uo pipefail
prefix="."
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) prefix="$2"; shift 2 ;;
    *) shift ;;
  esac
done
mkdir -p "$prefix/dist/assets"
printf '<div id="root"></div>\n' >"$prefix/dist/index.html"
printf 'console.log(1)\n' >"$prefix/dist/assets/app.js"
: >"${FAKE_STATE:?}/npm-ran"
exit 0
SHIM

  chmod +x "$SHIM_DIR"/*
}

sandbox_new() {
  cleanup_sandbox
  SANDBOX="$(mktemp -d /tmp/peco-deploy-test.XXXXXX)"
  FAKE_HOME="$SANDBOX/home"
  FAKE_REPO="$SANDBOX/repo"
  SHIM_DIR="$SANDBOX/bin"
  UNIT="$FAKE_HOME/.config/systemd/user/peco-daemon.service"
  mkdir -p "$FAKE_HOME/.config/systemd/user" "$SHIM_DIR" "$SANDBOX/systemd" \
    "$SANDBOX/cwd" "$FAKE_REPO/scripts" "$FAKE_REPO/crates/peco-daemon/src" \
    "$FAKE_REPO/peco-webui"
  cp "$SCRIPT_SRC" "$FAKE_REPO/scripts/deploy-daemon.sh"
  printf 'pub const HOME_DIR_NAME: &str = ".peco-daemon";\n' \
    >"$FAKE_REPO/crates/peco-daemon/src/config.rs"
  : >"$FAKE_REPO/crates/peco-daemon/Cargo.toml"
  echo inactive >"$SANDBOX/systemd/active"
  echo 0 >"$SANDBOX/systemd/mainpid"
  echo disabled >"$SANDBOX/systemd/enabled"
  : >"$SANDBOX/ss.txt"
  init_shims
}

# 干净环境跑脚本：只带 HOME / USER / PATH / shim 状态，别的一概没有 ——
# 「换一个干净 shell 再跑一次」正是被修复的那条路径。
#   run_script [VAR=value ...] -- <脚本参数...>
run_script() {
  local envs=()
  while [[ $# -gt 0 && "$1" != "--" ]]; do
    envs+=("$1")
    shift
  done
  if [[ "${1:-}" == "--" ]]; then
    shift
  fi
  (
    cd "$SANDBOX/cwd" || exit 99
    env -i \
      HOME="$FAKE_HOME" \
      USER=peco-test \
      TERM=dumb \
      PATH="$SHIM_DIR:/usr/local/bin:/usr/bin:/bin" \
      FAKE_STATE="$SANDBOX/systemd" \
      FAKE_SS="$SANDBOX/ss.txt" \
      FAKE_UNIT="$UNIT" \
      FAKE_REPO="$FAKE_REPO" \
      "${envs[@]}" \
      bash "$FAKE_REPO/scripts/deploy-daemon.sh" "$@"
  )
}

dist_fixture() {
  local dir="$1"
  mkdir -p "$dir"
  printf '<div id="root"></div>\n' >"$dir/index.html"
}

# ── 用例 ──────────────────────────────────────────────────────────────────

# B1：--purge-data 的危险目标必须在 rm -rf 之前被拒，且不得真的删任何东西。
purge_data_refuses_dangerous_paths() {
  local home="$FAKE_HOME"
  mkdir -p "$home/.peco" "$home/.peco2" "$home/.peco-daemon"
  : >"$home/canary-home"
  : >"$home/.peco/server.db"
  : >"$home/.peco2/canary"
  : >"$home/.peco-daemon/peco.db"

  local -a targets=("$home" "$home/" "$home/.peco" "$home/.peco2" "/" ".." ".")
  local t out rc
  for t in "${targets[@]}"; do
    out="$(run_script PECO_DAEMON_CONFIRM_PURGE=1 PECO_DAEMON_HOME="$t" \
      -- uninstall --purge-data 2>&1)"
    rc=$?
    check_ne 0 "$rc" "purge '$t' 必须被拒绝"
    check_not_contains "data root removed" "$out" "purge '$t' 不得执行删除"
  done

  # 空串：数据根由 /api/peco 的 work_dir 反推得到空（work_dir 没有 <home> 前缀）。
  echo active >"$SANDBOX/systemd/active"
  echo 4242 >"$SANDBOX/systemd/mainpid"
  out="$(run_script PECO_DAEMON_CONFIRM_PURGE=1 \
    FAKE_WORK_DIR_OVERRIDE=/spaces/peco/work -- uninstall --purge-data 2>&1)"
  rc=$?
  check_ne 0 "$rc" "空数据根必须被拒绝"
  check_not_contains "data root removed" "$out" "空数据根不得执行删除"
  echo inactive >"$SANDBOX/systemd/active"

  check_file "$home/canary-home" "家目录标记必须还在"
  check_file "$home/.peco/server.db" "peco-server 的数据必须还在"
  check_file "$home/.peco2/canary" "被拉黑的数据根必须还在"
  check_file "$home/.peco-daemon/peco.db" "危险路径用例不该碰 daemon 数据根"
}

# B1：有活着的 peco-daemon 时不得 purge；进程消失后才放行。
purge_data_interlocks_with_a_live_daemon() {
  local root="$FAKE_HOME/.peco-daemon"
  mkdir -p "$root"
  : >"$root/peco.db"
  cp /bin/sleep "$SANDBOX/peco-daemon"
  "$SANDBOX/peco-daemon" 30 &
  local live=$!
  echo "$live" >"$root/daemon.pid"

  local out rc
  out="$(run_script PECO_DAEMON_CONFIRM_PURGE=1 -- uninstall --purge-data 2>&1)"
  rc=$?
  check_ne 0 "$rc" "有活着的 peco-daemon 时必须拒绝"
  check_contains "$live" "$out" "报错应指向那个 pid"
  check_file "$root/peco.db" "拒绝时数据必须还在"


  kill "$live" 2>/dev/null
  wait "$live" 2>/dev/null
  out="$(run_script PECO_DAEMON_CONFIRM_PURGE=1 -- uninstall --purge-data 2>&1)"
  rc=$?
  check_eq 0 "$rc" "pid 已消失时应放行：$out"
  check_contains "purging data root" "$out" "删之前应打印将要删的路径与体积"
  check_contains "peco.db" "$out" "删之前应打印目录内容摘要"
  check_no_file "$root/peco.db" "放行后数据应被删掉"
}

# B2：refresh-env 只换 PATH 那一行，数据根与静态层开关一字不改。
refresh_env_replaces_only_the_path_line() {
  local alt="$SANDBOX/alt-home" dist="$SANDBOX/dist"
  dist_fixture "$dist"
  local out rc

  out="$(run_script PECO_DAEMON_HOME="$alt" PECO_WEBUI_DIST="$dist" -- install 2>&1)"
  rc=$?
  check_eq 0 "$rc" "install 应成功：$out"
  check_contains "Environment=\"PECO_HOME=$alt\"" "$(cat "$UNIT")" \
    "install 应把自定义数据根写进 unit"
  # 运维手工加的一行（脚本认识的开关里没有它）：refresh-env 只该换 PATH，
  # 全量重渲染会把它悄悄抹掉。
  sed -i 's|^StandardOutput=journal$|Environment="PECO_DAEMON_EXIT_POLICY=keep"\nStandardOutput=journal|' "$UNIT"
  check_contains "PECO_DAEMON_EXIT_POLICY=keep" "$(cat "$UNIT")" "手工加的行应在 unit 里"
  local before
  before="$(cat "$UNIT")"

  out="$(run_script PATH="$SHIM_DIR:/usr/bin:/bin:/opt/probe/bin" -- refresh-env 2>&1)"
  rc=$?
  check_eq 0 "$rc" "refresh-env 应成功：$out"
  local after
  after="$(cat "$UNIT")"
  check_contains "Environment=\"PECO_HOME=$alt\"" "$after" "refresh-env 不得重算数据根"
  check_contains "Environment=\"PECO_WEBUI_DIST=$dist\"" "$after" \
    "refresh-env 不得重算静态层"
  check_contains "/opt/probe/bin" "$after" "refresh-env 应刷新 PATH"
  check_eq "" "$(diff <(grep -v '^Environment="PATH=' <<<"$before") \
    <(grep -v '^Environment="PATH=' <<<"$after"))" \
    "除 PATH 行外 unit 必须一字不改"

  # 关掉静态层后再裸跑 refresh-env：那行不能自己长回来。
  out="$(run_script PECO_DAEMON_HOME="$alt" PECO_WEBUI_DIST= -- install 2>&1)"
  rc=$?
  check_eq 0 "$rc" "关闭静态层的 install 应成功：$out"
  check_not_contains "PECO_WEBUI_DIST=" "$(cat "$UNIT")" "显式置空应不写 dist 行"
  out="$(run_script PATH="$SHIM_DIR:/usr/bin:/bin:/opt/probe2/bin" -- refresh-env 2>&1)"
  rc=$?
  check_eq 0 "$rc" "refresh-env 应成功：$out"
  check_not_contains "PECO_WEBUI_DIST=" "$(cat "$UNIT")" \
    "refresh-env 不得把静态层偷偷打开"
}

# B2：重跑 install 以已装 unit 为基线；换数据根必须显式承认。
install_keeps_the_installed_data_root() {
  local alt="$SANDBOX/alt-home" dist="$SANDBOX/dist" other="$SANDBOX/other-home"
  dist_fixture "$dist"
  local out rc

  out="$(run_script PECO_DAEMON_HOME="$alt" PECO_WEBUI_DIST="$dist" -- install 2>&1)"
  rc=$?
  check_eq 0 "$rc" "首次 install 应成功：$out"

  local backups_before
  backups_before="$(find "$(dirname "$UNIT")" -name '*.bak.*' | wc -l)"
  out="$(run_script -- install 2>&1)"
  rc=$?
  check_eq 0 "$rc" "干净 shell 重跑 install 应成功：$out"
  check_contains "Environment=\"PECO_HOME=$alt\"" "$(cat "$UNIT")" \
    "干净 shell 重跑 install 不得换数据根"
  check_eq "$backups_before" "$(find "$(dirname "$UNIT")" -name '*.bak.*' | wc -l)" \
    "内容没变的 install 不该再备份 unit（幂等）"
  check_contains "Environment=\"PECO_WEBUI_DIST=$dist\"" "$(cat "$UNIT")" \
    "干净 shell 重跑 install 不得关掉静态层"

  out="$(run_script PECO_DAEMON_HOME="$other" -- install 2>&1)"
  rc=$?
  check_ne 0 "$rc" "换数据根必须显式 --allow-home-change"
  check_contains "$alt" "$out" "拒绝信息应打印旧数据根"
  check_contains "$other" "$out" "拒绝信息应打印新数据根"
  check_contains "Environment=\"PECO_HOME=$alt\"" "$(cat "$UNIT")" \
    "被拒绝时 unit 不得改动"

  out="$(run_script PECO_DAEMON_HOME="$other" -- install --allow-home-change 2>&1)"
  rc=$?
  check_eq 0 "$rc" "--allow-home-change 应放行：$out"
  check_contains "Environment=\"PECO_HOME=$other\"" "$(cat "$UNIT")" \
    "放行后 unit 应记下新数据根"
}

# B3：全新 clone 上 dist 还不存在，install 要在构建之后才校验它。
fresh_clone_install_bootstraps_the_dist() {
  local out rc
  check_no_file "$FAKE_REPO/peco-webui/dist" "起点不应有 dist"
  out="$(run_script -- install 2>&1)"
  rc=$?
  check_eq 0 "$rc" "空 dist 起步的 install 应成功：$out"
  check_file "$SANDBOX/systemd/cargo-ran" "应真的走到构建"
  check_file "$SANDBOX/systemd/npm-ran" "应真的构建前端"
  check_file "$FAKE_REPO/peco-webui/dist/index.html" "构建应产出 dist"
  check_contains "Environment=\"PECO_WEBUI_DIST=$FAKE_REPO/peco-webui/dist\"" \
    "$(cat "$UNIT")" "unit 应写入构建出来的 dist"

  rm -rf "$FAKE_REPO/peco-webui/dist"
  out="$(run_script -- install --no-build 2>&1)"
  rc=$?
  check_ne 0 "$rc" "--no-build 缺 dist 必须拒绝"
  check_contains "webui dist not usable" "$out" "报错应指向 dist"
}

# B4：占用判定按端口，非 loopback 默认拒绝。
bind_addr_occupancy_and_public_guard() {
  local out rc
  dist_fixture "$FAKE_REPO/peco-webui/dist"

  write_ss "LISTEN 0 128 0.0.0.0:6971 0.0.0.0:* users:((\"peco-daemon\",pid=5150,fd=7))"
  out="$(run_script -- check 2>&1)"
  rc=$?
  check_ne 0 "$rc" "0.0.0.0:6971 被占时 check 必须失败"
  check_not_contains "is free" "$out" "不得报端口空闲"
  check_contains "5150" "$out" "应打印占用者的 pid"

  local line
  for line in \
    'LISTEN 0 128 [::]:6971 [::]:* users:(("x",pid=7001,fd=3))' \
    'LISTEN 0 128 *:6971 *:* users:(("y",pid=7002,fd=3))' \
    'LISTEN 0 128 myhost:6971 0.0.0.0:* users:(("z",pid=7003,fd=3))'; do
    write_ss "$line"
    out="$(run_script -- check 2>&1)"
    rc=$?
    check_ne 0 "$rc" "本地地址 '$line' 必须被判占用"
    check_not_contains "is free" "$out" "本地地址 '$line' 不得报空闲"
  done

  : >"$SANDBOX/ss.txt"
  out="$(run_script -- check 2>&1)"
  rc=$?
  check_eq 0 "$rc" "端口空闲时 check 应通过：$out"

  out="$(run_script PECO_DAEMON_BIND_ADDR=0.0.0.0:9999 -- check 2>&1)"
  rc=$?
  check_ne 0 "$rc" "非 loopback 默认必须拒绝"
  check_contains "only loopback" "$out" "报错应说明只支持 loopback"

  out="$(run_script PECO_DAEMON_BIND_ADDR=0.0.0.0:9999 PECO_DAEMON_ALLOW_PUBLIC=1 -- check 2>&1)"
  rc=$?
  check_eq 0 "$rc" "ALLOW_PUBLIC=1 应放行：$out"
  check_contains "WARN" "$out" "必须打 WARN"
  check_contains "NOT loopback" "$out" "WARN 必须说清风险"

  out="$(run_script "PECO_DAEMON_BIND_ADDR=[::1]:9999" -- check 2>&1)"
  rc=$?
  check_eq 0 "$rc" "[::1]:9999 应被接受：$out"
}

# B5：pid 不一致是失败条件，不是提示。
status_fails_on_pid_disagreement() {
  local alt="$SANDBOX/alt-home" dist="$SANDBOX/dist"
  mkdir -p "$alt"
  dist_fixture "$dist"
  local out rc

  run_script PECO_DAEMON_HOME="$alt" PECO_WEBUI_DIST="$dist" -- install >/dev/null 2>&1
  echo active >"$SANDBOX/systemd/active"
  echo 4242 >"$SANDBOX/systemd/mainpid"

  echo 9999 >"$alt/daemon.pid"
  out="$(run_script -- status 2>&1)"
  rc=$?
  check_eq 1 "$rc" "pid 不一致必须 exit 1：$out"
  check_contains "9999" "$out" "应打印 pid 文件里的值"
  check_contains "4242" "$out" "应打印 MainPID"
  check_contains "$alt/spaces/peco/work" "$out" "应打印原始 work_dir"

  echo 4242 >"$alt/daemon.pid"
  out="$(run_script -- status 2>&1)"
  rc=$?
  check_eq 0 "$rc" "pid 一致且 dist 存在应 exit 0：$out"

  rm -f "$alt/daemon.pid"
  out="$(run_script -- status 2>&1)"
  rc=$?
  check_eq 1 "$rc" "pid 文件缺失也算不一致"
}

# Nit 4 + Nit 5：unit 权限与 $UNIT.new 残留。
unit_mode_is_fixed_and_uninstall_cleans_leftovers() {
  dist_fixture "$FAKE_REPO/peco-webui/dist"
  local out rc
  out="$(
    umask 002
    run_script -- install 2>&1
  )"
  rc=$?
  check_eq 0 "$rc" "install 应成功：$out"
  check_eq 644 "$(stat -c %a "$UNIT")" "unit 权限应固定 0644（不随 umask）"

  : >"$UNIT.new"
  run_script -- uninstall >/dev/null 2>&1
  check_no_file "$UNIT.new" "uninstall 应清理 \$UNIT.new"
  check_no_file "$UNIT" "uninstall 应删掉 unit"
}

# Nit 9：端口准入在装二进制、构建之前。
install_admits_the_port_before_touching_the_binary() {
  dist_fixture "$FAKE_REPO/peco-webui/dist"
  write_ss 'LISTEN 0 128 127.0.0.1:6971 0.0.0.0:* users:(("python3",pid=1,fd=3))'
  local out rc
  out="$(run_script -- install --takeover 2>&1)"
  rc=$?
  check_ne 0 "$rc" "身份不符必须拒绝接管"
  check_contains "refusing to take over pid 1" "$out" "报错应指向被拒的 pid"
  check_no_file "$FAKE_HOME/.local/bin/peco-daemon" "拒绝接管时不得覆盖二进制"
  check_no_file "$SANDBOX/systemd/cargo-ran" "准入应在构建之前"
}

write_ss() {
  {
    printf 'State Recv-Q Send-Q Local Address:Port Peer Address:Port Process\n'
    printf '%s\n' "$1"
  } >"$SANDBOX/ss.txt"
}

# ── 运行 ──────────────────────────────────────────────────────────────────

# B7：代理环境与 PATH 同源：install 照抄、refresh-env 刷新、缺值时保留基线。
#
# 这条用例钉的是真故障：本机出网要走登录 shell 里的本地代理，systemd --user 的
# 环境是干净的，unit 少了这几行，daemon 的模型请求只会得到
# 「error sending request for url (.../v1/messages)」——既没有状态码也没有原因。
proxy_env_is_propagated_and_refreshable() {
  local dist="$SANDBOX/dist"
  dist_fixture "$dist"
  local out rc unit_text

  out="$(run_script PECO_WEBUI_DIST="$dist" \
    HTTPS_PROXY=http://127.0.0.1:7890 NO_PROXY="localhost,127.0.0.1" -- install 2>&1)"
  rc=$?
  check_eq 0 "$rc" "带代理变量的 install 应成功：$out"
  unit_text="$(cat "$UNIT")"
  check_contains 'Environment="HTTPS_PROXY=http://127.0.0.1:7890"' "$unit_text" \
    "install 应把 HTTPS_PROXY 写进 unit"
  check_contains 'Environment="NO_PROXY=localhost,127.0.0.1"' "$unit_text" \
    "install 应把 NO_PROXY 写进 unit"

  # 只改一个变量：它必须被刷新，而没给的 NO_PROXY 必须保留基线（不是被抹掉）。
  out="$(run_script HTTPS_PROXY=http://127.0.0.1:7891 -- refresh-env 2>&1)"
  rc=$?
  check_eq 0 "$rc" "refresh-env 应成功：$out"
  unit_text="$(cat "$UNIT")"
  check_contains 'Environment="HTTPS_PROXY=http://127.0.0.1:7891"' "$unit_text" \
    "refresh-env 应换掉 HTTPS_PROXY 的值"
  check_not_contains "7890" "$unit_text" "旧代理值不得残留"
  check_contains 'Environment="NO_PROXY=localhost,127.0.0.1"' "$unit_text" \
    "调用者没给 NO_PROXY 时应沿用已装 unit 的基线"
  check_eq "1" "$(grep -c '^Environment="HTTPS_PROXY=' <<<"$unit_text")" \
    "HTTPS_PROXY 只能有一行（重复行会在 systemd 里叠成两份环境）"

  # 裸跑 refresh-env（干净环境、没有任何代理变量）：基线仍在，不该被清空。
  out="$(run_script PATH="$SHIM_DIR:/usr/bin:/bin:/opt/probe/bin" -- refresh-env 2>&1)"
  rc=$?
  check_eq 0 "$rc" "裸跑 refresh-env 应成功：$out"
  unit_text="$(cat "$UNIT")"
  check_contains 'Environment="HTTPS_PROXY=http://127.0.0.1:7891"' "$unit_text" \
    "没有代理变量时不得抹掉已装好的代理"
  check_contains "/opt/probe/bin" "$unit_text" "PATH 仍应被刷新"
}

ALL_TESTS=(
  purge_data_refuses_dangerous_paths
  purge_data_interlocks_with_a_live_daemon
  refresh_env_replaces_only_the_path_line
  proxy_env_is_propagated_and_refreshable
  install_keeps_the_installed_data_root
  fresh_clone_install_bootstraps_the_dist
  bind_addr_occupancy_and_public_guard
  status_fails_on_pid_disagreement
  unit_mode_is_fixed_and_uninstall_cleans_leftovers
  install_admits_the_port_before_touching_the_binary
)

for test_name in "${ALL_TESTS[@]}"; do
  if [[ -n "$FILTER" && "$test_name" != *"$FILTER"* ]]; then
    continue
  fi
  CURRENT="$test_name"
  CASE_FAILS=0
  sandbox_new
  if [[ "$FAKE_HOME" == "$HOME" ]]; then
    printf 'refusing to run: fake HOME collides with the real one\n' >&2
    exit 2
  fi
  "$test_name"
  if [[ "$CASE_FAILS" -eq 0 ]]; then
    printf 'ok   %s\n' "$test_name"
    TESTS_OK=$((TESTS_OK + 1))
  else
    printf 'FAIL %s\n' "$test_name"
    TESTS_FAILED=$((TESTS_FAILED + 1))
    FAILED_NAMES+=("$test_name")
  fi
done

printf '\n%s passed, %s failed\n' "$TESTS_OK" "$TESTS_FAILED"
if [[ "$TESTS_FAILED" -gt 0 ]]; then
  printf 'failed: %s\n' "${FAILED_NAMES[*]}"
  exit 1
fi
