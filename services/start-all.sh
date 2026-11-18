#!/usr/bin/env bash
# ============================================================================
# 一键启动工作台的全部后台服务（Linux / macOS / Git Bash）
#
#   用法：  bash start-all.sh        # 前台运行，Ctrl+C 全部停止
#           bash start-all.sh &      # 后台运行
#
# 只启动本机还没在监听的端口，已经在跑的会跳过 ——
# 这样可以和「工作台自己拉起的服务」共存，不会重复起两份。
# ============================================================================
set -u

cd "$(dirname "$0")" || exit 1

# ---- 可调配置（也可以用环境变量覆盖） ----
: "${SIYUAN_API_URL:=http://127.0.0.1:6806}"
: "${SIYUAN_TOKEN:=}"
: "${SCRAPE_CHROME:=}"
: "${ZHIHU_CLI:=}"
: "${PLAYWRIGHT_BROWSERS_PATH:=}"

export SIYUAN_API_URL SIYUAN_TOKEN
[ -n "$SCRAPE_CHROME" ] && export SCRAPE_CHROME
[ -n "$ZHIHU_CLI" ] && export ZHIHU_CLI
[ -n "$PLAYWRIGHT_BROWSERS_PATH" ] && export PLAYWRIGHT_BROWSERS_PATH

PY="${PYTHON:-python3}"
command -v "$PY" >/dev/null 2>&1 || PY=python
command -v "$PY" >/dev/null 2>&1 || { echo "[错误] 找不到 python，请先安装 Python 3.9+"; exit 1; }
command -v node >/dev/null 2>&1 || { echo "[错误] 找不到 node，请先安装 Node.js 20+"; exit 1; }

PIDS=()

port_open() {
    # $1 = 端口；用 bash 的 /dev/tcp 探测，不依赖 nc/lsof
    (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null && { exec 3>&- 3<&-; return 0; }
    return 1
}

start() {
    local name="$1" port="$2"; shift 2
    if port_open "$port"; then
        echo "  [跳过] $name —— 端口 $port 已在监听"
        return
    fi
    echo "  [启动] $name  (端口 $port)"
    "$@" >"logs-$name.log" 2>&1 &
    PIDS+=($!)
}

cleanup() {
    echo ""
    echo "正在停止所有服务…"
    for pid in "${PIDS[@]:-}"; do
        kill "$pid" 2>/dev/null
    done
    wait 2>/dev/null
    echo "已停止。"
    exit 0
}
trap cleanup INT TERM

echo "工作台后台服务"
echo "  思源内核：$SIYUAN_API_URL"
echo "  脚本目录：$(pwd)"
echo ""

start regen-server    3918 node regen-server.js
start zhihu-bridge    3921 "$PY" zhihu_bridge.py
start invoice-parser  3923 "$PY" invoice_parser.py
start scrapling-fetch 3920 "$PY" scrapling_fetch.py

echo ""
echo "全部已启动。日志：logs-*.log"
echo "按 Ctrl+C 停止。"
wait
