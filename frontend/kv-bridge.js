// localStorage 兼容层：底层换成 Rust 侧的 SQLite。
//
// ## 为什么要这层
//
// 原来前端用 localStorage 存快照 / 配置（159 处调用），
// 底层是 Chromium 的 LevelDB —— 单文件、无事务、看不到内容。
// 现在数据存在 SQLite 里（见 src-tauri/src/kvstore.rs）。
//
// 但 localStorage 是**同步** API，SQLite 通过 IPC 访问是**异步**的。
// 如果直接把 159 处改成 await，改动大、容易引入新 bug。
//
// 所以：
//   1. 页面加载时（本文件要放在所有业务脚本**之前**）
//      调 kv_all 一次性把全部键值拉到内存
//   2. 把 window.localStorage 换成一个**同步操作内存**的替身
//   3. 每次 setItem/removeItem 都记进待写队列，防抖 300ms 后
//      批量调 kv_set_many 落盘
//
// 这样业务代码一行不改，数据却进了 SQLite。
//
// ## 首次升级
//
// 老的 LevelDB 里已经有数据。如果直接换掉，用户就"丢数据"了。
// 所以启动时先读一遍原生 localStorage，
// 把 SQLite 里没有的键**导入**过去（kv_import 只补不覆盖）。
(function () {
  'use strict';

  var TAURI_INVOKE =
    (window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke) ||
    (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke) ||
    null;

  if (!TAURI_INVOKE) {
    /* 没有 Tauri（比如浏览器里直接开文件），保持原生 localStorage 不动 */
    try { console.warn('[kv] 无 Tauri，继续用原生 localStorage'); } catch (e) {}
    return;
  }

  /* 保留原生实现，用于首次迁移读取 */
  var native = window.localStorage;
  var nativeSnapshot = {};
  try {
    for (var i = 0; i < native.length; i++) {
      var nk = native.key(i);
      if (nk != null) nativeSnapshot[nk] = native.getItem(nk);
    }
  } catch (e) {}

  /* 内存镜像：所有同步读都从这里来 */
  var mem = {};
  var ready = false;

  /* 待写入队列：key -> value（空字符串表示删除） */
  var queue = {};
  var flushTimer = null;
  var FLUSH_MS = 300;

  function scheduleFlush() {
    if (flushTimer) return;
    flushTimer = setTimeout(function () {
      flushTimer = null;
      flush();
    }, FLUSH_MS);
  }

  function flush() {
    var pairs = [];
    for (var k in queue) {
      if (Object.prototype.hasOwnProperty.call(queue, k)) {
        pairs.push([k, queue[k]]);
      }
    }
    queue = {};
    if (!pairs.length) return;
    TAURI_INVOKE('kv_set_many', { pairs: pairs }).catch(function (e) {
      /* 写失败：把这些键放回队列，下次再试 */
      for (var j = 0; j < pairs.length; j++) {
        if (queue[pairs[j][0]] == null) queue[pairs[j][0]] = pairs[j][1];
      }
      try { console.warn('[kv] 落盘失败，稍后重试:', e); } catch (e2) {}
      scheduleFlush();
    });
  }

  /* 页面关闭前把没写完的赶紧刷掉 */
  window.addEventListener('beforeunload', function () {
    if (flushTimer) { clearTimeout(flushTimer); flushTimer = null; }
    flush();
  });
  document.addEventListener('visibilitychange', function () {
    if (document.visibilityState === 'hidden') {
      if (flushTimer) { clearTimeout(flushTimer); flushTimer = null; }
      flush();
    }
  });

  /* ---- 同步替身 ---- */
  var shim = {
    get length() {
      var n = 0;
      for (var k in mem) if (Object.prototype.hasOwnProperty.call(mem, k)) n++;
      return n;
    },
    key: function (idx) {
      var keys = Object.keys(mem);
      return idx >= 0 && idx < keys.length ? keys[idx] : null;
    },
    getItem: function (k) {
      k = String(k);
      return Object.prototype.hasOwnProperty.call(mem, k) ? mem[k] : null;
    },
    setItem: function (k, v) {
      k = String(k);
      v = String(v);
      if (mem[k] === v) return;      /* 没变就不写 */
      mem[k] = v;
      queue[k] = v;
      scheduleFlush();
      /* 让同源的其它 document（主窗口 / 侧边栏 / 预览）收到变更 */
      notify(k, v);
    },
    removeItem: function (k) {
      k = String(k);
      if (!Object.prototype.hasOwnProperty.call(mem, k)) return;
      delete mem[k];
      queue[k] = '';
      scheduleFlush();
      notify(k, null);
    },
    clear: function () {
      for (var k in mem) {
        if (Object.prototype.hasOwnProperty.call(mem, k)) {
          queue[k] = '';
          notify(k, null);
        }
      }
      mem = {};
      scheduleFlush();
    }
  };

  /* ---- 跨窗口通知 ----
     原生 localStorage 在同源窗口之间有 storage 事件，
     换掉之后没了。业务里（主窗口 ↔ 侧边栏）靠它同步，
     所以这里手动发一个同形态的自定义事件。 */
  function notify(k, v) {
    try {
      var ev = new Event('storage');
      ev.key = k;
      ev.newValue = v;
      ev.oldValue = null;
      ev.storageArea = shim;
      ev.url = location.href;
      window.dispatchEvent(ev);
    } catch (e) {}
  }

  /* ---- 启动：拉全量 + 迁移 ---- */
  var bootDone = false;
  function boot() {
    if (bootDone) return Promise.resolve();
    bootDone = true;

    return TAURI_INVOKE('kv_all')
      .then(function (all) {
        mem = all || {};

        /* 首次升级：把原生 localStorage 里多出来的键补进 SQLite */
        var missing = [];
        for (var k in nativeSnapshot) {
          if (!Object.prototype.hasOwnProperty.call(nativeSnapshot, k)) continue;
          var v = nativeSnapshot[k];
          if (v == null) continue;
          if (!Object.prototype.hasOwnProperty.call(mem, k)) missing.push([k, v]);
        }
        if (missing.length) {
          for (var m = 0; m < missing.length; m++) mem[missing[m][0]] = missing[m][1];
          TAURI_INVOKE('kv_import', { pairs: missing }).catch(function () {});
          try { console.log('[kv] 已从旧 localStorage 迁移 ' + missing.length + ' 个键'); } catch (e) {}
        }

        ready = true;
        /* 通知业务：数据就绪了 */
        try {
          var ev = new Event('kv-ready');
          window.dispatchEvent(ev);
        } catch (e) {}
      })
      .catch(function (e) {
        /* 读失败就退回原生，保证程序能用 */
        try { console.error('[kv] 读取失败，退回 localStorage:', e); } catch (e2) {}
        ready = true;
        for (var k in nativeSnapshot) {
          if (Object.prototype.hasOwnProperty.call(nativeSnapshot, k)) mem[k] = nativeSnapshot[k];
        }
        try { window.dispatchEvent(new Event('kv-ready')); } catch (e3) {}
      });
  }

  /* ---- 替换 window.localStorage ---- */
  try {
    Object.defineProperty(window, 'localStorage', {
      configurable: true,
      get: function () { return shim; }
    });
  } catch (e) {
    try { window.localStorage = shim; } catch (e2) {}
  }

  /* 暴露给设置页显示状态 */
  window.__kv = {
    shim: shim,
    isReady: function () { return ready; },
    pending: function () { return Object.keys(queue).length; },
    flushNow: function () {
      if (flushTimer) { clearTimeout(flushTimer); flushTimer = null; }
      flush();
    },
    info: function () { return TAURI_INVOKE('kv_info'); }
  };

  /* 开始加载。
     ⚠ 业务脚本可能在 kv-ready 之前就跑，
       那时读到的是空对象 —— 但它们都是"读不到就用默认值"，
       等 kv-ready 到了再重绘一次即可。
       为减少这个窗口期，boot() 在脚本最前面就发起。 */
  boot();
})();
