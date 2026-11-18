# 本地抓取服务：用 scrapling 抓取并清洗正文（无需 token）
# 用法: GET/POST http://127.0.0.1:3920/fetch?url=...  或 POST JSON {"url": "..."}
import json
import os
import re
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

try:
    from scrapling.fetchers import Fetcher
except Exception as e:  # pragma: no cover
    raise SystemExit("scrapling 不可用: %r" % e)

try:
    from lxml import html as lh
    from markdownify import markdownify as _md
except Exception:
    lh = None
    _md = None


def _clean(s):
    s = re.sub(r"[ \t\u00a0]+", " ", s)
    s = re.sub(r"\n\s*\n+", "\n\n", s)
    return s.strip()


def _dedupe(text):
    # 合并相邻重复行、压缩连续空行
    lines = text.splitlines()
    out = []
    prev = None
    for l in lines:
        s = l.strip()
        if not s:
            if out and out[-1] == "":
                continue
            out.append("")
            continue
        if s == prev:
            continue
        out.append(l)
        prev = s
    return re.sub(r"\n\s*\n+", "\n\n", "\n".join(out)).strip()


def _host(url):
    try:
        return urllib.parse.urlparse(url).netloc.lower()
    except Exception:
        return ""


def _meta(root, key):
    if root is None:
        return ""
    try:
        for el in root.cssselect('meta[property="%s"]' % key) + root.cssselect('meta[name="%s"]' % key):
            c = (el.get("content") or "").strip()
            if c:
                return c
    except Exception:
        pass
    return ""


def _search_bing(root, parsed):
    out = []
    for li in root.cssselect("li.b_algo"):
        a = li.cssselect("h2 a")
        if not a:
            a = li.cssselect("a")
        if not a:
            continue
        a = a[0]
        title = " ".join(a.text_content().split())
        href = a.get("href", "")
        snippet = ""
        p = li.cssselect(".b_caption p")
        if not p:
            p = li.cssselect("p")
        if p:
            snippet = " ".join(p[0].text_content().split())
        if title:
            out.append("### %s\n%s\n%s" % (title, href, snippet))
    return out


def _main_html(root):
    # 优先正文容器，减少导航/页脚噪音
    for sel in ("article", "main", '[role="main"]', "#content", ".content",
                ".article", "#mainContent", ".post-content", ".entry-content"):
        try:
            nodes = root.cssselect(sel)
        except Exception:
            continue
        if nodes:
            nodes = [n for n in nodes if len(" ".join(n.text_content().split())) > 200]
            if nodes:
                return nodes[0]
    return None


def _baike(root):
    out = []
    main = None
    for sel in (".main-content", "#bodyContent", ".lemma-summary", "#content"):
        try:
            n = root.cssselect(sel)
            if n:
                main = n[0]
                break
        except Exception:
            pass
    scope = main if main is not None else root
    try:
        els = scope.cssselect("h2,h3,h4,div.para,p,li")
    except Exception:
        els = []
    prev = None
    for el in els:
        t = " ".join(el.text_content().split())
        if not t or t == prev:
            continue
        out.append(t)
        prev = t
    return out


def _render_dyn(url):
    """用无头 Chromium 渲染动态页面（用于抓不到正文的站点）。

    ⚠ 浏览器路径不再写死本机路径，按以下顺序确定：
      1. 环境变量 SCRAPE_CHROME —— 指向 chrome-headless-shell.exe / chrome.exe
      2. 留空则交给 Playwright 自己找（它会读 PLAYWRIGHT_BROWSERS_PATH）
    这样同一份脚本既能跑在本机，也能跑在服务器上。
    """
    from playwright.sync_api import sync_playwright
    exe = (os.environ.get("SCRAPE_CHROME") or "").strip()
    if exe and not os.path.isfile(exe):
        exe = ""
    with sync_playwright() as p:
        kw = {
            "headless": True,
            "args": ["--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage"],
        }
        if exe:
            kw["executable_path"] = exe
        b = p.chromium.launch(**kw)
        try:
            pg = b.new_page()
            pg.goto(url, wait_until="domcontentloaded", timeout=40000)
            pg.wait_for_timeout(1500)
            # 分次滚动到底，触发懒加载正文
            for _ in range(6):
                try:
                    pg.evaluate("window.scrollTo(0, document.body.scrollHeight)")
                except Exception:
                    pass
                pg.wait_for_timeout(650)
            return pg.content()
        finally:
            b.close()


