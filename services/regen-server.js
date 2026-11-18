/*
 * weekly-summary 本地重生成服务 (Plan A)
 * 由 DSH 以常驻后台任务启动，监听 http://127.0.0.1:3918
 *  POST /regen  -> 经 bullet-journal MCP 拉本周实时番茄数据 -> 重写 weekly-summary.html -> 返回
 *  GET  /health -> {ok:true}
 */
'use strict';
const http = require('http');
const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');
const readline = require('readline');

const PORT = Number(process.env.PORT || 3918);
/* ⚠ 所有路径都按「本文件所在目录」解析，不再写死本机绝对路径。
   这样同一份脚本复制到服务器上也能直接跑。 */
const PROJECT = __dirname;
const OUT = path.join(PROJECT, 'weekly-summary.html');
/* 思源 bullet-journal MCP（可选）。没有的话周报服务仍可工作，
   只是少一路数据源。用环境变量 MCP_SERVER 指定。 */
const MCP = process.env.MCP_SERVER
  || 'D:/0_study/0_7_output/output/data/plugins/siyuan-plugin-bullet-journal/mcp-server.js';
const CONFIG_PATH = path.join(PROJECT, 'regen-config.json');
/* 用户级配置目录：安装包里**不带** regen-config.json（里面有私有 token），
   真实配置放这里，首次运行会自动从脚本目录迁移过来。 */
const USER_DIR = process.env.LOCALAPPDATA
  ? path.join(process.env.LOCALAPPDATA, 'com.dsw.siyuan-research-worktable')
  : path.join(require('os').homedir(), '.siyuan-research-worktable');
const USER_CONFIG_PATH = path.join(USER_DIR, 'regen-config.json');
const DEFAULT_API = process.env.SIYUAN_API_URL || 'http://127.0.0.1:6806';
const DEFAULT_TOKEN = process.env.SIYUAN_TOKEN || '';

/* 当前生效的配置文件路径（保存配置时写这里） */
let activeConfigPath = USER_CONFIG_PATH;

/* 数据源配置，优先级：
 *   1. 环境变量 SIYUAN_API_URL / SIYUAN_TOKEN
 *   2. 用户配置目录的 regen-config.json
 *   3. 脚本目录的 regen-config.json（老版本/开发期）
 *   4. 默认本机思源
 */
function loadConfig() {
  const candidates = [USER_CONFIG_PATH, CONFIG_PATH];
  for (const p of candidates) {
    try {
      if (!fs.existsSync(p)) continue;
      const j = JSON.parse(fs.readFileSync(p, 'utf8'));
      const api = String(j.api || '').trim() || DEFAULT_API;
      const token = String(j.token || '').trim() || DEFAULT_TOKEN;
      activeConfigPath = p;
      return { api: api.replace(/\/+$/, ''), token };
    } catch (e) { /* 继续试下一个 */ }
  }
  return { api: DEFAULT_API.replace(/\/+$/, ''), token: DEFAULT_TOKEN };
}
let CONF = loadConfig();

