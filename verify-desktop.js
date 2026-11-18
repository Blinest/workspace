#!/usr/bin/env node
/* ============================================================================
 * 桌面版 exe · 验收脚本（把成品丢进干净目录真跑，不是看编译成功就算过）
 * ----------------------------------------------------------------------------
 * 用法：  node verify-desktop.js
 *
 * 原理：给 WebView2 塞 --remote-debugging-port，就能像验网页一样验 exe 内部。
 * 检查：窗口/标题、配置是否为空、水印是否在、样式是否完整、
 *       以及完整买家旅程（填令牌 → 连接 → 文档/数据库真的出来）。
 * ==========================================================================*/
"use strict";
const fs = require("fs");
const path = require("path");
const os = require("os");
const { spawn, execSync } = require("child_process");

const HERE = __dirname;
/* 约定：默认验 dist 里的 exe；给了参数就验指定的那份（例如从交付 zip 解出来的） */
const EXE = process.argv[2] ? path.resolve(process.argv[2]) : path.join(HERE, "dist", "思源科研工作台.exe");
const TOKEN = (() => {
  try {
    const s = fs.readFileSync(path.join(HERE, "..", "..", "learning-graph.html"), "utf8");
    const m = /siyuanToken:\s*"([^"]+)"/.exec(s);
    return m ? m[1] : "";
  } catch (e) { return ""; }
})();
/* 端口必须随机：WebView2 的调试端口是进程级独占的，上一轮没退干净的
   msedgewebview2 会一直占着固定端口，让下一轮「启动失败」——是验收脚本的坑，不是 exe 的坑。 */
const PORT = 9400 + Math.floor(Math.random() * 400);
const nap = ms => new Promise(r => setTimeout(r, ms));

const results = [];
function check(name, ok, detail) { results.push({ name, ok: !!ok, detail }); return !!ok; }

/* ============================================================================
 * exe 二进制扫描：交付物里绝对不能出现的东西
 * 令牌不写死在这里 —— 从源码里现读，避免验尸工具自己变成泄漏点
 * ==========================================================================*/
function collectSecrets() {
  const out = [];
  const read = p => { try { return fs.readFileSync(p, "utf8"); } catch (e) { return ""; } };
  const root = path.join(HERE, "..", "..");

  const lg = read(path.join(root, "learning-graph.html"));
  let m = /siyuanToken:\s*"([^"]+)"/.exec(lg);
  if (m && m[1]) out.push(["思源 API 令牌", new RegExp(m[1])]);

  const sa = read(path.join(root, "siyuan-assistant.html"));
  const others = [...sa.matchAll(/token:\s*"([a-z0-9]{8,})"/g)].map(x => x[1]).filter(t => !m || t !== m[1]);
  if (others.length) out.push(["其它工作区令牌", new RegExp(others.join("|"))]);

  out.push(["本机绝对路径", /5_2_privdata|5_database|0_study|0_7_output|_kami_regen/]);
  out.push(["私有服务端口", /127\.0\.0\.1:39(18|2[0-4])/]);
  out.push(["个人数据库 / 块 id", /[0-9]{14}-[a-z0-9]{7}/]);
  return out;
}

function scanExe(file) {
  const b = fs.readFileSync(file);
  const utf8 = b.toString("utf8");
  const utf16 = b.toString("utf16le");
  const hits = [];
  for (const [name, re] of collectSecrets()) {
    const n = ((utf8.match(re) || []).length) + ((utf16.match(re) || []).length);
    if (n) {
      const sample = (utf8.match(re) || [])[0] || "";
      hits.push({ name, n, sample: String(sample).slice(0, 60) });
    }
  }
  return hits;
}

