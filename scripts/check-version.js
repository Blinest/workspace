#!/usr/bin/env node
/**
 * 版本号自检：确认 6 处版本号完全一致。
 *
 *   node scripts/check-version.js
 *
 * 退出码 0 = 一致；1 = 有差异（会列出每一处的实际值）
 */
"use strict";
const fs = require("fs");
const path = require("path");

const ROOT = path.join(__dirname, "..");

function readJson(rel) {
  try {
    return JSON.parse(fs.readFileSync(path.join(ROOT, rel), "utf8"));
  } catch (e) {
    return null;
  }
}

function readText(rel) {
  try {
    return fs.readFileSync(path.join(ROOT, rel), "utf8");
  } catch (e) {
    return "";
  }
}

const checks = [];

/* tauri.conf.json */
const tauri = readJson("src-tauri/tauri.conf.json");
checks.push(["src-tauri/tauri.conf.json", tauri && tauri.version]);

/* Cargo.toml */
const cargo = readText("src-tauri/Cargo.toml");
const mCargo = /^version\s*=\s*"([^"]+)"/m.exec(cargo);
checks.push(["src-tauri/Cargo.toml", mCargo && mCargo[1]]);

/* package.json */
const pkg = readJson("package.json");
checks.push(["package.json", pkg && pkg.version]);

/* package-lock.json（两处） */
const lock = readJson("package-lock.json");
checks.push(["package-lock.json (顶层)", lock && lock.version]);
checks.push(["package-lock.json (packages[\"\"])", lock && lock.packages && lock.packages[""] && lock.packages[""].version]);

/* frontend/index.html（页脚 vX.Y.Z） */
const idx = readText("frontend/index.html");
const mIdx = /本地运行\s*·\s*数据不出本机\s*·\s*v([\d.]+)/.exec(idx);
checks.push(["frontend/index.html (页脚)", mIdx && mIdx[1]]);

/* ---- 输出 ---- */
let bad = 0;
const values = new Set();

console.log("版本号自检");
console.log("");
for (const [where, ver] of checks) {
  const ok = !!ver;
  if (ok) values.add(ver);
  if (!ok) bad++;
  console.log("  " + (ok ? "✓" : "✗") + " " + where.padEnd(36) + (ver || "（没读到）"));
}

console.log("");
if (bad > 0) {
  console.log("✗ 有 " + bad + " 处读不到版本号");
  process.exit(1);
}
if (values.size > 1) {
  console.log("✗ 版本号不一致：" + Array.from(values).join(" / "));
  process.exit(1);
}

const v = Array.from(values)[0];
console.log("✓ 全部一致：v" + v);
console.log("  安装包会命名为  SiyuanResearchWorktable_" + v + "_x64-setup.exe");
process.exit(0);