/* ---------------- MCP 客户端（stdio，拉起 bullet-journal mcp-server） ---------------- */
function mcpCall(tool, args) {
  return new Promise((resolve, reject) => {
    let child;
    try {
      child = spawn('node', [MCP], {
        env: { ...process.env, SIYUAN_TOKEN: CONF.token, SIYUAN_API_URL: CONF.api }
      });
    } catch (e) { return reject(e); }
    const rl = readline.createInterface({ input: child.stdout });
    let id = 0; const pending = {}; let stderr = '';
    child.stderr.on('data', d => { stderr += d; if (stderr.length > 4000) stderr = stderr.slice(-4000); });
    function send(o){ try { child.stdin.write(JSON.stringify(o) + '\n'); } catch (e) {} }
    function req(method, params){ const i = ++id; return new Promise((res, rej) => { pending[i] = {res, rej}; send({jsonrpc:'2.0', id:i, method, params}); }); }
    rl.on('line', l => { if (!l.trim()) return; let m; try { m = JSON.parse(l); } catch (e) { return; }
      if (m.id && pending[m.id]) { const p = pending[m.id]; delete pending[m.id]; m.error ? p.rej(new Error(JSON.stringify(m.error))) : p.res(m.result); }
    });
    const timer = setTimeout(() => { child.kill(); reject(new Error('MCP timeout')); }, 30000);
    const finish = (fn, v) => { clearTimeout(timer); try { child.kill(); } catch (e) {} fn(v); };
    (async () => {
      try {
        await req('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'regen-server', version: '1.0' } });
        send({ jsonrpc: '2.0', method: 'notifications/initialized' });
        const res = await req('tools/call', { name: tool, arguments: args || {} });
        // MCP 结果内容可能是 JSON 字符串
        let text = '';
        const cont = (res && res.content) || [];
        for (const c of cont) if (c && c.type === 'text') text += (c.text || '');
        let data;
        try { data = JSON.parse(text); } catch (e) { data = text; }
        finish(resolve, data);
      } catch (e) { finish(reject, e); }
    })();
  });
}

/* ---------------- 工具 ---------------- */
function pad(n){ return (n < 10 ? '0' : '') + n; }
function fmtDate(d){ return d.getFullYear() + '-' + pad(d.getMonth()+1) + '-' + pad(d.getDate()); }
function thisWeek(){
  const d = new Date();
  const dow = (d.getDay() + 6) % 7;           // 周一=0
  const mon = new Date(d.getFullYear(), d.getMonth(), d.getDate() - dow);
  const sun = new Date(mon.getFullYear(), mon.getMonth(), mon.getDate() + 6);
  return { start: mon, end: sun };
}
function esc(s){ return String(s == null ? '' : s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;'); }
function periodLabel(start, end){
  const s = start.getFullYear() + '.' + pad(start.getMonth()+1) + '.' + pad(start.getDate());
  const e = pad(end.getMonth()+1) + '.' + pad(end.getDate());
  return s + ' – ' + e;
}

/* ---------------- HTML 片段生成 ---------------- */
function buildStatCards(o){
  return [
    ['专注番茄', o.count + '', '个', '本周记录的工作时段'],
    ['专注时长', o.minutes + '', 'min', '≈ ' + (o.hours ? o.hours : 0) + ' 小时有效投入'],
    ['推进任务', o.tasks + '', '项', '本周投入执行的任务项'],
    ['工作项目', o.projects + '', '个', o.projectNames.length ? o.projectNames.slice(0,2).join(' · ') : '—']
  ].map(c => '<div class="stat"><p class="lbl">' + c[0] + '</p><p class="val">' + c[1] + '<small>' + c[2] + '</small></p><p class="cap">' + c[3] + '</p></div>').join('\n    ');
}
function buildBars(days, byDay){
  const max = Math.max(1, ...days.map(d => byDay[fmtDate(d)] || 0));
  const cols = days.map(d => {
    const k = fmtDate(d); const v = byDay[k] || 0;
    const lbl = pad(d.getMonth()+1) + '-' + pad(d.getDate());
    if (!v) return '<div class="bar-col"><div class="bar zero"></div><div class="bar-lbl">' + lbl + '</div></div>';
    const h = Math.max(4, Math.round(v / max * 100));
    return '<div class="bar-col"><div class="bar" style="height:' + h + '%"><span class="v">' + v + '</span></div><div class="bar-lbl">' + lbl + '</div></div>';
  });
  let peak = null, peakMin = 0;
  days.forEach(d => { const v = byDay[fmtDate(d)] || 0; if (v > peakMin) { peakMin = v; peak = d; } });
  const cap = peakMin
    ? ('本周专注记录合计 ' + Object.values(byDay).reduce((a,b)=>a+b,0) + ' min；峰值 ' + pad(peak.getMonth()+1) + '-' + pad(peak.getDate()) + '（' + peakMin + ' min）。柱高对应当天实际专注分钟，供日分布速览。')
    : '本周暂无番茄钟记录。';
  return { bars: '<div class="bars">' + cols.join('') + '</div>', cap: '<p class="chart-caption">' + cap + '</p>' };
}
/* 读本周各天的项目文档，抽出「任务头 → 事项」的对应关系。
   番茄块是普通段落，没有 itemContent 属性，只能靠文档结构把说明文字
   挂回它所属的任务名下。 */
async function loadDocTasks(week){
  const out = [];   /* [{task, item, text}] */
  const roots = ['/日常记录/待办', '每日待办'];
  let nbs = [];
  try {
    const r = await fetch(CONF.api + '/api/notebook/lsNotebooks', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: 'Token ' + CONF.token },
      body: '{}'
    });
    const j = await r.json();
    nbs = ((j && j.data && j.data.notebooks) || []);
  } catch (e) { return out; }

  for (const nb of nbs) {
    let docs = [];
    try {
      const r = await fetch(CONF.api + '/api/query/sql', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Authorization: 'Token ' + CONF.token },
        body: JSON.stringify({ stmt: "SELECT id, hpath FROM blocks WHERE box='" + nb.id + "' AND type='d'" })
      });
      const j = await r.json();
      docs = (j && j.data) || [];
    } catch (e) { continue; }
    for (const d of docs) {
      const hp = String(d.hpath || '');
      if (!roots.some(x => hp.startsWith(x))) continue;
      let md = '';
      try {
        const r = await fetch(CONF.api + '/api/export/exportMdContent', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json', Authorization: 'Token ' + CONF.token },
          body: JSON.stringify({ id: d.id })
        });
        const j = await r.json();
        md = (j && j.data && j.data.content) || '';
      } catch (e) { continue; }
      let curTask = '';
      for (const line of md.split(/\r?\n/)) {
        const s = line.trim();
        if (!s) continue;
        if (s.includes('📋')) {
          curTask = s.replace(/📋.*/, '').replace(/[🔥✅]/g, '').replace(/📅.*/, '').trim();
          continue;
        }
        if (/^- \[[ xX]\]/.test(s)) {
          const item = s.replace(/^- \[[ xX]\]\s*/, '').replace(/📅.*$/, '').replace(/[🔥✅]/g, '').trim();
          if (curTask && item) out.push({ task: curTask, item });
        }
      }
    }
  }
  return out;
}

