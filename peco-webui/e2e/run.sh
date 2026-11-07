#!/usr/bin/env bash
# 一键跑浏览器验收台：临时 PECO_HOME 起 daemon → 起 dev server → 灌演示数据 →
# xvfb-run 下的真实 Chromium 量测 → 收摊（进程与临时目录都不留）。
#
# 幂等：端口先探测，被占就报错退出（不去杀别人的进程）；临时目录 mktemp 每次独立。
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WEBUI="$(cd "$HERE/.." && pwd)"
REPO="$(cd "$WEBUI/.." && pwd)"

DAEMON_PORT="${E2E_DAEMON_PORT:-6971}"
VITE_PORT=5174
BASE_URL="http://127.0.0.1:${VITE_PORT}"

DAEMON_PID=""
VITE_PID=""
PECO_HOME=""

log() { printf '%s\n' "[run.sh] $*"; }

port_busy() {
  ss -ltnH 2>/dev/null | grep -qE "[:.]$1[[:space:]]"
}

cleanup() {
  local code=$?
  # 两个后台任务都用 setsid 起了自己的进程组：`npm run dev` 会再派 `sh -c vite`，
  # 只杀直接子进程会把 node 留在 5174 上，下一次跑就撞端口。杀整组。
  for pid in "$VITE_PID" "$DAEMON_PID"; do
    [ -n "$pid" ] || continue
    kill -- "-$pid" 2>/dev/null || kill "$pid" 2>/dev/null
  done
  for pid in "$VITE_PID" "$DAEMON_PID"; do
    [ -n "$pid" ] || continue
    wait "$pid" 2>/dev/null
  done
  # 只在真的起过东西时检查释放（端口被别人的进程占着是启动前就该报错的情形）。
  if [ -n "$VITE_PID" ] || [ -n "$DAEMON_PID" ]; then
    for port in "$VITE_PORT" "$DAEMON_PORT"; do
      for _ in $(seq 1 20); do
        port_busy "$port" || break
        sleep 0.2
      done
      port_busy "$port" && log "警告：端口 $port 仍未释放，请手动确认"
    done
  fi
  if [ -n "$PECO_HOME" ] && [ -d "$PECO_HOME" ]; then
    if [ "${E2E_KEEP_LOGS:-0}" = "1" ]; then
      log "保留日志：$PECO_HOME/logs"
    else
      rm -rf "$PECO_HOME"
    fi
  fi
  exit "$code"
}
trap cleanup EXIT INT TERM

for port in "$DAEMON_PORT" "$VITE_PORT"; do
  if port_busy "$port"; then
    log "端口 $port 已被占用：本脚本只起自己那一套，不去动别人的进程。先让开端口再跑。"
    exit 2
  fi
done

PECO_HOME="$(mktemp -d /tmp/peco-webui-e2e.XXXXXX)"
LOGS="$PECO_HOME/logs"
mkdir -p "$LOGS"

log "构建 daemon"
cargo build -p peco-daemon --quiet --manifest-path "$REPO/Cargo.toml" || {
  log "cargo build -p peco-daemon 失败"
  exit 1
}

log "启动 daemon（PECO_HOME=$PECO_HOME，bind 127.0.0.1:$DAEMON_PORT）"
setsid env PECO_HOME="$PECO_HOME" PECO_DAEMON_BIND_ADDR="127.0.0.1:$DAEMON_PORT" \
  "$REPO/target/debug/peco-daemon" >"$LOGS/daemon.log" 2>&1 &
DAEMON_PID=$!

for _ in $(seq 1 120); do
  curl -sf "http://127.0.0.1:${DAEMON_PORT}/api/peco" >/dev/null 2>&1 && break
  kill -0 "$DAEMON_PID" 2>/dev/null || { log "daemon 起不来，日志尾部："; tail -20 "$LOGS/daemon.log"; exit 1; }
  sleep 0.5
done
curl -sf "http://127.0.0.1:${DAEMON_PORT}/api/peco" >/dev/null 2>&1 || {
  log "等 /api/peco 超时，日志尾部："; tail -20 "$LOGS/daemon.log"; exit 1
}
log "daemon 就绪：$(curl -s "http://127.0.0.1:${DAEMON_PORT}/api/peco")"

log "启动 dev server（端口 $VITE_PORT）"
setsid bash -c 'cd "$1" && npm run dev -- --port "$2" --strictPort' _ "$WEBUI" "$VITE_PORT" \
  >"$LOGS/vite.log" 2>&1 &
VITE_PID=$!

for _ in $(seq 1 120); do
  curl -sf "$BASE_URL/" >/dev/null 2>&1 && break
  kill -0 "$VITE_PID" 2>/dev/null || { log "dev server 起不来，日志尾部："; tail -20 "$LOGS/vite.log"; exit 1; }
  sleep 0.5
done
curl -sf "$BASE_URL/" >/dev/null 2>&1 || {
  log "等 $BASE_URL 超时，日志尾部："; tail -20 "$LOGS/vite.log"; exit 1
}
log "dev server 就绪"

log "写入演示数据"
python3 "$HERE/seed.py" --home "$PECO_HOME" || { log "seed 失败"; exit 1; }

rm -rf "$HERE/.report"
log "跑断言（xvfb-run + headless:false）"
(
  cd "$WEBUI" || exit 1
  E2E_BASE_URL="$BASE_URL" E2E_VITE_PID="$VITE_PID" \
    xvfb-run -a npx playwright test --config "$HERE/playwright.config.ts"
)
STATUS=$?

echo
log "playwright 退出码 $STATUS"
if [ "$STATUS" -ne 0 ]; then
  log "有断言未通过（这是测量台：非零退出码 = 实测与规格不符，结果见上方表格）"
  log "daemon 日志尾部："
  tail -5 "$LOGS/daemon.log"
else
  log "全部断言通过"
fi
exit "$STATUS"
