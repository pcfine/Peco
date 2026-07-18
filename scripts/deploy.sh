#!/usr/bin/env bash
# ============================================================================
# peco 一键部署脚本
# ============================================================================
# 用法:
#   sudo bash scripts/deploy.sh
#   DEEPSEEK_API_KEY=sk-xxx PECO_JWT_SECRET=xxx sudo -E bash scripts/deploy.sh
#   sudo bash scripts/deploy.sh --uninstall
# ============================================================================

set -euo pipefail

# 捕获任何错误，输出行号方便排查
trap 'echo "[FATAL] Script failed at line $LINENO" >&2' ERR

# 确保有输出（有些终端可能缓冲）
printf "=== peco deploy start ===\n\n"

# ── 颜色 ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; BOLD='\033[1m'; NC='\033[0m'
log_info()  { printf "  ${GREEN}[INFO]${NC} %s\n" "$*"; }
log_warn()  { printf "  ${YELLOW}[WARN]${NC} %s\n" "$*"; }
log_error() { printf "  ${RED}[ERROR]${NC} %s\n" "$*"; }
die()       { log_error "$*"; exit 1; }
step()      { printf "\n${BOLD}${BLUE}[%s]${NC}\n" "$*"; }

# ── 修复 sudo 下找不到 cargo/node 的问题 ──────────────────────────────────
# sudo 重置了 PATH，rustup/nvm 装在用户 HOME 下，需手动加入
fix_sudo_env() {
    if [[ -n "${SUDO_USER:-}" ]]; then
        USER_HOME="$(eval echo ~"$SUDO_USER")"
        # cargo (rustup)
        [[ -d "$USER_HOME/.cargo/bin" ]] && export PATH="$USER_HOME/.cargo/bin:$PATH" || true
        # nvm node
        for nvm_dir in "$USER_HOME/.nvm/versions/node"/*/bin; do
            [[ -d "$nvm_dir" ]] && export PATH="$nvm_dir:$PATH" || true
        done
        # fnm node
        if [[ -d "$USER_HOME/.local/share/fnm" ]]; then
            export PATH="$USER_HOME/.local/share/fnm:$PATH"
        fi
        # volta node
        [[ -d "$USER_HOME/.volta/bin" ]] && export PATH="$USER_HOME/.volta/bin:$PATH" || true
    fi
}

# ── 默认配置 ──────────────────────────────────────────────────────────────
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
printf "  Script: %s\n  Repo:   %s\n\n" "${BASH_SOURCE[0]}" "$REPO_DIR"
INSTALL_BIN="${INSTALL_BIN:-/usr/local/bin/peco-server}"
WEB_ROOT="${WEB_ROOT:-/var/www/peco-webui}"
DATA_DIR="${DATA_DIR:-/var/lib/peco}"
SERVER_HOST="${PECO_SERVER_HOST:-127.0.0.1}"
SERVER_PORT="${PECO_SERVER_PORT:-3000}"
SYSTEMD_USER="${SYSTEMD_USER:-www-data}"
SYSTEMD_GROUP="${SYSTEMD_GROUP:-www-data}"
NGINX_LISTEN="${NGINX_LISTEN:-80}"

# ── 解析参数 ──────────────────────────────────────────────────────────────
UNINSTALL=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --uninstall) UNINSTALL=true; shift ;;
        -h|--help)
            echo "Usage: sudo bash scripts/deploy.sh [--uninstall]"
            echo ""
            echo "Environment variables (all optional, will prompt if missing):"
            echo "  DEEPSEEK_API_KEY      DeepSeek API key"
            echo "  PECO_JWT_SECRET     JWT signing secret (auto-generated if unset)"
            echo "  WEB_ROOT              Frontend dir (default: /var/www/peco-webui)"
            echo "  DATA_DIR              Data dir (default: /var/lib/peco)"
            exit 0
            ;;
        *) die "Unknown option: $1 (use --help)";;
    esac
done

# ── 卸载 ──────────────────────────────────────────────────────────────────
if $UNINSTALL; then
    step "Uninstalling peco..."
    systemctl stop peco-server 2>/dev/null || true
    systemctl disable peco-server 2>/dev/null || true
    rm -f /etc/systemd/system/peco-server.service
    systemctl daemon-reload 2>/dev/null || true
    rm -f /etc/nginx/sites-enabled/peco /etc/nginx/sites-available/peco
    nginx -t &>/dev/null && systemctl reload nginx 2>/dev/null || true
    rm -f "$INSTALL_BIN"
    rm -rf "$WEB_ROOT"
    rm -rf /etc/peco-server
    log_warn "Data dir kept: $DATA_DIR (remove manually: rm -rf $DATA_DIR)"
    log_info "Uninstall complete."
    exit 0
fi

# ── 检查 ──────────────────────────────────────────────────────────────────
printf "  UID=%s SUDO_USER=%s\n" "$(id -u)" "${SUDO_USER:-<none>}"
[[ "$(id -u)" == "0" ]] || die "Must run as root: sudo bash $0"
fix_sudo_env
printf "  HOME=%s PATH=%s\n" "${HOME:-}" "${PATH:-}"

step "1/7" "Checking tools..."
for cmd in cargo node npm nginx; do
    if command -v "$cmd" &>/dev/null; then
        log_info "$cmd: $(command -v "$cmd")"
    else
        log_warn "$cmd not found in PATH (build will fail)"
    fi
done

# ── 加载环境变量 ──────────────────────────────────────────────────────────
step "2/7" "Loading config..."
if [[ -f "$REPO_DIR/.env" ]]; then set -a; source "$REPO_DIR/.env"; set +a; log_info "Loaded .env"; fi

if [[ -z "${DEEPSEEK_API_KEY:-}" ]]; then
    read -r -p "  DEEPSEEK_API_KEY: " DEEPSEEK_API_KEY
    [[ -z "$DEEPSEEK_API_KEY" ]] && die "DEEPSEEK_API_KEY is required."
fi

if [[ -z "${PECO_JWT_SECRET:-}" ]]; then
    PECO_JWT_SECRET="$(openssl rand -hex 32)"
    log_warn "PECO_JWT_SECRET auto-generated: ${PECO_JWT_SECRET:0:16}..."
fi

# ── 编译后端 ──────────────────────────────────────────────────────────────
step "3/7" "Building peco-server (Rust)..."
cd "$REPO_DIR"
sudo -u "$SUDO_USER" env HOME="$USER_HOME" PATH="$PATH" cargo build --release -p peco-server
log_info "Build done: target/release/peco-server"

# ── 编译前端 ──────────────────────────────────────────────────────────────
step "4/7" "Building webui (React)..."
cd "$REPO_DIR/webui"
sudo -u "$SUDO_USER" env HOME="$USER_HOME" PATH="$PATH" npm ci --prefer-offline
sudo -u "$SUDO_USER" env HOME="$USER_HOME" PATH="$PATH" npm run build
log_info "Build done: webui/dist/"

# ── 安装 ──────────────────────────────────────────────────────────────────
step "5/7" "Installing files..."
cp "$REPO_DIR/target/release/peco-server" "$INSTALL_BIN"
chmod 755 "$INSTALL_BIN"
log_info "Binary: $INSTALL_BIN"

mkdir -p "$WEB_ROOT"
rm -rf "${WEB_ROOT:?}"/*
cp -r "$REPO_DIR/webui/dist"/* "$WEB_ROOT/"
log_info "Static files: $WEB_ROOT"

mkdir -p "$DATA_DIR" "$DATA_DIR/sessions"
id "$SYSTEMD_USER" &>/dev/null || useradd --system --no-create-home --shell /usr/sbin/nologin "$SYSTEMD_USER"
chown -R "$SYSTEMD_USER:$SYSTEMD_GROUP" "$DATA_DIR" "$WEB_ROOT"
chmod 750 "$DATA_DIR"
log_info "Data dir: $DATA_DIR"

# ── 写入配置 ──────────────────────────────────────────────────────────────
step "6/7" "Writing service configs..."

# systemd
mkdir -p /etc/peco-server
cat > /etc/peco-server/env <<EOF
DEEPSEEK_API_KEY=$DEEPSEEK_API_KEY
PECO_JWT_SECRET=$PECO_JWT_SECRET
PECO_SERVER_HOST=$SERVER_HOST
PECO_SERVER_PORT=$SERVER_PORT
PECO_DATA_DIR=$DATA_DIR
PECO_DATABASE_URL=sqlite:$DATA_DIR/server.db?mode=rwc
RUST_LOG=peco_server=info,tower_http=warn
EOF
chmod 600 /etc/peco-server/env

cat > /etc/systemd/system/peco-server.service <<EOF
[Unit]
Description=peco AI Agent Server
After=network.target

[Service]
Type=simple
User=$SYSTEMD_USER
Group=$SYSTEMD_GROUP
WorkingDirectory=$DATA_DIR
EnvironmentFile=/etc/peco-server/env
ExecStart=$INSTALL_BIN
Restart=always
RestartSec=5
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=$DATA_DIR

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
log_info "systemd service written"

# nginx
cat > /etc/nginx/sites-available/peco <<NGINX
server {
    listen $NGINX_LISTEN;
    server_name _;
    root $WEB_ROOT;
    index index.html;

    location / {
        try_files \$uri \$uri/ /index.html;
    }

    location /api/ {
        proxy_pass http://$SERVER_HOST:$SERVER_PORT;
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
        proxy_buffering off;
        proxy_cache off;
        proxy_read_timeout 3600s;
    }

    location /docs {
        proxy_pass http://$SERVER_HOST:$SERVER_PORT;
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
    }

    location /api-docs/ {
        proxy_pass http://$SERVER_HOST:$SERVER_PORT;
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
    }

    location /assets/ {
        expires 1y;
        add_header Cache-Control "public, immutable";
    }

    gzip on;
    gzip_types text/plain text/css application/json application/javascript text/xml application/xml;
    gzip_min_length 1000;
}
NGINX

ln -sf /etc/nginx/sites-available/peco /etc/nginx/sites-enabled/peco
log_warn "Removing default nginx site (if this server hosts other sites, re-enable manually)"
rm -f /etc/nginx/sites-enabled/default
nginx -t || die "Nginx config test failed"
log_info "Nginx config written"

# ── 启动 ──────────────────────────────────────────────────────────────────
step "7/7" "Starting services..."
systemctl enable peco-server
systemctl restart peco-server
systemctl reload nginx 2>/dev/null || systemctl start nginx

log_info "Waiting for server..."
sleep 3
if systemctl is-active --quiet peco-server; then
    log_info "peco-server is running."
else
    log_warn "Service may not have started. Check: journalctl -u peco-server -n 30"
fi

echo ""
log_info "========================================"
log_info "  Deployment complete!"
log_info "========================================"
log_info "  Web UI : http://localhost:$NGINX_LISTEN"
log_info "  API    : http://$SERVER_HOST:$SERVER_PORT"
log_info "  Docs   : http://$SERVER_HOST:$SERVER_PORT/docs"
echo ""
log_info "Manage:  systemctl [status|restart|stop] peco-server"
log_info "Logs:    journalctl -u peco-server -f"
echo ""