function buildProjects(records, docTasks){
  /* 番茄记录是从「番茄行文本」解析出来的，很多块上没有 itemContent
     （不是插件的属性块，只是普通段落），直接分组会全落到「未命名」。
     这里按优先级回退：itemContent → taskName → 文档里同名事项所属的任务 → 从 description 里剥文案。 */
  function nameOf(r){
    if (r.itemContent && r.itemContent.trim()) return r.itemContent.trim();
    if (r.taskName && String(r.taskName).trim()) return String(r.taskName).trim();
    const d = String(r.description || '').trim();
    /* 番茄说明里常写着事项原文，拿它去文档里反查自己属于哪个任务 */
    if (d && docTasks && docTasks.length) {
      const hit = docTasks.find(x => d.includes(x.item) && x.item.length >= 4);
      if (hit) return hit.task;
    }
    if (!d) return '未命名';
    /* description 形如 "\~2026-11-11 22:42:26 完成专题四PPT改到能发出去" */
    const m = d.match(/\\?~\s*\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\s*(.*)$/);
    if (m && m[1].trim()) return m[1].trim();
    const m2 = d.match(/^\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\s*(.*)$/);
    if (m2 && m2[1].trim()) return m2[1].trim();
    return d;
  }
  const projMap = new Map();
  for (const r of records) { if (!projMap.has(r.projectName)) projMap.set(r.projectName, []); projMap.get(r.projectName).push(r); }
  const blocks = [];
  let taskCount = 0;
  for (const [proj, recs] of projMap) {
    const total = recs.reduce((a, r) => a + (r.actualDurationMinutes || r.durationMinutes || 0), 0);
    // 按事项分组
    const itemMap = new Map();
    for (const r of recs) { const k = nameOf(r); if (!itemMap.has(k)) itemMap.set(k, []); itemMap.get(k).push(r); }
    taskCount += itemMap.size;
    let inner = '';
    for (const [item, rows] of itemMap) {
      const itMin = rows.reduce((a, r) => a + (r.actualDurationMinutes || r.durationMinutes || 0), 0);
      const lis = rows.map(r => {
        const m = (r.actualDurationMinutes || r.durationMinutes || 0);
        const txt = (r.description && r.description.trim()) ? r.description.trim() : item;
        const dl = (r.date || '').slice(5).replace('-', '-');
        return '<li><span class="day">' + dl + '</span><span>' + esc(txt) + '</span><span class="pomo">' + m + 'min</span></li>';
      }).join('');
      inner += '<div class="task"><div class="t"><span class="name">' + esc(item) + '</span><span class="badge">本周 ' + itMin + ' min</span></div>' +
               '<p class="schedule">' + rows.length + ' 个番茄 · ' + itMin + ' min</p><ul>' + lis + '</ul></div>';
    }
    const totalMin = recs.reduce((a, r) => a + (r.actualDurationMinutes || r.durationMinutes || 0), 0);
    blocks.push('<div class="project"><div class="proj-head"><span class="nm">' + esc(proj) + '</span>' +
      '<span class="total">' + recs.length + ' 番茄 · ' + totalMin + ' min</span></div>' + inner + '</div>');
  }
  const html = blocks.length ? blocks.join('\n\n  ') : '<p class="kv">本周无番茄记录。</p>';
  return { html, taskCount };
}

