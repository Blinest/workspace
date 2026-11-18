#!/usr/bin/env node
/* ============================================================================
 * 思源科研工作台 · 桌面版打包脚本
 * ----------------------------------------------------------------------------
 * 一条命令从「交付包」生成 Windows 单文件 exe：
 *     node build-desktop.js
 *
 * 做四件事：
 *   1. 取 projects/思源科研工作台_v1.0.0/ 的成品（唯一事实来源，避免两份代码打架）
 *   2. 把 dshell.css 内联进 HTML —— 变成真正的单文件
 *   3. 用 esbuild 压缩混淆 JS（去掉注释、缩短变量名，右键看源码是一团乱码）
 *   4. cargo 编译成 Rust 二进制，产物复制到 dist/思源科研工作台.exe
 *
 * 本机注意：CARGO_TARGET_DIR 必须指向 D 盘（C 盘只剩 9 GB）。
 * ==========================================================================*/
"use strict";
const fs = require("fs");
const path = require("path");
const os = require("os");
const { spawnSync } = require("child_process");

const HERE = __dirname;
const DS_ROOT = path.join(HERE, "..");                    // → projects/
const WORKSPACE = path.join(HERE, "..", "..");            // → dsdata/（工作台根，用来找 esbuild）
const PKG = path.join(DS_ROOT, "思源科研工作台_v1.0.0");
const SRC_HTML = path.join(PKG, "思源科研工作台.html");
const SRC_CSS = path.join(PKG, "dshell.css");
const FRONTEND = path.join(HERE, "frontend");
const SRC_TAURI = path.join(HERE, "src-tauri");
const DIST = path.join(HERE, "dist");
const TARGET_DIR = process.env.CARGO_TARGET_DIR || path.join(DS_ROOT, "_cargo_target");
const BIN_NAME = "siyuan-research-worktable.exe";
const FINAL_NAME = "思源科研工作台.exe";

const NO_OBF = process.argv.includes("--no-obfuscate");

/* esbuild：优先用本机已有的那份（工作台项目自带），没有就退回 npx */
function findEsbuild() {
  const c = [
    path.join(WORKSPACE, "dsh-worktable", "dsh-worktable-main", "01_content", "node_modules", "@esbuild", "win32-x64", "esbuild.exe"),
    path.join(WORKSPACE, "node_modules", "@esbuild", "win32-x64", "esbuild.exe"),
    path.join(WORKSPACE, "node_modules", "esbuild", "bin", "esbuild")
  ];
  return c.find(p => { try { return fs.existsSync(p); } catch (e) { return false; } }) || null;
}

/* 去掉 HTML 注释 —— 注释会告诉改包的人「哪里是水印、哪里是入口」。
   先把 <script>/<style> 整块摘出来保护起来，只清真正裸露的 HTML 注释。
   带版本号的那条版权戳保留（那是署名，不是给人看的说明书）。 */
function stripHtmlComments(html) {
  const guards = [];
  let out = html.replace(/<(script|style)\b[^>]*>[\s\S]*?<\/\1>/gi, m => {
    guards.push(m);
    return "\u0000G" + (guards.length - 1) + "\u0000";
  });
  out = out.replace(/<!--[\s\S]*?-->/g, m => (/思源科研工作台 v\d/.test(m) ? m : ""));
  out = out.replace(/\u0000G(\d+)\u0000/g, (m, i) => guards[Number(i)]);
  return out;
}

function assert(cond, msg) { if (!cond) { console.error("✗ " + msg); process.exit(1); } }

/* ========================== exe 版使用说明 ========================== */

