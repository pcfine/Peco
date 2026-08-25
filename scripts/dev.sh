#!/usr/bin/env bash
# ============================================================================
# peco 开发调试一键启动脚本
# ============================================================================
# 用法:
#   bash scripts/dev.sh
#   bash scripts/dev.sh --backend-only     # 只启动后端
#   bash scripts/dev.sh --frontend-only    # 只启动前端
# ============================================================================

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ── 颜色 ──────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; BOLD='\033[1m'; NC='\033[0m'
log_info() { printf "  ${GREEN}[INFO]${NC} %s\n" "$*"; }

# ── 解析参数 ──────────────────────────────────────────────────────────────
BACKEND=true
FRONTEND=true
while [[ $# -gt 0 ]]; do
    case "$1" in
        --backend-only) FRONTEND=false; shift ;;
        --frontend-only) BACKEND=false; shift ;;
        -h|--help)
            echo "Usage: bash scripts/dev.sh [--backend-only|--frontend-only]"
            echo ""
            echo "Starts both backend (peco-server) and frontend (webui) for development."
            echo "Ctrl+C to stop all services."
            exit 0
            ;;
        *) echo "Unknown option: $1 (use --help)"; exit 1 ;;
    esac
done

# ── 加载 .env ──────────────────────────────────────────────────────────────
cd "$REPO_DIR"
if [[ -f .env ]]; then
    set -a; source .env; set +a
    log_info "Loaded .env"
elif [[ -f crates/peco-core/.env ]]; then
    set -a; source crates/peco-core/.env; set +a
    log_info "Loaded crates/peco-core/.env"
fi

if [[ -z "${DEEPSEEK_API_KEY:-}" ]]; then
    echo "  [WARN] DEEPSEEK_API_KEY not set. Set it in .env or export it."
fi

# ── 清理函数 ──────────────────────────────────────────────────────────────
cleanup() {
    printf "\n"
    log_info "Shutting down..."
    [[ -n "${BACKEND_PID:-}" ]] && kill "$BACKEND_PID" 2>/dev/null && log_info "Backend stopped."
    wait 2>/dev/null || true
    log_info "All services stopped."
}
trap cleanup EXIT INT TERM

# ── 启动后端 ──────────────────────────────────────────────────────────────
if $BACKEND; then
    printf "\n${BOLD}${BLUE}=== Starting peco-server (backend) ===${NC}\n"
    log_info "Building & running peco-server..."
    log_info "API  : http://localhost:9227"
    log_info "Docs : http://localhost:9227/docs"

    # 启用 debug 日志：tracing_subscriber 通过 RUST_LOG 读取过滤级别。
    # 默认打开本项目 crate 的 debug 日志（含 AgentLooper 状态变化），依赖保持 info。
    export RUST_LOG="${RUST_LOG:-peco_core=debug,peco_server=debug,tower_http=info}"
    log_info "RUST_LOG=$RUST_LOG"

    # 排查 LLM 调用（model-provider）时追加 model_provider 的日志：
    #   RUST_LOG="...,model_provider=debug" bash scripts/dev.sh
    #     → 打印每次请求/响应摘要（input_items 计数、tokens、latency、SSE 流终止统计等）。
    #   RUST_LOG="...,model_provider=trace" bash scripts/dev.sh
    #     → 在 debug 基础上再打印完整请求体/响应体原文（含用户对话内容，谨慎使用）。
    # 注意：model_provider 不以 `peco` 开头，不会命中上面的默认 directive；不显式列出
    # 时它会落到 EnvFilter 的 ERROR 默认级别，于是该层 warn/debug 全部静默。

    cargo run -p peco-server &
    BACKEND_PID=$!
    sleep 2

    if ! kill -0 "$BACKEND_PID" 2>/dev/null; then
        echo "  [ERROR] Backend failed to start. Check output above."
        exit 1
    fi
    log_info "Backend running (PID: $BACKEND_PID)"
fi

# ── 启动前端 ──────────────────────────────────────────────────────────────
if $FRONTEND; then
    printf "\n${BOLD}${BLUE}=== Starting webui (frontend) ===${NC}\n"
    cd "$REPO_DIR/webui"

    log_info "Installing dependencies (if needed)..."
    npm install --silent 2>/dev/null || npm install

    log_info "Starting Vite dev server..."
    log_info "Web UI: http://localhost:9233"

    npx vite --host 0.0.0.0 &
    FRONTEND_PID=$!

    printf "\n"
    printf "${GREEN}========================================${NC}\n"
    printf "${GREEN}  peco dev environment ready!${NC}\n"
    printf "${GREEN}========================================${NC}\n"
    $BACKEND && printf "  Backend API : ${BOLD}http://localhost:9227${NC}\n"
    $BACKEND && printf "  API Docs    : ${BOLD}http://localhost:9227/docs${NC}\n"
    printf "  Web UI      : ${BOLD}http://localhost:9233${NC}\n"
    printf "\n${YELLOW}  Press Ctrl+C to stop all services${NC}\n"
    printf "\n"

    wait
fi

# 如果只启动后端，前端没启动的话，在这里等待
if $BACKEND && ! $FRONTEND; then
    printf "\n${YELLOW}Press Ctrl+C to stop${NC}\n"
    wait "$BACKEND_PID"
fi