/* ---------------- 主重生成 ---------------- */
async function regen(week){
  const startS = fmtDate(week.start), endS = fmtDate(week.end);
  const stats = await mcpCall('get_pomodoro_stats', { startDate: startS, endDate: endS });
  const raw = await mcpCall('get_pomodoro_records', { startDate: startS, endDate: endS });
  const records = (raw && raw.records) || [];
  const minutes = records.reduce((a, r) => a + (r.actualDurationMinutes || r.durationMinutes || 0), 0);
  const hours = Math.floor(minutes / 60);
  const byDay = {}; records.forEach(r => { const k = r.date; byDay[k] = (byDay[k] || 0) + (r.actualDurationMinutes || r.durationMinutes || 0); });
  const days = []; for (let d = new Date(week.start); d <= week.end; d.setDate(d.getDate() + 1)) days.push(new Date(d));
  const projectNames = [...new Set(records.map(r => r.projectName).filter(Boolean))];
  /* 任务名兜底：番茄块上没有 itemContent 时，从当日文档的
     `xxx 📋` 任务头里按说明文字反查。 */
  const docTasks = await loadDocTasks(week);
  const proj = buildProjects(records, docTasks);
  const tasks = [...new Set(records.map(r => r.itemContent || r.taskName || '').filter(Boolean))];

  const summary = {
    count: (stats && (stats.totalCount || stats.totalMinutes)) || records.length,
    minutes, hours, tasks: Math.max(tasks.length, proj.taskCount), projects: projectNames.length, projectNames
  };
  const period = periodLabel(week.start, week.end);
  const bars = buildBars(days, byDay);
  const projectsHtml = proj.html;

  // 读现有文件并按锚点替换
  let html = fs.readFileSync(OUT, 'utf8');
  function replaceRegion(name, inner){
    const re = new RegExp('<!--\\{\\{' + name + '\\}\\}-->[\\s\\S]*?<!--\\{\\{/' + name + '\\}\\}-->');
    if (!re.test(html)) throw new Error('anchor not found: ' + name);
    html = html.replace(re, '<!--{{' + name + '}}-->\n    ' + inner + '\n  <!--{{/' + name + '}}-->');
  }
  replaceRegion('PERIOD', esc(period));
  replaceRegion('STATS', buildStatCards(summary));
  replaceRegion('BARS', bars.bars + '\n    ' + bars.cap);
  replaceRegion('PROJECTS', projectsHtml);
  fs.writeFileSync(OUT, html, 'utf8');
  return { period, records: records.length, minutes, week: { start: startS, end: endS } };
}

/* ---------------- 导出 PDF（Edge 无头打印） ---------------- */
const EDGE_CANDIDATES = [
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
];
function findBrowser(){
  for (const p of EDGE_CANDIDATES) { try { if (fs.existsSync(p)) return p; } catch (e) {} }
  return null;
}
function exportPdf(outPath){
  return new Promise((resolve, reject) => {
    const exe = findBrowser();
    if (!exe) return reject(new Error('未找到 Edge/Chrome，可执行文件不存在'));
    const src = path.join(PROJECT, 'weekly-summary.html');
    if (!fs.existsSync(src)) return reject(new Error('未找到 weekly-summary.html'));
    const fileUrl = 'file:///' + src.replace(/\\/g, '/');
    const args = [
      '--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
      '--no-pdf-header-footer', '--print-to-pdf-no-header',
      '--virtual-time-budget=8000',
      '--print-to-pdf=' + outPath,
      fileUrl,
    ];
    let child;
    try {
      child = spawn(exe, args, { windowsHide: true });
    } catch (e) { return reject(e); }
    let err = '';
    child.stderr.on('data', d => { err += d; if (err.length > 3000) err = err.slice(-3000); });
    const timer = setTimeout(() => { try { child.kill(); } catch (e) {} reject(new Error('打印超时')); }, 60000);
    child.on('error', e => { clearTimeout(timer); reject(e); });
    child.on('exit', () => {
      clearTimeout(timer);
      // Edge 打印完成后进程退出，检查文件是否生成
      setTimeout(() => {
        try {
          if (fs.existsSync(outPath) && fs.statSync(outPath).size > 0) resolve(outPath);
          else reject(new Error('未生成 PDF。' + (err ? err.slice(-300) : '')));
        } catch (e) { reject(e); }
      }, 800);
    });
  });
}