function buildDesktopReadme() {
  return `# 思源科研工作台 · 桌面版 v1.0.0

一个独立窗口的科研工作台：把你思源笔记里的文档、标签、数据库汇总起来看、查、导出、交给 AI 分析。
**单文件 exe，不装任何东西，不联网上传，数据只在你本机。**

---

## 一、第一次打开（30 秒）

### 1. 双击 思源科研工作台.exe

> **如果弹出蓝色的「Windows 已保护你的电脑」**
> 这是 Windows 对所有**未购买数字签名证书**的个人软件的默认拦截，不是病毒提示。
> 点左边小字 **「更多信息」** → 再点 **「仍要运行」**，就这一次，以后不再问。

### 2. 装并打开思源笔记
到 <https://b3log.org/siyuan> 下载安装（免费）。打开你要统计的那个工作区。

### 3. 拿你自己的 API 令牌
思源里：**设置 → 关于 → API 令牌**，点「复制」。

### 4. 填进工作台
第一次打开会自动落在「设置」页 —— 把令牌粘进「API 令牌」框，点 **保存并加载**。
看到顶部变成绿色的 **✓ 已连接** 就成功了。

> 令牌只存在你自己电脑的本地配置里，不会发给任何服务器。

---

## 二、里面有什么

| 页签 | 干什么 |
|---|---|
| **学习路线** | 把当前学习主题聚成一条分阶段路线（接 AI 后可按你的笔记实时生成） |
| **知识图谱** | 全库文档 / 主题 / 标签 / 字符数总览 + 可缩放的关系图，点节点下钻 |
| **数据库汇总** | 汇总思源里所有「数据库」：行数列数、AI 提炼的关键数字、统计与分类分布、整表查看、导出 XLSX / CSV、一键跳回思源对应位置 |
| **周报总结 / 剪藏助手** | 可选模块，桌面版未内置 |

### 数据库汇总的几个实用点
- **一屏看完**：点任意数据库的「展开数据」，自动聚焦到这张表并压缩排版，尽量不滚动就看全
- **关键数字**：每个库自动提炼 3~5 条「短标签：真实数字」
- **空白字段**：切到「空白字段」页，列出哪些列还没填，并**直接写回思源**
- **导出**：单库导出 XLSX（数据 + 统计两张表），也能一键导出全部

---

## 三、接上 AI（可选，但强烈建议）

「数据库汇总」的 AI 提炼、「学习路线」的 AI 生成，都需要一个 **OpenAI 兼容**的对话接口。
在「设置 → AI 学习路线引擎」里填接口地址、模型名、API Key。

留空也能用 —— 会自动退回**本地统计摘要**（不花一分钱，只是文字没那么顺）。

---

## 四、常见问题

**Q：会不会把我的笔记传到网上？**
不会。只有你主动点「AI 提炼 / AI 生成路线」时，才会把**当前这张表的统计数据**（列名、合计、分布，不是全文）发给你自己配置的模型接口。不配 AI Key 就完全不联网。

**Q：杀毒软件报毒 / 提示未知发布者？**
未签名软件的常见现象。可以到 VirusTotal 上传自查。介意的话请改用网页版。

**Q：思源没开 / 端口不是 6806？**
默认 \`http://127.0.0.1:6806\`。思源关着读不到数据；端口不同就在「设置」里改「API 地址」。

**Q：显示 0 个文档？**
① 确认思源正在运行；② 点「测试连接」看报什么错；③ 令牌复制时不要带空格。

**Q：换电脑了？**
把 exe 拷过去，令牌重新填一次即可。配置存在本机，不跟着文件走。

**Q：能放在 U 盘里用吗？**
可以，这个 exe 是绿色免安装的。

**Q：能商用 / 二次分发吗？**
可以用于你自己的学习和工作。**请勿二次分发、转售或改包后发布。**

---

## 五、文件清单

\`\`\`
思源科研工作台.exe   ← 全部内容都在这一个文件里，双击即用
使用说明.md          ← 本文件
\`\`\`

---

© 2026 作者保留所有权利。
`;
}

/* ============================================================================
 * 擦掉二进制里的本机构建路径
 * ----------------------------------------------------------------------------
 * tauri-codegen 会把 crate 的绝对路径拼进权限表那个大字符串字面量里，
 * --remap-path-prefix 管不到宏生成的字面量，只能在产物上做字节级替换。
 * ⚠ 替换串必须与原文**等长** —— 否则后面所有偏移错位，exe 直接废掉。
 * ==========================================================================*/
function scrubLocalPaths(exePath) {
  const buf = fs.readFileSync(exePath);
  const targets = [WORKSPACE, "5_2_privdata", "5_database", "0_7_output", "0_study", "_kami_regen"];
  let total = 0;
  for (const t of targets) {
    const from = Buffer.from(t, "utf8");
    const to = Buffer.alloc(from.length, 0x2e);        // 等长的点
    let i = 0;
    for (;;) {
      i = buf.indexOf(from, i);
      if (i < 0) break;
      to.copy(buf, i);
      total++;
      i += from.length;
    }
  }
  if (total) fs.writeFileSync(exePath, buf);
  return total;
}

