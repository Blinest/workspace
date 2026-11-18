# 服务目录

工作台的四个后台服务都放在这个目录里。它们都是**独立的 HTTP 服务**，
既可以由工作台自动拉起，也可以在服务器上单独启动。

| 端口 | 脚本 | 作用 | 运行环境 | 依赖 |
|---|---|---|---|---|
| 3918 | `regen-server.js` | 周报总结 / 番茄钟周报数据 | Node.js | 无（纯标准库） |
| 3920 | `scrapling_fetch.py` | 网页正文抓取（剪藏助手） | Python | `scrapling` `lxml` `markdownify` `playwright` |
| 3921 | `zhihu_bridge.py` | 知乎搜索 / 热榜 | Python | `zhihu-cli.exe` |
| 3923 | `invoice_parser.py` | 发票 PDF 解析 | Python | `pymupdf` `rapidocr_onnxruntime` |

---

## 一、在服务器上启动

### 1. 装依赖

```bash
# Node 服务不需要额外包，只要装了 Node 20+
node --version

# Python 服务
pip install -r requirements.txt

# 剪藏服务的浏览器内核（只需一次，约 300MB）
python -m playwright install chromium
```

### 2. 配置

所有配置都走**环境变量**，不再写死本机路径：

| 变量 | 用于 | 说明 |
|---|---|---|
| `SCRAPE_CHROME` | 3920 | Chromium 可执行文件路径。留空则用 Playwright 默认位置 |
| `PLAYWRIGHT_BROWSERS_PATH` | 3920 | Playwright 浏览器缓存目录（默认 `~/.cache/ms-playwright`） |
| `ZHIHU_CLI` | 3921 | `zhihu-cli.exe` 路径。留空则在脚本同目录 / `bin/` / PATH 里找 |
| `SIYUAN_API_URL` | 3918 | 思源内核地址（默认 `http://127.0.0.1:6806`） |
| `SIYUAN_TOKEN` | 3918 | 思源 API Token |

> **注意**：`regen-config.json` 里可能存着你的私有 token。
> 发布给别人时**不要带上这个文件**，让对方用环境变量配置，或者在界面上重设。

### 3. 启动

```bash
# 一键全起（Linux / macOS）
bash start-all.sh

# 一键全起（Windows）
start-all.bat

# 或者单独起
node regen-server.js
python scrapling_fetch.py
python zhihu_bridge.py
python invoice_parser.py
```

### 4. 验证

```bash
curl http://127.0.0.1:3918/health      # 周报
curl "http://127.0.0.1:3920/fetch?url=https://example.com"   # 抓取
curl "http://127.0.0.1:3921/search?q=测试"                    # 知乎
curl http://127.0.0.1:3923/            # 发票
```

---

## 二、端口与安全

**四个服务都只监听 `127.0.0.1`**，默认不接受外部连接。

如果确实要让远程客户端使用（比如剪藏跑在服务器上、工作台在另一台机器）：

1. 改监听地址为 `0.0.0.0`（各脚本里 `ThreadingHTTPServer(("127.0.0.1", PORT), ...)` → `"0.0.0.0"`）
2. **务必**在前面加一层反向代理（Nginx / Caddy）并配好认证
3. 这些服务本身**没有鉴权**，直接暴露到公网等于把抓取能力开放给任何人

---

## 三、工作台如何找到这些服务

工作台启动时会按顺序找脚本：

```
1. exe 同级 services/      ← 安装包
2. exe 同级 scripts/       ← 绿色版
3. <项目根>/scripts        ← 开发期
4. 老路径 dsdata/          ← 兼容旧环境
```

找到哪个就用它的**所在目录**作为工作进程的工作目录
（周报服务靠 `__dirname` 定位 `capacity.json`，必须在同一目录）。

如果工作台已经能连上你在服务器上起的服务，它检测到端口占用就会跳过启动，直接用现成的。

---

## 四、前端连哪个地址

前端默认连 `127.0.0.1`。要让工作台连服务器上的服务，改这里：

| 数据 | 存储键 | 示例 |
|---|---|---|
| 剪藏抓取服务 | `dsh.worktable.scrape.v1` | `{"base":"http://your-server:3920"}` |

设置方式：主界面「剪藏助手」→「设置」→「抓取服务」填地址。

其余三个服务（3918 / 3921 / 3923）目前是本地兜底，如需远程化需要同步改前端的地址常量。

---

## 五、文件说明

| 文件 | 说明 |
|---|---|
| `regen-server.js` | 周报数据服务，会读写同目录的 `capacity.json` |
| `capacity.json` | 近期执行力缓存（可删，会重建） |
| `regen-config.json` | 思源连接配置（**含 token，发布时别带**） |
| `scrapling_fetch.py` | 抓取服务 |
| `zhihu_bridge.py` | 知乎桥接 |
| `zhihu-cli.exe` | 知乎命令行工具（如果放这里，`zhihu_bridge.py` 会自动找到） |
| `invoice_parser.py` | 发票解析 |
| `requirements.txt` | Python 依赖 |
| `start-all.sh` / `start-all.bat` | 一键启动全部服务 |