/* ---------------- 导出 PNG（Edge 无头截图，先量真实高度避免空白/截断） ---------------- */
/* Python 解释器：环境变量 PYTHON_EXE 优先，其次 PATH 里的 python/python3。
   不再写死本机安装路径，服务器上也能跑。 */
function pythonBin(){
  const env = String(process.env.PYTHON_EXE || '').trim();
  if (env && fs.existsSync(env)) return env;
  return process.platform === 'win32' ? 'python' : 'python3';
}
function measureHeight(htmlPath, width){
  return new Promise((resolve) => {
    try {
      const py = pythonBin();
      const script = path.join(PROJECT, 'measure_height.py');
      if (!fs.existsSync(script)) { resolve(0); return; }
      const child = spawn(py, [script, htmlPath, String(width)], { windowsHide: true });
      let out = '', err = '';
      child.stdout.on('data', d => { out += d; });
      child.stderr.on('data', d => { err += d; });
      const timer = setTimeout(() => { try { child.kill(); } catch (e) {} resolve(0); }, 30000);
      child.on('error', () => { clearTimeout(timer); resolve(0); });
      child.on('exit', () => {
        clearTimeout(timer);
        const n = parseInt(String(out).trim(), 10);
        resolve(Number.isFinite(n) && n > 0 ? n : 0);
      });
    } catch (e) { resolve(0); }
  });
}

function exportPng(outPath, width, height){
  return new Promise(async (resolve, reject) => {
    const exe = findBrowser();
    if (!exe) return reject(new Error('未找到 Edge/Chrome，可执行文件不存在'));
    const src = path.join(PROJECT, 'weekly-summary.html');
    if (!fs.existsSync(src)) return reject(new Error('未找到 weekly-summary.html'));
    const fileUrl = 'file:///' + src.replace(/\\/g, '/');
    const W = width || 1400;
    // 未显式指定高度时，先量页面真实高度（加少量余量），避免底部大片空白或被截断
    let H = height || 0;
    if (!H) {
      const mh = await measureHeight(src, W);
      H = mh ? (mh + 40) : 3600;
    }
    const args = [
      '--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
      '--hide-scrollbars', '--force-device-scale-factor=2',
      '--virtual-time-budget=8000',
      '--window-size=' + W + ',' + H,
      '--screenshot=' + outPath,
      fileUrl,
    ];
    let child;
    try {
      child = spawn(exe, args, { windowsHide: true });
    } catch (e) { return reject(e); }
    let err = '';
    child.stderr.on('data', d => { err += d; if (err.length > 3000) err = err.slice(-3000); });
    const timer = setTimeout(() => { try { child.kill(); } catch (e) {} reject(new Error('截图超时')); }, 60000);
    child.on('error', e => { clearTimeout(timer); reject(e); });
    child.on('exit', () => {
      clearTimeout(timer);
      setTimeout(() => {
        try {
          if (fs.existsSync(outPath) && fs.statSync(outPath).size > 0) resolve(outPath);
          else reject(new Error('未生成 PNG。' + (err ? err.slice(-300) : '')));
        } catch (e) { reject(e); }
      }, 800);
    });
  });
}

/* ---------------- 笔记正文 AI 总结：渲染 ---------------- */
function cut(s, n){ s = String(s == null ? '' : s).trim(); return s.length > n ? s.slice(0, n) + '…' : s; }
/* ---------- 每日执行力：工作台推送过来，渲染进周报 ---------- */
/* 每次用到时都重新读盘，避免服务长期运行后内存里还是旧数据 */
let capState = null;
function loadCapacity() {
  try {
    const capFile = path.join(__dirname, 'capacity.json');
    if (fs.existsSync(capFile)) {
      capState = JSON.parse(fs.readFileSync(capFile, 'utf8'));
      return capState;
    }
  } catch (e) {}
  return capState;
}
loadCapacity();