function main() {
  console.log("═══ 思源科研工作台 · 桌面版打包 ═══");
  assert(fs.existsSync(SRC_HTML), "找不到交付包：" + SRC_HTML + "\n先跑 node build-package.js");
  assert(fs.existsSync(SRC_CSS), "找不到样式表：" + SRC_CSS);

  let html = fs.readFileSync(SRC_HTML, "utf8");
  const css = fs.readFileSync(SRC_CSS, "utf8");
  const before = Buffer.byteLength(html);

  /* ---- 1. 内联样式表 ---- */
  const link = /<link[^>]*href="dshell\.css"[^>]*>/;
  assert(link.test(html), "HTML 里没找到对 dshell.css 的引用，无法内联");
  html = html.replace(link, "<style>\n" + css + "\n</style>");
  console.log("  ✓ 样式表已内联（" + (css.length / 1024).toFixed(1) + " KB）");

  /* ---- 2. 混淆脚本 ---- */
  const esbuild = findEsbuild();
  if (NO_OBF || !esbuild) {
    console.log("  ⚠ 跳过混淆" + (NO_OBF ? "（--no-obfuscate）" : "（没找到 esbuild）"));
  } else {
    const tmp = path.join(os.tmpdir(), "dsw-obf-" + process.pid);
    fs.mkdirSync(tmp, { recursive: true });
    let n = 0, saved = 0;
    html = html.replace(/<script(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/g, (m, code) => {
      if (!code.trim()) return m;
      const i = n++, inp = path.join(tmp, "in" + i + ".js"), outp = path.join(tmp, "out" + i + ".js");
      fs.writeFileSync(inp, code, "utf8");
      const r = spawnSync(esbuild, [inp, "--minify", "--target=chrome110", "--charset=utf8", "--outfile=" + outp], { encoding: "utf8" });
      if (r.status !== 0 || !fs.existsSync(outp)) { console.log("  ⚠ 第 " + i + " 段脚本混淆失败，保留原文：" + String(r.stderr).slice(0, 200)); return m; }
      const out = fs.readFileSync(outp, "utf8");
      saved += code.length - out.length;
      return "<script>\n" + out + "</script>";
    });
    fs.rmSync(tmp, { recursive: true, force: true });
    console.log("  ✓ 已混淆 " + n + " 段脚本，缩小 " + (saved / 1024).toFixed(1) + " KB");
  }

  /* ---- 3. 去掉 HTML 注释 ---- */
  const beforeStrip = html.length;
  html = stripHtmlComments(html);
  console.log("  ✓ 已清除 HTML 注释（少 " + (beforeStrip - html.length) + " 字节；含水印位置等提示性注释）");

  /* ---- 4. 写进 Tauri 前端目录 ---- */
  fs.mkdirSync(FRONTEND, { recursive: true });
  fs.writeFileSync(path.join(FRONTEND, "index.html"), html, "utf8");
  console.log("  ✓ frontend/index.html  " + (Buffer.byteLength(html) / 1024).toFixed(1) + " KB（原 " + (before / 1024).toFixed(1) + " KB）");

  /* ---- 4. cargo 编译 ---- */
  console.log("  · 开始编译（首次约 10~25 分钟，之后改前端只需几十秒）…");
  const env = Object.assign({}, process.env, { CARGO_TARGET_DIR: TARGET_DIR });
  const r = spawnSync("cargo", ["build", "--release"], { cwd: SRC_TAURI, env, stdio: "inherit", shell: true });
  assert(r.status === 0, "cargo 编译失败（退出码 " + r.status + "）");

  /* ---- 5. 收产物 ---- */
  const built = path.join(TARGET_DIR, "release", BIN_NAME);
  assert(fs.existsSync(built), "编译成功但没找到 exe：" + built);
  fs.mkdirSync(DIST, { recursive: true });
  const finalExe = path.join(DIST, FINAL_NAME);
  fs.copyFileSync(built, finalExe);
  const mb = (fs.statSync(finalExe).size / 1024 / 1024).toFixed(2);
  console.log("  ✓ 产物：" + finalExe + "  " + mb + " MB");

  /* ---- 6. 擦掉二进制里的本机构建路径 ---- */
  const scrubbed = scrubLocalPaths(finalExe);
  console.log(scrubbed
    ? ("  ✓ 已擦除 " + scrubbed + " 处本机构建路径（等长替换，二进制结构不变）")
    : "  · 二进制里没有残留本机路径");

  /* ---- 7. 说明书 + 交付 zip ---- */
  fs.writeFileSync(path.join(DIST, "使用说明.md"), buildDesktopReadme(), "utf8");
  console.log("  ✓ 使用说明.md（含 SmartScreen 提示）");
  const ZIP = path.join(DIST, "思源科研工作台_桌面版_v1.0.0.zip");
  try { fs.rmSync(ZIP, { force: true }); } catch (e) {}
  const sevenZip = ["C:/Program Files/7-Zip/7z.exe", "C:/Program Files (x86)/7-Zip/7z.exe"].find(p => { try { return fs.existsSync(p); } catch (e) { return false; } });
  if (sevenZip) {
    const z = spawnSync(sevenZip, ["a", "-tzip", "-mx=9", ZIP, FINAL_NAME, "使用说明.md"], { cwd: DIST, encoding: "utf8" });
    if (z.status === 0 && fs.existsSync(ZIP)) console.log("  ✓ 交付 zip：" + path.basename(ZIP) + "  " + (fs.statSync(ZIP).size / 1024 / 1024).toFixed(2) + " MB");
    else console.log("  ⚠ zip 打包失败：" + String(z.stderr).slice(0, 200));
  } else {
    console.log("  ⚠ 没找到 7-Zip，跳过 zip（exe 和说明书已在 dist/）");
  }

  console.log("\n下一步：跑 node verify-desktop.js —— 它会把 exe 复制到干净目录真启动一次，逐项验收。");
}

main();
