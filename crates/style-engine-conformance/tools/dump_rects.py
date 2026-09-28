#!/usr/bin/env python3
"""ADR-0003 Numeric Channel 基准生成器。

对每个 case 目录（case.html + case.css + manifest.toml）用 Playwright
Chromium 无头打开 case.html，收集 [data-key] 元素的 getBoundingClientRect，
连同浏览器版本元数据写入 golden/numeric.json。

用法：
    python tools/dump_rects.py                # 全部 case
    python tools/dump_rects.py box-model      # 指定 case（目录名）

约定：
- 视口/缩放取自 manifest.toml（viewport, scale）。
- 字体注入（manifest.fonts）为后续文本用例预留：@font-face 以 file://
  指向仓库内字体文件；当前 v0 用例不含文本，fonts 留空。
- golden 内含 browser_version——基准与 Chrome 版本绑定，升级需重生成。
"""
import json
import sys
import tomllib
from pathlib import Path

from playwright.sync_api import sync_playwright

TOOLS = Path(__file__).resolve().parent
CASES = TOOLS.parent / "cases"

COLLECT_JS = """
() => Array.from(document.querySelectorAll('[data-key]')).map(el => {
  const r = el.getBoundingClientRect();
  return { key: Number(el.dataset.key), rect: [r.x, r.y, r.width, r.height] };
})
"""


def dump_case(case_dir: Path, browser) -> None:
    manifest = tomllib.loads((case_dir / "manifest.toml").read_text("utf-8"))
    vw, vh = manifest.get("viewport", [800.0, 600.0])
    page = browser.new_page(
        viewport={"width": int(vw), "height": int(vh)},
        device_scale_factor=manifest.get("scale", 1.0),
    )
    page.goto((case_dir / "case.html").as_uri())
    page.wait_for_load_state("networkidle")
    boxes = page.evaluate(COLLECT_JS)
    meta = {
        "generator": "tools/dump_rects.py",
        "browser": "chromium",
        "browser_version": browser.version,
        "viewport": [vw, vh],
        "device_scale_factor": manifest.get("scale", 1.0),
        "fonts": manifest.get("fonts", []),
    }
    golden_dir = case_dir / "golden"
    golden_dir.mkdir(exist_ok=True)
    out = {"meta": meta, "boxes": boxes}
    (golden_dir / "numeric.json").write_text(
        json.dumps(out, indent=1, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    print(f"{case_dir.name}: {len(boxes)} boxes -> golden/numeric.json")
    page.close()


def main() -> int:
    wanted = set(sys.argv[1:])
    dirs = sorted(d for d in CASES.iterdir() if (d / "manifest.toml").exists())
    if wanted:
        dirs = [d for d in dirs if d.name in wanted]
    if not dirs:
        print("no cases matched", file=sys.stderr)
        return 2
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        for d in dirs:
            dump_case(d, browser)
        browser.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