function buildCapacityHtml(cap) {
  if (!cap || !Array.isArray(cap.days) || !cap.days.length) {
    return '<div class="panel"><p style="color:#a39c8e;font-size:12px;margin:0">还没有历史数据。每天的完成率与用时会在云端累积后显示。</p></div>';
  }
  const rows = cap.days.slice().reverse().map(function (d) {
    const cls = d.score >= 80 ? 'g' : (d.score >= 50 ? 'm' : 'b');
    return '<div class="capRow"><span class="capDate">' + String(d.date).slice(5) + '</span>'
      + '<span class="capBar"><i class="' + cls + '" style="width:' + d.score + '%"></i></span>'
      + '<span class="capNum">' + d.done + '/' + d.total + '</span>'
      + '<span class="capMin">' + d.mins + '分</span></div>';
  }).join('');
  return '<div class="panel"><div class="capHead">近 ' + cap.days.length + ' 天 · 平均每天 '
    + cap.avgTasks + ' 项 / ' + cap.avgMins + ' 分钟 · 完成率 ' + cap.avgScore + '%</div>'
    + rows + '</div>';
}
function buildNotesHtml(p){
  /* 刻意收紧：每节最多 3 条、每条不超过 48 字，避免整页被 AI 原文灌满 */
  const list = (arr) => (arr || []).slice(0, 3).map(i => '<li>· ' + esc(cut(i, 48)) + '</li>').join('');
  let h = '';
  /* headline 已单独渲染到页面顶部（{{HEADLINE}}），这里不再重复 */
  const secs = (p.sections || []).filter(s => s && s.h).slice(0, 4);
  if (secs.length) {
    h += '<div style="display:grid;grid-template-columns:1fr 1fr;gap:14px">';
    secs.forEach(s => {
      h += '<div class="panel"><h3>' + esc(s.h) + '</h3>'
         + '<ul class="kv" style="padding-left:0;list-style:none;margin:0;line-height:1.9">'
         + list(s.items) + '</ul></div>';
    });
    h += '</div>';
  }
  if ((p.docs || []).length) {
    h += '<div class="tags" style="margin-top:14px">'
       + p.docs.slice(0, 24).map(d => '<span>' + esc(d.title) + '</span>').join('')
       + '</div>';
  }
  if (p.at) {
    h += '<p class="footnote" style="border:none;margin-top:12px;padding-top:0;font-size:11.5px">'
       + esc(p.model || 'AI') + ' 生成于 ' + esc(p.at)
       + '，覆盖本周更新的 ' + (p.docs || []).length + ' 篇笔记。</p>';
  }
  return h || '<p class="kv">（AI 未返回可展示的内容）</p>';
}