(async () => {
  if (!fs.existsSync(EXE)) { console.error("找不到 exe：" + EXE + "\n先跑 node build-desktop.js"); process.exit(1); }

  /* 丢进干净目录（模拟买家下载后解压） */
  const sandbox = path.join(os.tmpdir(), "dsw-exe-" + process.pid);
  fs.rmSync(sandbox, { recursive: true, force: true });
  fs.mkdirSync(sandbox, { recursive: true });
  const runExe = path.join(sandbox, "思源科研工作台.exe");
  fs.copyFileSync(EXE, runExe);
  console.log("验收对象：" + EXE);
  console.log("干净目录：" + sandbox);
  console.log("exe 体积：" + (fs.statSync(runExe).size / 1024 / 1024).toFixed(2) + " MB");

  const env = Object.assign({}, process.env, {
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=" + PORT,
    WEBVIEW2_USER_DATA_FOLDER: path.join(sandbox, "_profile")
  });
  console.log("\n启动 exe …");
  const proc = spawn(runExe, [], { env, detached: false, stdio: "ignore" });

  const getJSON = p => new Promise((res, rej) => {
    require("http").get({ host: "127.0.0.1", port: PORT, path: p }, r => {
      let s = ""; r.on("data", d => s += d); r.on("end", () => { try { res(JSON.parse(s)); } catch (e) { rej(e); } });
    }).on("error", rej);
  });

  let target = null;
  for (let i = 0; i < 120; i++) {
    try {
      const l = await getJSON("/json/list");
      target = l.find(t => t.webSocketDebuggerUrl && (t.type === "page" || t.type === "webview"));
    } catch (e) {}
    if (target && target.url && !/^about:/.test(target.url)) break;
    await nap(400);
  }
  if (!target) {
    check("exe 能启动并打开网页视图", false, "60 秒内没等到 WebView2 目标（可能被 SmartScreen / 杀软拦了）");
    try { execSync("taskkill /PID " + proc.pid + " /T /F", { stdio: "ignore" }); } catch (e) {}
    report(); process.exit(1);
  }
  console.log("网页视图已就绪：" + target.url + "（" + target.type + "）");

  const ws = new WebSocket(target.webSocketDebuggerUrl);
  const pend = new Map(); let id = 0;
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error("ws 连接失败")); });
  ws.onmessage = e => {
    const m = JSON.parse(e.data);
    if (m.id && pend.has(m.id)) { const p = pend.get(m.id); pend.delete(m.id); m.error ? p.rej(new Error(JSON.stringify(m.error))) : p.res(m.result); }
  };
  const send = (method, params) => new Promise((res, rej) => { const i = ++id; pend.set(i, { res, rej }); ws.send(JSON.stringify({ id: i, method, params })); });
  const evl = async expr => {
    const r = await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails).slice(0, 400));
    return r.result.value;
  };

  try {
    /* 让脚本在页面里捕获错误 */
    await evl(`window.__ERR=window.__ERR||[];window.addEventListener('error',function(e){window.__ERR.push('ERR:'+(e.message||''))});window.addEventListener('unhandledrejection',function(e){window.__ERR.push('REJ:'+((e.reason&&(e.reason.message||e.reason))||''))});1`);
    await nap(800);

    const R = await evl(`(function(){
      var g=function(s,p){var e=document.querySelector(s);if(!e)return null;var c=getComputedStyle(e),o={};p.forEach(function(k){o[k]=c[k]});o.w=Math.round(e.getBoundingClientRect().width);o.h=Math.round(e.getBoundingClientRect().height);return o};
      var sb=document.getElementById('setupBar');
      var foot=document.querySelector('.lg-foot');
      return {
        origin: location.origin,
        title: document.title,
        hasApp: !!document.getElementById('dbList'),
        cfgToken: !!((window.DSW_CONFIG||{}).siyuanToken),
        tabOn: (document.querySelector('#tabs .dshell-tab.dshell-tabOn')||{}).textContent,
        guide: ((document.getElementById('testResult')||{}).textContent||'').slice(0,40),
        setupBar: sb ? getComputedStyle(sb).display : 'missing',
        foot: foot ? foot.innerText.replace(/\\s+/g,' ').trim() : null,
        footH: foot ? Math.round(foot.getBoundingClientRect().height) : 0,
        styles: { btn:g('.dshell-btn',['borderRadius','display']), card:g('#pane-settings .dshell-card',['borderRadius','padding']), body:g('body',['backgroundColor','fontSize']) },
        lsWorks: (function(){ try{ localStorage.setItem('__t','1'); var v=localStorage.getItem('__t')==='1'; localStorage.removeItem('__t'); return v; }catch(e){ return 'ERR:'+e.message } })(),
        srcReadable: (function(){
          var s=document.documentElement.outerHTML;
          return {
            len: s.length,
            htmlComments: (s.match(/<!--/g)||[]).length,
            jsBlockComments: (s.match(/\\/\\* -{3,}/g)||[]).length,
            keyFnGone: s.indexOf('function dbCellOf')<0,
            watermarkHintGone: s.indexOf('换成你自己的名字')<0 && s.indexOf('品牌署名')<0
          };
        })(),
        errors: window.__ERR||[]
      };
    })()`);

    console.log("\n页面来源      : " + R.origin);
    console.log("文档标题      : " + R.title);
    console.log("应用已挂载    : " + R.hasApp);
    console.log("配置已清空    : " + (!R.cfgToken));
    console.log("默认页签      : " + R.tabOn + " / 引导：" + R.guide);
    console.log("引导条        : " + R.setupBar);
    console.log("版权水印      : " + JSON.stringify(R.foot) + "  高 " + R.footH + "px");
    console.log("组件样式      : " + JSON.stringify(R.styles));
    console.log("源码可读性    : " + JSON.stringify(R.srcReadable));

    check("exe 能启动并渲染出应用界面", R.hasApp === true, "origin=" + R.origin);
    check("页面来源是本地协议（不是 file://）", /tauri|localhost/.test(R.origin), R.origin);
    check("配置已清空（首次运行无令牌）", R.cfgToken === false, "token=" + R.cfgToken);
    check("首次运行自动跳到「设置」", R.tabOn === "设置", "tab=" + R.tabOn);
    check("全局引导条可见", R.setupBar === "flex", R.setupBar);
    check("版权水印始终可见且不占太多高度", R.foot && /思源科研工作台/.test(R.foot) && R.footH <= 26, "h=" + R.footH + "px");
    check("样式随包生效（按钮胶囊 / 卡片不塌）",
      R.styles.btn.borderRadius === "999px" && R.styles.card.h > 20,
      JSON.stringify(R.styles.btn) + " " + JSON.stringify(R.styles.card));
    check("localStorage 可用（配置能存住）", R.lsWorks === true, String(R.lsWorks));
    check("源码已被混淆（关键函数名不可读）",
      R.srcReadable.keyFnGone === true, JSON.stringify(R.srcReadable));
    check("HTML 注释与水印提示已清除",
      R.srcReadable.htmlComments <= 1 && R.srcReadable.watermarkHintGone === true,
      "html注释=" + R.srcReadable.htmlComments + " 提示已去=" + R.srcReadable.watermarkHintGone);

    if (TOKEN) {
      console.log("\n── 买家旅程：填令牌 → 保存并加载 ──");
      const J = await evl(`(async function(){
        var pause=function(ms){return new Promise(function(r){setTimeout(r,ms)})};
        var waitFor=async function(fn,ms){var t0=Date.now();for(;;){if(fn())return true;if(Date.now()-t0>ms)return false;await pause(200)}};
        var out={};
        document.getElementById('setupGo').click(); await pause(400);
        out.pane=(document.querySelector('.lg-pane.lg-on')||{}).id;
        var inp=document.getElementById('apiToken');
        inp.value=${JSON.stringify(TOKEN)};
        inp.dispatchEvent(new Event('input',{bubbles:true}));
        document.getElementById('btnLoad').click();
        out.connected=await waitFor(function(){return document.getElementById('connBadge').textContent.indexOf('已连接')>=0},60000);
        out.badge=document.getElementById('connBadge').textContent;
        out.barHidden=document.getElementById('setupBar').offsetParent===null;
        await pause(600);
        out.graph=(document.getElementById('graphStats')||{}).innerText.replace(/\\s+/g,' ').trim();
        document.querySelector('#tabs .dshell-tab[data-pane="db"]').click();
        await waitFor(function(){return document.querySelectorAll('#dbList .db-card').length},60000);
        await pause(700);
        out.cards=document.querySelectorAll('#dbList .db-card').length;
        out.first=(document.querySelector('#dbList .db-card .db-pill')||{}).textContent;
        out.aiPts=(document.querySelector('#dbList .db-card .db-aiPts')||{querySelectorAll:function(){return[]}}).querySelectorAll('li').length;
        /* 水印在数据库页也还在？ */
        out.footOnDb=(document.querySelector('.lg-foot')||{}).innerText?'yes':'no';
        out.errors=window.__ERR||[];
        return out;
      })()`);
      console.log("跳转设置      : " + J.pane);
      console.log("连接状态      : " + J.badge);
      console.log("图谱统计      : " + J.graph);
      console.log("数据库卡片    : " + J.cards + " 张，首张 " + J.first);
      check("exe 内能跳到设置页", J.pane === "pane-settings", String(J.pane));
      check("exe 内能连上思源并加载数据", J.connected === true, J.badge);
      check("连上后引导条消失", J.barHidden === true, "hidden=" + J.barHidden);
      check("知识图谱读到数据", /\d/.test(J.graph || ""), J.graph);
      check("数据库汇总读到卡片", J.cards > 0, J.cards + " 张，首张 " + J.first);
      check("关键数据分点渲染", J.aiPts > 0, J.aiPts + " 点");
      check("水印在数据库页仍然可见", J.footOnDb === "yes", J.footOnDb);
      check("全程无运行期 JS 报错", (J.errors || []).length === 0, JSON.stringify(J.errors));
    }
    check("渲染期无 JS 报错", (R.errors || []).length === 0, JSON.stringify(R.errors));
  } catch (e) {
    check("验收执行异常", false, String(e && e.message));
  }

  /* ---- exe 二进制扫描（不看运行时，直接翻字节）---- */
  const binHits = scanExe(runExe);
  check("exe 二进制内无令牌 / 无本机路径 / 无个人数据 id",
    binHits.length === 0,
    binHits.length ? binHits.map(h => h.name + "×" + h.n + "（如 " + h.sample + "）").join("；") : "已扫描 5 类敏感串，均未出现");

  try { ws.close(); } catch (e) {}
  try { execSync("taskkill /PID " + proc.pid + " /T /F", { stdio: "ignore" }); } catch (e) {}
  /* WebView2 的子进程不一定挂在主进程的树里，按 user-data 目录再补一刀 */
  try {
    execSync('powershell -NoProfile -Command "Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like \'*' +
      sandbox.replace(/\\/g, "\\\\") + '*\' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }"',
      { stdio: "ignore", timeout: 20000 });
  } catch (e) {}
  await nap(1500);
  try { fs.rmSync(sandbox, { recursive: true, force: true, maxRetries: 6, retryDelay: 400 }); } catch (e) {}
  report();
  process.exit(results.some(r => !r.ok) ? 1 : 0);
})();

function report() {
  console.log("\n──────── 桌面版验收结果 ────────");
  let bad = 0;
  results.forEach(r => { if (!r.ok) bad++; console.log((r.ok ? "  ✓ " : "  ✗ ") + r.name + "\n      " + (r.detail || "")); });
  console.log(bad ? ("\n✗ " + bad + " 项未通过") : "\n✓ 全部 " + results.length + " 项通过");
}
