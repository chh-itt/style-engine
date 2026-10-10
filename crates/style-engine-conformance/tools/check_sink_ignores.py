#!/usr/bin/env python3
"""Sink 忽略卫生门禁（块 A：soft/vello/tiny 三 sink parity）。

扫描 style-engine-soft / style-engine-vello / style-engine-tiny 源码中的
「静默忽略」形态：
  let _ = expr;        # 显式丢弃
  let _name = expr;    # 下划线前缀假绑定
  expr.ok();           # Result 丢弃（.ok() 返回值未消费）
策略：全部违规即失败——parity 目标是「每一条样式影响都进 Sink 渲染」，
静默忽略 = 行为断层。确需忽略的少数合法场景（如只读探针的返回值）列入
ALLOWLIST（文件路径子串 + 片段子串双匹配），白名单条目若不再命中也算
失败（防陈旧条目掩盖回归）。

用法：python tools/check_sink_ignores.py   （仓库根或任意 CWD 皆可）
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
SCAN_ROOTS = [
    TOOLS.parent.parent / "style-engine-soft" / "src",
    TOOLS.parent.parent / "style-engine-vello" / "src",
    TOOLS.parent.parent / "style-engine-tiny" / "src",
]

# (文件路径子串, 片段子串)——两段都命中才视为白名单匹配。
ALLOWLIST: list[tuple[str, str]] = [
    # 初始为空：soft 的 let _ = radius / let _ = text_align 已由
    # Border radius / Text align 支持落地移除；后续确需忽略时在此登记
    # 并附一行理由注释。
]

# 匹配三种形态（单行）：
#  1) let _ = ...;  2) let _name = ...;（_ 后接字母/数字）  3) ...ok();（尾缀丢弃）
PATTERNS = [
    re.compile(r"^\s*let\s+_(?=[\s=])(?:[a-z0-9_]*)?\s*=", re.IGNORECASE),
    re.compile(r"\.ok\(\)\s*;\s*$"),
]


def find_ignores(root: Path) -> list[tuple[Path, int, str]]:
    hits: list[tuple[Path, int, str]] = []
    for rs in root.rglob("*.rs"):
        for lineno, line in enumerate(
            rs.read_text(encoding="utf-8").splitlines(), start=1
        ):
            stripped = line.strip()
            if stripped.startswith("//"):
                continue
            if any(p.search(line) for p in PATTERNS):
                hits.append((rs, lineno, stripped))
    return hits


def main() -> int:
    # Windows 控制台默认 GBK：显式 UTF-8 防 ::error 中文注记乱码。
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8")
    all_hits: list[tuple[Path, int, str]] = []
    for root in SCAN_ROOTS:
        if not root.exists():
            print(f"扫描根缺失：{root}", file=sys.stderr)
            return 2
        all_hits.extend(find_ignores(root))

    unlisted = [
        (f, n, s)
        for (f, n, s) in all_hits
        if not any(fp in str(f) and sn in s for fp, sn in ALLOWLIST)
    ]
    # 白名单条目未命中（含整个文件路径对不上）= 陈旧条目，一并报。
    stale = [
        (fp, sn)
        for fp, sn in ALLOWLIST
        if not any(fp in str(f) and sn in s for f, _, s in all_hits)
    ]

    for f, n, s in unlisted:
        print(f"::error file={f},line={n}::静默忽略未登记：{s}")
    for fp, sn in stale:
        print(f"::error::白名单条目失效（未命中任何现场）：{fp} · {sn}")

    if unlisted or stale:
        print(
            f"sink 忽略卫生失败：{len(unlisted)} 处未登记，{len(stale)} 条陈旧白名单",
            file=sys.stderr,
        )
        return 1
    print(
        f"sink 忽略卫生通过：soft/vello/tiny 源码 0 处静默忽略（白名单 {len(ALLOWLIST)} 条）"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
