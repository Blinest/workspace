# 知乎桥接：让浏览器调用 zhihu-cli(搜索/热榜)，返回整理好的文本
# GET http://127.0.0.1:3921/search?q=关键词&count=8   /  /hot?limit=15
import json
import os
import subprocess
import sys
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

def _find_cli():
    """定位 zhihu-cli.exe。

    ⚠ 不再写死本机路径 —— 这样同一份脚本能在服务器上跑。
    查找顺序：
      1. 环境变量 ZHIHU_CLI
      2. 脚本同目录 / 同目录 bin\ 下的 zhihu-cli.exe
      3. 老的本机安装位置（兼容旧环境）
    """
    env = (os.environ.get("ZHIHU_CLI") or "").strip()
    if env and os.path.isfile(env):
        return env
    here = os.path.dirname(os.path.abspath(__file__))
    for rel in ("zhihu-cli.exe", os.path.join("bin", "zhihu-cli.exe")):
        p = os.path.join(here, rel)
        if os.path.isfile(p):
            return p
    legacy = os.path.join(os.environ.get("LOCALAPPDATA", ""), "ZhihuCLI", "current", "zhihu-cli.exe")
    if os.path.isfile(legacy):
        return legacy
    return "zhihu-cli"      # 交给 PATH 找

BIN = _find_cli()


def _run(args):
    r = subprocess.run([BIN] + args, capture_output=True, text=True,
                       encoding="utf-8", timeout=90)
    if r.returncode != 0:
        raise RuntimeError((r.stderr or "").strip() or ("exit %s" % r.returncode))
    return r.stdout


def _search(q, count):
    j = json.loads(_run(["search", "zhihu", "--query", q, "--count", str(count)]))
    items = (j.get("Data") or {}).get("Items") or []
    out = []
    for it in items:
        out.append("### " + (it.get("Title") or "").strip())
        meta = []
        if it.get("AuthorName"):
            meta.append("作者：" + str(it.get("AuthorName")))
        if it.get("VoteUpCount") is not None:
            meta.append("赞同 " + str(it.get("VoteUpCount")))
        if it.get("CommentCount") is not None:
            meta.append("评论 " + str(it.get("CommentCount")))
        if meta:
            out.append("（" + " · ".join(meta) + "）")
        txt = (it.get("ContentText") or "").strip()
        if txt:
            out.append(txt[:600])
        if it.get("Url"):
            out.append(str(it.get("Url")))
    return "\n\n".join(out)


def _hot(limit):
    j = json.loads(_run(["hot", "--limit", str(limit)]))
    items = ((j.get("Data") or {}).get("Items")) or []
    out = []
    for it in items:
        t = (it.get("Title") or "").strip()
        if t:
            out.append("### " + t)
        s = (it.get("Summary") or "").strip()
        if s:
            out.append(s[:400])
        if it.get("Url"):
            out.append(str(it.get("Url")))
    return "\n\n".join(out)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _json(self, code, obj):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.send_header("Access-Control-Allow-Methods", "GET,POST,OPTIONS")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_OPTIONS(self):
        self._json(200, {"ok": True})

    def do_GET(self):
        q = urllib.parse.urlparse(self.path)
        query = urllib.parse.parse_qs(q.query)
        try:
            if q.path.endswith("/hot"):
                limit = int((query.get("limit") or ["15"])[0])
                text = _hot(limit)
                self._json(200, {"ok": True, "title": "知乎热榜", "text": text})
            else:
                kw = (query.get("q") or [""])[0]
                count = int((query.get("count") or ["8"])[0])
                text = _search(kw, count)
                self._json(200, {"ok": True, "title": "知乎搜索：" + kw, "text": text})
        except Exception as e:
            self._json(500, {"ok": False, "error": str(e)[:500]})


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", 3921), Handler).serve_forever()
