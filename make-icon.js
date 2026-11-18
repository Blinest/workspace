/* ============================================================================
 * 生成应用图标（零依赖）
 *   深蓝圆角底 + 米色知识图谱节点
 *   输出：
 *     src-tauri/icons/icon.ico            —— 多尺寸 ICO（16/24/32/48/64/128/256）
 *     src-tauri/icons/icon.png            —— 512×512（商店/通用）
 *     src-tauri/icons/{N}x{N}.png         —— Tauri 打包要求的尺寸
 *   需要时直接跑：node make-icon.js
 * ==========================================================================*/
"use strict";
const fs = require("fs");
const path = require("path");
const zlib = require("zlib");

const NAVY = [27, 54, 93, 255];        // #1B365D
const CREAM = [246, 244, 236, 255];    // #F6F4EC
const AMBER = [180, 83, 9, 255];       // #B45309

/* 按目标尺寸画一张 RGBA 图（所有几何量按 S 缩放，保证小图标也清晰） */
function render(S) {
  const buf = Buffer.alloc(S * S * 4, 0);
  const k = S / 256;                    // 相对于 256 设计稿的缩放

  function put(x, y, c, a = 1) {
    if (x < 0 || y < 0 || x >= S || y >= S) return;
    const i = (y * S + x) * 4;
    const sa = c[3] / 255 * a;
    const da = buf[i + 3] / 255;
    const oa = sa + da * (1 - sa);
    if (oa <= 0) return;
    for (let m = 0; m < 3; m++) buf[i + m] = Math.round((c[m] * sa + buf[i + m] * da * (1 - sa)) / oa);
    buf[i + 3] = Math.round(oa * 255);
  }

  function roundRect(x0, y0, w, h, r, c) {
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        const px = x + 0.5, py = y + 0.5;
        const cx = Math.min(Math.max(px, r), w - r);
        const cy = Math.min(Math.max(py, r), h - r);
        const d = Math.hypot(px - cx, py - cy) - r;
        const cov = Math.min(1, Math.max(0, 0.5 - d));
        if (cov > 0) put(x0 + x, y0 + y, c, cov);
      }
    }
  }

  function line(ax, ay, bx, by, w, c) {
    const minX = Math.floor(Math.min(ax, bx) - w), maxX = Math.ceil(Math.max(ax, bx) + w);
    const minY = Math.floor(Math.min(ay, by) - w), maxY = Math.ceil(Math.max(ay, by) + w);
    const dx = bx - ax, dy = by - ay, len2 = dx * dx + dy * dy || 1;
    for (let y = minY; y <= maxY; y++) {
      for (let x = minX; x <= maxX; x++) {
        const px = x + 0.5, py = y + 0.5;
        let t = ((px - ax) * dx + (py - ay) * dy) / len2;
        t = Math.min(1, Math.max(0, t));
        const d = Math.hypot(px - (ax + t * dx), py - (ay + t * dy)) - w / 2;
        const cov = Math.min(1, Math.max(0, 0.5 - d));
        if (cov > 0) put(x, y, c, cov);
      }
    }
  }

  function dot(cx, cy, r, c) {
    for (let y = Math.floor(cy - r - 2); y <= Math.ceil(cy + r + 2); y++) {
      for (let x = Math.floor(cx - r - 2); x <= Math.ceil(cx + r + 2); x++) {
        const d = Math.hypot(x + 0.5 - cx, y + 0.5 - cy) - r;
        const cov = Math.min(1, Math.max(0, 0.5 - d));
        if (cov > 0) put(x, y, c, cov);
      }
    }
  }

  roundRect(8 * k, 8 * k, 240 * k, 240 * k, 54 * k, NAVY);
  const nodes = [[80, 86], [176, 78], [66, 176], [168, 178], [126, 128]].map(p => [p[0] * k, p[1] * k]);
  const edges = [[0, 4], [1, 4], [2, 4], [3, 4], [0, 1], [2, 3]];
  edges.forEach(([a, b]) => line(nodes[a][0], nodes[a][1], nodes[b][0], nodes[b][1], 5 * k, [246, 244, 236, 200]));
  dot(nodes[4][0], nodes[4][1], 20 * k, AMBER);
  [0, 1, 2, 3].forEach(i => dot(nodes[i][0], nodes[i][1], 14 * k, CREAM));

  return buf;
}

/* ---- PNG 编码 ---- */
function crc32(b) {
  let c, table = [];
  for (let n = 0; n < 256; n++) { c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xEDB88320 ^ (c >>> 1) : c >>> 1; table[n] = c >>> 0; }
  let crc = 0xFFFFFFFF;
  for (let i = 0; i < b.length; i++) crc = table[(crc ^ b[i]) & 0xFF] ^ (crc >>> 8);
  return (crc ^ 0xFFFFFFFF) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(typeof zlib.crc32 === "function" ? zlib.crc32(td) >>> 0 : crc32(td) >>> 0);
  return Buffer.concat([len, td, crc]);
}
function toPng(S) {
  const buf = render(S);
  const raw = Buffer.alloc((S * 4 + 1) * S);
  for (let y = 0; y < S; y++) {
    raw[y * (S * 4 + 1)] = 0;
    buf.copy(raw, y * (S * 4 + 1) + 1, y * S * 4, (y + 1) * S * 4);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(S, 0); ihdr.writeUInt32BE(S, 4);
  ihdr[8] = 8; ihdr[9] = 6; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
    chunk("IHDR", ihdr),
    chunk("IDAT", zlib.deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0))
  ]);
}

/* ---- 多尺寸 ICO ---- */
function toIco(sizes) {
  const pngs = sizes.map(s => ({ size: s, png: toPng(s) }));
  const dir = Buffer.alloc(6);
  dir.writeUInt16LE(0, 0); dir.writeUInt16LE(1, 2); dir.writeUInt16LE(pngs.length, 4);
  let offset = 6 + 16 * pngs.length;
  const entries = [];
  pngs.forEach(p => {
    const e = Buffer.alloc(16);
    e[0] = p.size >= 256 ? 0 : p.size;
    e[1] = p.size >= 256 ? 0 : p.size;
    e[2] = 0; e[3] = 0;
    e.writeUInt16LE(1, 4); e.writeUInt16LE(32, 6);
    e.writeUInt32LE(p.png.length, 8);
    e.writeUInt32LE(offset, 12);
    offset += p.png.length;
    entries.push(e);
  });
  return Buffer.concat([dir, ...entries, ...pngs.map(p => p.png)]);
}

const outDir = path.join(__dirname, "src-tauri", "icons");
fs.mkdirSync(outDir, { recursive: true });

const ICO_SIZES = [16, 24, 32, 48, 64, 128, 256];
const ico = toIco(ICO_SIZES);
fs.writeFileSync(path.join(outDir, "icon.ico"), ico);
console.log("icons/icon.ico          " + (ico.length / 1024).toFixed(1) + " KB  (" + ICO_SIZES.join("/") + ")");

/* Tauri Windows 打包要求这些文件 */
const PNG_OUT = [
  ["32x32.png", 32],
  ["128x128.png", 128],
  ["128x128@2x.png", 256],
  ["icon.png", 512]
];
PNG_OUT.forEach(([name, size]) => {
  const png = toPng(size);
  fs.writeFileSync(path.join(outDir, name), png);
  console.log("icons/" + name.padEnd(17) + " " + (png.length / 1024).toFixed(1) + " KB  (" + size + "×" + size + ")");
});