def _parse_html(raw, url, title):
    mode = "page"
    text = ""
    root = None
    if raw and lh is not None:
        try:
            root = lh.fromstring(raw)
        except Exception:
            root = None
    host = _host(url)
    if root is not None:
        if "baike.baidu.com" in host:
            lines = _baike(root)
            if lines:
                mode = "baike"
                text = "\n\n".join(lines)
    if not text and root is not None:
        cont = _main_html(root)
        if cont is not None:
            mode = "article"
            try:
                text = _md(lh.tostring(cont, encoding="unicode"))
            except Exception:
                text = " ".join(cont.text_content().split())
        else:
            text = " ".join(root.text_content().split())
    return mode, _clean(text)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _cors(self):
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.send_header("Access-Control-Allow-Methods", "GET,POST,OPTIONS")

    def _json(self, code, obj):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self._cors()
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_OPTIONS(self):
        self._json(200, {"ok": True})

    def _read_url(self):
        q = urllib.parse.urlparse(self.path)
        u = urllib.parse.parse_qs(q.query).get("url", [""])[0]
        if not u:
            try:
                ln = int(self.headers.get("Content-Length") or 0)
                raw = self.rfile.read(ln) if ln else b""
                u = json.loads(raw or b"{}").get("url", "")
            except Exception:
                pass
        return u

    def do_GET(self):
        q = urllib.parse.urlparse(self.path)
        url = self._read_url()
        if q.path.endswith("/dyn"):
            if not url:
                self._json(400, {"ok": False, "error": "no url"})
                return
            try:
                raw = _render_dyn(url)
                mode, text = _parse_html(raw, url, "")
                if len(text) > 200000:
                    text = text[:200000]
                self._json(200, {"ok": True, "title": "", "text": text, "mode": mode, "url": url, "dyn": True})
            except Exception as e:
                self._json(500, {"ok": False, "error": str(e)[:500], "url": url})
            return
        self.fetch(url)

    def do_POST(self):
        self.fetch(self._read_url())

    def fetch(self, url):
        if not url:
            self._json(400, {"ok": False, "error": "no url"})
            return
        try:
            p = Fetcher.get(url, timeout=25)
            title = ""
            try:
                t = p.css("title::text").get() if hasattr(p, "css") else None
                title = (t or "").strip()
            except Exception:
                pass
            raw = ""
            if hasattr(p, "html_content"):
                try:
                    raw = p.html_content
                except Exception:
                    raw = ""
            host = _host(url)
            mode = "page"
            text = ""
            if raw and lh is not None:
                try:
                    root = lh.fromstring(raw)
                except Exception:
                    root = None
                if root is not None:
                    if "bilibili.com" in host:
                        ds = _meta(root, "og:description") or ""
                        if ds:   # 有带标签的描述(播放量等)才用哔哩专用结果；否则退回通用正文
                            mt = _meta(root, "og:title") or ""
                            au = _meta(root, "author") or _meta(root, "og:video:actor") or ""
                            parts = [mt or title]
                            if au:
                                parts.append("UP主：" + au)
                            parts.append(ds)
                            parts = [x for x in parts if x]
                            mode = "bilibili"
                            text = "\n\n".join(parts)
                    if not text:
                        if host.startswith("www.bing") or host.startswith("cn.bing") or "bing.com" in host:
                            res = _search_bing(root, urllib.parse.urlparse(url))
                            if res:
                                mode = "search"
                                text = "\n\n".join(res)
                    if not text and "baike.baidu.com" in host:
                        lines = _baike(root)
                        if lines:
                            mode = "baike"
                            text = "\n\n".join(lines)
                    if not text:
                        cont = _main_html(root)
                        if cont is not None:
                            mode = "article"
                            try:
                                text = _md(lh.tostring(cont, encoding="unicode"))
                            except Exception:
                                text = " ".join(cont.text_content().split())
                        else:
                            text = " ".join(root.text_content().split())
            if not text:
                try:
                    md = p.markdown() if hasattr(p, "markdown") else ""
                    text = md or ""
                except Exception:
                    text = ""
            if not text:
                text = title
            text = _clean(text)
            if "bilibili.com" in host:
                # 信息流卡片：把「播放+弹幕 数值\n\n时长」补成带标签
                text = re.sub(
                    r"(\d+(?:\.\d+)?\u4e07?)(\d+)\s*(\d{1,2}:\d{2})",
                    r"播放 \1 · 弹幕 \2 · 时长 \3",
                    text,
                )
            text = _dedupe(text)
            if len(text) > 200000:
                text = text[:200000]
            self._json(200, {"ok": True, "title": title, "text": text, "mode": mode, "url": url})
        except Exception as e:
            self._json(500, {"ok": False, "error": str(e)[:500], "url": url})


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", 3920), Handler).serve_forever()