/* ---------------- HTTP 服务 ---------------- */
const server = http.createServer((req, res) => {
  const send = (code, obj) => { res.writeHead(code, { 'Content-Type': 'application/json; charset=utf-8', 'Access-Control-Allow-Origin': '*', 'Access-Control-Allow-Methods': 'GET,POST,OPTIONS', 'Access-Control-Allow-Headers': 'Content-Type' }); res.end(JSON.stringify(obj)); };
  if (req.method === 'OPTIONS') return send(200, { ok: true });
  if (req.method === 'GET' && req.url.startsWith('/health')) return send(200, { ok: true, pid: process.pid });
  if ((req.method === 'POST' || req.method === 'GET') && req.url.startsWith('/pdf')) {
    let body = '';
    const run = async () => {
      let out = null;
      try { out = JSON.parse(body || '{}').out || null; } catch (e) {}
      const outPath = out || path.join(PROJECT, 'weekly-summary.pdf');
      try {
        const p = await exportPdf(outPath);
        send(200, { ok: true, path: p, size: fs.statSync(p).size });
      } catch (e) { send(500, { ok: false, error: String((e && e.message) || e) }); }
    };
    req.on('data', c => { body += c; if (body.length > 1e5) req.destroy(); });
    req.on('end', run);
    return;
  }
  if ((req.method === 'POST' || req.method === 'GET') && req.url.startsWith('/png')) {
    let body = '';
    const run = async () => {
      let out = null, w = null, h = null;
      try { const j = JSON.parse(body || '{}'); out = j.out || null; w = j.width || null; h = j.height || null; } catch (e) {}
      const outPath = out || path.join(PROJECT, 'weekly-summary.png');
      try {
        const p = await exportPng(outPath, w, h);
        send(200, { ok: true, path: p, size: fs.statSync(p).size });
      } catch (e) { send(500, { ok: false, error: String((e && e.message) || e) }); }
    };
    req.on('data', c => { body += c; if (body.length > 1e5) req.destroy(); });
    req.on('end', run);
    return;
  }
  if (req.method === 'POST' && req.url.startsWith('/regen')) {
    let body = '';
    req.on('data', c => { body += c; if (body.length > 1e5) req.destroy(); });
    req.on('end', async () => {
      let at = null; try { at = JSON.parse(body || '{}').at || null; } catch (e) {}
      try {
        const out = await regen(thisWeek());
        send(200, { ok: true, at: at || new Date().toLocaleString('zh-CN', { hour12: false }), records: out.records, minutes: out.minutes, period: out.period, week: out.week });
      } catch (e) { send(500, { ok: false, error: String((e && e.message) || e) }); }
    });
    return;
  }
  /* 接收工作台算好的「每日执行力」，存盘并供 /weekly 渲染 */
  if (req.method === 'POST' && req.url.startsWith('/capacity')) {
    let body = '';
    req.on('data', function (c) { body += c; });
    req.on('end', function () {
      try {
        capState = JSON.parse(body || '{}');
        fs.writeFileSync(path.join(__dirname, 'capacity.json'), JSON.stringify(capState));
        loadCapacity();
        res.writeHead(200, { 'Content-Type': 'application/json', 'Access-Control-Allow-Origin': '*' });
        res.end('{"ok":true}');
      } catch (e) {
        res.writeHead(400, { 'Content-Type': 'application/json', 'Access-Control-Allow-Origin': '*' });
        res.end(JSON.stringify({ ok: false, error: String((e && e.message) || e) }));
      }
    });
    return;
  }
  /* 返回当前 weekly-summary.html 的实时内容，供工作台内嵌 iframe 使用。
     每次请求都重新读盘，因此页面内「重新生成」写盘后 reload 即可拿到最新版。 */
  if (req.method === 'GET' && (req.url === '/weekly' || req.url.startsWith('/weekly?'))) {
    try {
      let html = fs.readFileSync(OUT, 'utf8');
      html = html.replace(/<!--\{\{CAPACITY\}\}\-->[\s\S]*?<!--\{\{\/CAPACITY\}\}\-->/,
        '<!--{{CAPACITY}}-->' + buildCapacityHtml(loadCapacity()) + '<!--{{/CAPACITY}}-->');
      res.writeHead(200, {
        'Content-Type': 'text/html; charset=utf-8',
        'Cache-Control': 'no-store',
        'Access-Control-Allow-Origin': '*'
      });
      res.end(html);
    } catch (e) {
      res.writeHead(500, { 'Content-Type': 'text/plain; charset=utf-8', 'Access-Control-Allow-Origin': '*' });
      res.end('weekly-summary.html 读取失败：' + String((e && e.message) || e));
    }
    return;
  }
  /* 写入「笔记内容 AI 总结」区块：把前端用 AI 生成的摘要灌进 weekly-summary.html 的 {{NOTES}} 锚点 */
  if (req.method === 'POST' && req.url.startsWith('/notes')) {
    let body = '';
    req.on('data', c => { body += c; if (body.length > 4e6) req.destroy(); });
    req.on('end', () => {
      let p = null;
      try { p = JSON.parse(body || '{}'); } catch (e) {}
      if (!p || typeof p !== 'object' || Array.isArray(p)) return send(400, { ok: false, error: '请求体不是合法 JSON 对象' });
      try {
        let html = fs.readFileSync(OUT, 'utf8');
      html = html.replace(/<!--\{\{CAPACITY\}\}\-->[\s\S]*?<!--\{\{\/CAPACITY\}\}\-->/,
        '<!--{{CAPACITY}}-->' + buildCapacityHtml(loadCapacity()) + '<!--{{/CAPACITY}}-->');
        const reNotes = /<!--\{\{NOTES\}\}\-->[\s\S]*?<!--\{\{\/NOTES\}\}-->/;
        if (!reNotes.test(html)) throw new Error('weekly-summary.html 里找不到 {{NOTES}} 锚点，请先补上该区块');
        const inner = buildNotesHtml(p);
        // 用函数式替换，避免摘要里的 $& / $1 被当成替换模式
        let next = html.replace(reNotes, () => '<!--{{NOTES}}-->\n    ' + inner + '\n  <!--{{/NOTES}}-->');
        // 顶部副标题同步改用 AI 的 headline（该区域可选，模板里没有就跳过）
        const reHead = /<!--\{\{HEADLINE\}\}\-->[\s\S]*?<!--\{\{\/HEADLINE\}\}-->/;
        if (reHead.test(next)) {
          const head = p.headline ? cut(p.headline, 110) : '本周主线待生成';
          next = next.replace(reHead, () => '<!--{{HEADLINE}}-->' + esc(head) + '<!--{{/HEADLINE}}-->');
        }
        fs.writeFileSync(OUT, next, 'utf8');
        send(200, { ok: true, docs: (p.docs || []).length, sections: (p.sections || []).length, at: p.at || null });
      } catch (e) { send(500, { ok: false, error: String((e && e.message) || e) }); }
    });
    return;
  }
  /* 数据源配置：查看 / 修改。可指向服务器上的思源（公网 IP + 端口） */
  if (req.method === 'GET' && req.url.startsWith('/config')) {
    return send(200, { ok: true, api: CONF.api, hasToken: !!CONF.token, configPath: activeConfigPath });
  }
  if (req.method === 'POST' && req.url.startsWith('/config')) {
    let body = '';
    req.on('data', c => { body += c; if (body.length > 1e5) req.destroy(); });
    req.on('end', () => {
      let j = null;
      try { j = JSON.parse(body || '{}'); } catch (e) {}
      if (!j || typeof j !== 'object' || Array.isArray(j)) return send(400, { ok: false, error: '请求体不是合法 JSON' });
      const api = String(j.api || CONF.api).trim().replace(/\/+$/, '');
      const token = String(j.token || CONF.token).trim();
      if (!/^https?:\/\//i.test(api)) return send(400, { ok: false, error: 'api 必须以 http:// 或 https:// 开头' });
      try {
        /* 写到用户配置目录 —— 脚本目录在安装包下可能只读 */
        try { fs.mkdirSync(USER_DIR, { recursive: true }); } catch (e2) {}
        let wrote = USER_CONFIG_PATH;
        try {
          fs.writeFileSync(USER_CONFIG_PATH, JSON.stringify({ api, token }, null, 2), 'utf8');
        } catch (e2) {
          /* 用户目录不可写时退回脚本目录 */
          fs.writeFileSync(CONFIG_PATH, JSON.stringify({ api, token }, null, 2), 'utf8');
          wrote = CONFIG_PATH;
        }
        activeConfigPath = wrote;
        CONF = { api, token };
        send(200, { ok: true, api: CONF.api, hasToken: !!CONF.token, configPath: wrote });
      } catch (e) { send(500, { ok: false, error: String((e && e.message) || e) }); }
    });
    return;
  }
  /* 连通性自检：用当前配置打一次 /api/system/version，用于确认公网 IP + 端口 + 令牌是否可用 */
  if (req.method === 'GET' && req.url.startsWith('/probe')) {
    const target = CONF.api + '/api/system/version';
    fetch(target, { headers: { Authorization: 'Token ' + CONF.token } })
      .then(r => r.text().then(t => {
        let j = null; try { j = JSON.parse(t); } catch (e) {}
        send(200, {
          ok: r.ok && !!j && j.code === 0,
          api: CONF.api,
          status: r.status,
          version: (j && j.data) || null,
          body: j ? undefined : t.slice(0, 300)
        });
      }))
      .catch(e => send(200, { ok: false, api: CONF.api, error: String((e && e.message) || e) }));
    return;
  }
  send(404, { ok: false, error: 'not found' });
});

server.on('error', (err) => {
  if (err && err.code === 'EADDRINUSE') {
    console.log('[' + new Date().toISOString() + '] port ' + PORT + ' already in use — another regen-server is running; exiting.');
    process.exit(0);
  }
  console.error('regen-server error:', err);
  process.exit(1);
});
server.listen(PORT, '127.0.0.1', () => console.log('regen-server listening on http://127.0.0.1:' + PORT + ' (pid ' + process.pid + ')'));
