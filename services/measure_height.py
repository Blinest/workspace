# -*- coding: utf-8 -*-
# 量周报页面真实宽高（供 PNG 截图定尺寸）
# 用法: python measure_height.py <html_path> <width>
import sys, io, pathlib
if sys.stdout.encoding != 'utf-8':
    try: sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8')
    except Exception: pass
from patchright.sync_api import sync_playwright

SHELL = r"C:\Users\q3634\AppData\Local\ms-playwright\chromium_headless_shell-1217\chrome-headless-shell-win64\chrome-headless-shell.exe"


def main():
    html = pathlib.Path(sys.argv[1]).resolve()
    width = int(sys.argv[2]) if len(sys.argv) > 2 else 1400
    url = "file:///" + str(html).replace("\\", "/")
    with sync_playwright() as p:
        b = p.chromium.launch(executable_path=SHELL, headless=True)
        pg = b.new_page(viewport={"width": width, "height": 900})
        pg.goto(url)
        pg.wait_for_timeout(2500)
        h = pg.evaluate("Math.max(document.body.scrollHeight, document.documentElement.scrollHeight)")
        b.close()
    # 只输出高度数字
    print(int(h))


if __name__ == "__main__":
    main()
