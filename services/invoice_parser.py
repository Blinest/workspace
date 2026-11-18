#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""
发票 PDF 解析服务（工作台「发票管家」后端）
监听 127.0.0.1:3923
  POST /parse      body: {"path": "绝对路径"}   或 {"paths": [...]}
  POST /scan       body: {"dir": "目录"}        批量扫描目录
  GET  /health

解析字段：发票号码、开票日期、购买方/销售方、金额、税额、价税合计、税率、项目名称、开票人
"""
import io
import json
import os
import re
import sys
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pymupdf

PORT = 3923

# ---- OCR（可选）：图片型 PDF 用 rapidocr 识别 ----
_ocr_engine = None
_ocr_failed = False


def get_ocr():
    global _ocr_engine, _ocr_failed
    if _ocr_engine is not None:
        return _ocr_engine
    if _ocr_failed:
        return None
    try:
        from rapidocr_onnxruntime import RapidOCR
        _ocr_engine = RapidOCR()
    except Exception:
        _ocr_failed = True
        _ocr_engine = None
    return _ocr_engine


def ocr_pdf_pages(doc, max_pages=4):
    """把 PDF 每页渲染成图后 OCR，返回识别到的纯文本。"""
    eng = get_ocr()
    if eng is None:
        return ""
    import tempfile
    texts = []
    for i, page in enumerate(doc):
        if i >= max_pages:
            break
        pix = page.get_pixmap(dpi=200)
        tmp = os.path.join(tempfile.gettempdir(), "inv_ocr_%d_%d.png" % (os.getpid(), i))
        try:
            pix.save(tmp)
            res, _ = eng(tmp)
            if res:
                # 按纵向位置排序后拼接，尽量还原阅读顺序
                rows = []
                for item in res:
                    box, txt = item[0], item[1]
                    ys = [p[1] for p in box]
                    xs = [p[0] for p in box]
                    rows.append((min(ys), min(xs), txt))
                rows.sort(key=lambda r: (round(r[0] / 12), r[1]))
                texts.append("\n".join(r[2] for r in rows))
        except Exception:
            pass
        finally:
            try:
                os.remove(tmp)
            except Exception:
                pass
    return "\n".join(texts)


def s(v):
    return "" if v is None else str(v)


def parse_invoice_pdf(path):
    """解析一张电子发票 PDF，返回结构化字段。"""
    if not os.path.isfile(path):
        raise FileNotFoundError(path)

    doc = pymupdf.open(path)
    text = ""
    for page in doc:
        text += page.get_text()
    used_ocr = False
    if not text.strip():
        # 图片型 PDF（如手机 APP 出的行程单）：走 OCR
        text = ocr_pdf_pages(doc)
        used_ocr = True
    doc.close()

    if not text.strip():
        raise ValueError("PDF 里没有可提取的文本，OCR 也未能识别（可能 OCR 引擎未安装）")

    lines = [l.strip() for l in text.splitlines() if l.strip()]
    _USED_OCR = used_ocr

    # 识别是否真的是发票：普通发票 / 航空行程单 / 火车票都算
    is_air = "客票行程单" in text or ("航空运输电子客票" in text)
    is_invoice = is_air or ("发票" in text and "价税合计" in text) or "发票号码" in text
    has_no = bool(re.search(r"\b\d{20}\b", text)) or bool(re.search(r"发票号码[:：]\s*(\d{10,25})", text))
    if not (is_invoice and has_no):
        raise ValueError("不是电子发票（缺少发票特征或发票号码）")

    out = {
        "ok": True,
        "ocr": _USED_OCR,
        "file": os.path.basename(path),
        "path": path,
        "invoiceNo": "",
        "date": "",
        "buyer": "",
        "buyerTaxId": "",
        "seller": "",
        "sellerTaxId": "",
        "amount": None,
        "tax": None,
        "total": None,
        "taxRate": "",
        "item": "",
        "drawer": "",
        "ticketNo": "",
        "ocr": False,
        "raw": text[:2000],
    }

    # 发票号码：20 位纯数字
    for l in lines:
        if re.fullmatch(r"\d{20}", l):
            out["invoiceNo"] = l
            break

    # 开票日期：YYYY年MM月DD日
    m = re.search(r"(\d{4})\s*年\s*(\d{1,2})\s*月\s*(\d{1,2})\s*日", text)
    if m:
        out["date"] = "%s-%02d-%02d" % (m.group(1), int(m.group(2)), int(m.group(3)))

    # ---- 航空运输电子客票行程单（字段名与普通发票不同）----
    if is_air:
        # 发票号码：OCR 后可能带空格
        m = re.search(r"发票号码[:：]\s*([0-9]{10,25})", text.replace(" ", ""))
        if m:
            out["invoiceNo"] = m.group(1)
        # 填开日期（行程单用「填开日期」而非「开票日期」）
        m = re.search(r"填开日期[:：]?\s*(\d{4})\s*年\s*(\d{1,2})\s*月\s*(\d{1,2})\s*日", text)
        if not m:
            m = re.search(r"(\d{4})\s*年\s*(\d{1,2})\s*月\s*(\d{1,2})\s*日", text)
        if m:
            out["date"] = "%s-%02d-%02d" % (m.group(1), int(m.group(2)), int(m.group(3)))
        # 销售方 = 填开单位
        m = re.search(r"填开单位[:：]?\s*([^\n]+)", text)
        if m:
            v = m.group(1).strip()
            v = v.replace("（APP渠道)", "（APP渠道）")
            out["seller"] = v
        # 购买方名称
        m = re.search(r"购买方名称[:：]?\s*([^\n]+)", text)
        if m:
            v = m.group(1).strip()
            out["buyer"] = re.sub(r"^购买方名称[:：]?\s*", "", v)
        # 合计金额：行程单有「合计」栏，金额形如 CNY 1560.00
        totals = re.findall(r"CNY\s*([0-9]+(?:\.[0-9]{1,2})?)", text)
        if totals:
            vals = [float(x) for x in totals]
            out["total"] = max(vals)
            # 票价作为不含税金额
            m = re.search(r"票价[^\n]*?CNY\s*([0-9]+(?:\.[0-9]{1,2})?)", text)
            if not m:
                m = re.search(r"CNY\s*([0-9]+(?:\.[0-9]{1,2})?)", text)
            if m:
                out["amount"] = float(m.group(1))
            m = re.search(r"增值税税额?\s*CNY\s*([0-9]+(?:\.[0-9]{1,2})?)", text)
            if m:
                out["tax"] = float(m.group(1))
            elif out["amount"] is not None:
                out["tax"] = round(out["total"] - out["amount"], 2)
            m = re.search(r"增值税税率\s*([0-9]+(?:\.[0-9]+)?)\s*%", text)
            if m:
                out["taxRate"] = m.group(1) + "%"
        # 项目：航线 + 航班号
        route = re.findall(r"[自至][:：]\s*([\u4e00-\u9fa5]{2,8})", text)
        flight = re.search(r"\b([A-Z]{2}\d{3,4})\b", text)
        parts = []
        if len(route) >= 2:
            parts.append("%s-%s" % (route[0], route[1]))
        elif route:
            parts.append(route[0])
        if flight:
            parts.append(flight.group(1))
        if parts:
            out["item"] = "航空客票 " + " ".join(parts)
        else:
            out["item"] = "航空客票"
        # 客票号
        m = re.search(r"电子客票号码[:：]\s*([0-9]{10,20})", text)
        if m:
            out["ticketNo"] = m.group(1)

    # 统一社会信用代码（18 位，字母数字）
    tids = re.findall(r"\b([0-9A-Z]{18})\b", text)
    tids = [t for t in tids if re.search(r"\d", t)]
    if tids:
        out["buyerTaxId"] = tids[0]
        if len(tids) > 1:
            out["sellerTaxId"] = tids[1]
    else:
        # 少数是 15/17/20 位
        tids = re.findall(r"\b([0-9A-Z]{15,20})\b", text)
        tids = [t for t in tids if re.search(r"\d", t) and not re.fullmatch(r"\d{20}", t)]
        if tids:
            out["buyerTaxId"] = tids[0]
            if len(tids) > 1:
                out["sellerTaxId"] = tids[1]

    # 名称：买家名称固定是南京航空航天大学这类，按「名称：」后的顺序取
    # 文本里名称值出现在标签块之后，这里用排除法：出现两次的「名称」标签，
    # 真实值按 buyer/seller 的固定顺序排在税额块之前
    names = [] if is_air else [l for l in lines if l.endswith("公司") or l.endswith("商行")
             or l.endswith("厂") or l.endswith("店") or l.endswith("中心")
             or l.endswith("大学") or l.endswith("学院") or l.endswith("研究所")
             or l.endswith("个体户）") or l.endswith("（个体）")]
    if names:
        # 第一个通常是购买方
        out["buyer"] = names[0]
        if len(names) > 1:
            out["seller"] = names[1]

    # ---- 金额 ----
    # 各厂商排版顺序不同，不能按出现顺序取。
    # 策略：收集所有「小数位≤2 的货币数字」，用「金额 + 税额 = 价税合计」的关系反推。
    digits_with_cur = []
    # ¥ 与数字可能跨行：¥\n0.86 或 ¥86.40 都视为一个金额
    for m in re.finditer(r"[¥￥]\s*\n?\s*([0-9]+(?:\.[0-9]{1,2})?)(?![0-9.])", text):
        v = m.group(1)
        if len(v.replace(".", "")) > 12:
            continue
        digits_with_cur.append(float(v))
    # 也收集光秃秃的金额行（部分发票不带 ¥）
    bare = [float(x) for x in re.findall(r"(?m)^([0-9]+(?:\.[0-9]{1,2})?)$", text)
            if len(x.replace(".", "")) <= 12]

    cands = list(dict.fromkeys(digits_with_cur + bare))

    # 用「大写金额」交叉校验：捌拾陆圆肆角整 → 86.40
    cn_num = {"零":0,"壹":1,"贰":2,"叁":3,"肆":4,"伍":5,"陆":6,"柒":7,"捌":8,"玖":9}
    cn_unit = {"拾":10,"佰":100,"仟":1000}
    def cn_to_num(t):
        m = re.search(r"([零壹贰叁肆伍陆柒捌玖拾佰仟万亿圆元角分整]{3,})", t)
        if not m:
            return None
        s2 = m.group(1)
        if ("圆" not in s2 and "元" not in s2) or len(s2) < 3:
            return None
        # 必须至少含一个中文数字
        if not any(c in cn_num for c in s2):
            return None
        int_part, _, frac_part = re.split(r"([圆元])", s2, maxsplit=1)[1:] + [""] if False else (None, None, None)
        # 简单解析：整数部分 + 角分
        head, sep, tail = re.split(r"([圆元])", s2, maxsplit=1) + ["", ""] if False else (s2, "", "")
        mm = re.match(r"(.*?)[圆元](.*)", s2)
        if not mm:
            return None
        ip, fp = mm.group(1), mm.group(2)
        total = 0
        cur = 0
        for ch in ip:
            if ch in cn_num:
                cur = cn_num[ch]
            elif ch in cn_unit:
                total += (cur or 1) * cn_unit[ch]
                cur = 0
        total += cur
        jiao = re.search(r"([零壹贰叁肆伍陆柒捌玖])角", fp)
        fen = re.search(r"([零壹贰叁肆伍陆柒捌玖])分", fp)
        val = total
        if jiao:
            val += cn_num[jiao.group(1)] / 10
        if fen:
            val += cn_num[fen.group(1)] / 100
        return round(val, 2)

    upper = cn_to_num(text)

    best = None
    # 1) 大写金额精确匹配（最可靠，多页发票也只认最终合计）
    if upper is not None:
        for c in cands:
            if abs(c - upper) < 0.005:
                best = c
                break
    # 2) 大写没匹配上：找 a + b = c 的组合
    if best is None:
        for c in sorted(cands, reverse=True):
            hit = None
            for a in cands:
                if a >= c:
                    continue
                for b in cands:
                    if abs((a + b) - c) < 0.02:
                        hit = (a, b, c)
                        break
                if hit:
                    break
            if hit:
                out["amount"], out["tax"], out["total"] = hit
                best = hit[2]
                break
    # 3) 兜底：取最大金额
    if best is None and cands:
        best = max(cands)
    if out["total"] is None and best is not None:
        out["total"] = best
    if out["total"] is not None and out["amount"] is None:
        smaller = [c for c in cands if c < out["total"] - 0.001]
        if smaller:
            out["amount"] = max(smaller)
            out["tax"] = round(out["total"] - out["amount"], 2)
        else:
            out["amount"] = out["total"]
            out["tax"] = 0.0
    # 税率
    m = re.search(r"(\d+(?:\.\d+)?)\s*%", text)
    if m:
        out["taxRate"] = m.group(1) + "%"
    elif "免税" in text:
        out["taxRate"] = "免税"

    # 项目名称：形如 *金属制品*步进电机麦轮
    m = re.search(r"\*([^*\n]+)\*\s*([^\n]*)", text)
    if m:
        out["item"] = (m.group(1) + " " + m.group(2)).strip()
    else:
        for l in lines:
            if l.startswith("*"):
                out["item"] = l.replace("*", " ").strip()
                break

    # 开票人
    for i, l in enumerate(lines):
        if l.startswith("开票人"):
            if i + 1 < len(lines):
                nxt = lines[i + 1]
                if nxt and not re.match(r"^[¥￥\d]", nxt) and len(nxt) <= 10:
                    out["drawer"] = nxt
            break
    if not out["drawer"]:
        cand = [l for l in lines if 2 <= len(l) <= 4 and re.fullmatch(r"[\u4e00-\u9fa5]{2,4}", l)]
        if cand:
            out["drawer"] = cand[-1]

    # 兜底：如果金额没解析出来，从文件名推断
    if out["total"] is None:
        m = re.search(r"([0-9]+(?:\.[0-9]{1,2})?)\s*元", out["file"])
        if m:
            out["total"] = float(m.group(1))
            out["amount"] = out["total"]
    if not out["date"]:
        m = re.search(r"(\d{4})\.(\d{2})\.(\d{2})", out["file"])
        if m:
            out["date"] = "%s-%s-%s" % (m.group(1), m.group(2), m.group(3))

    return out


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def _send(self, code, obj):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.send_header("Access-Control-Allow-Methods", "GET,POST,OPTIONS")
        self.end_headers()
        self.wfile.write(body)

    def do_OPTIONS(self):
        self._send(200, {"ok": True})

    def do_GET(self):
        if self.path.startswith("/health"):
            self._send(200, {"ok": True, "port": PORT})
        else:
            self._send(404, {"ok": False, "error": "not found"})

    def do_POST(self):
        try:
            n = int(self.headers.get("Content-Length") or 0)
            raw = self.rfile.read(n) if n else b"{}"
            req = json.loads(raw.decode("utf-8") or "{}")
        except Exception as e:
            self._send(400, {"ok": False, "error": "bad json: %s" % e})
            return

        try:
            if self.path.startswith("/parse"):
                paths = req.get("paths")
                if not paths:
                    p = req.get("path")
                    paths = [p] if p else []
                results = []
                for p in paths:
                    try:
                        results.append(parse_invoice_pdf(p))
                    except Exception as e:
                        results.append({"ok": False, "path": p,
                                        "file": os.path.basename(p),
                                        "error": str(e)})
                if req.get("path") and not req.get("paths"):
                    self._send(200, results[0])
                else:
                    self._send(200, {"ok": True, "items": results})
                return

            if self.path.startswith("/scan"):
                d = req.get("dir") or ""
                if not os.path.isdir(d):
                    self._send(400, {"ok": False, "error": "目录不存在: %s" % d})
                    return
                results = []
                skipped = []
                for name in sorted(os.listdir(d)):
                    if not name.lower().endswith(".pdf"):
                        continue
                    p = os.path.join(d, name)
                    try:
                        results.append(parse_invoice_pdf(p))
                    except Exception as e:
                        skipped.append({"file": name, "reason": str(e)})
                self._send(200, {"ok": True, "items": results, "skipped": skipped})
                return
                self._send(200, {"ok": True, "items": results})
                return

            self._send(404, {"ok": False, "error": "not found"})
        except Exception as e:
            self._send(500, {"ok": False, "error": str(e), "trace": traceback.format_exc()[-800:]})


def main():
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    sys.stderr.write("[invoice] 发票解析服务已启动 http://127.0.0.1:%d\n" % PORT)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()

