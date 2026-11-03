#!/usr/bin/env python3
"""把 notes.json 的最新更新说明同步进官网 index.html 的静态兜底区块。

运行时会 fetch notes.json 覆盖页面（断网时用户看到的是 index.html 里的静态副本），
所以发布新版本时必须同时更新静态副本，否则断网用户会停留在旧说明。
历史记录列表保持原样；这里只改「正式版」面板顶部的标题、日期和要点。

用法：
  python patch-site-index.py                 # 使用同目录 notes.json / index.html
  python patch-site-index.py --check         # 只检查是否已同步（CI 用）
  python patch-site-index.py --self-test     # 版本与转义不变量检查
"""
from __future__ import annotations

import argparse
import html
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_NOTES = HERE / "coomi-site" / "notes.json"
DEFAULT_INDEX = HERE / "coomi-site" / "index.html"

STABLE_PANEL = re.compile(
    r'(<div data-channel-panel="stable">)(.*?)(<details class="changelog-history">)',
    re.S,
)
HEADING = re.compile(r'(<h2 class="changelog-title">).*?(</h2>)', re.S)
SUB = re.compile(r'(<p class="changelog-sub">).*?(</p>)', re.S)
LIST = re.compile(r'(<ul class="changelog-list">).*?(</ul>)', re.S)


def fail(message: str) -> None:
    print(f"FAIL {message}", file=sys.stderr)
    raise SystemExit(1)


def replace_once(pattern: re.Pattern[str], replacement: str, text: str, label: str) -> str:
    updated, count = pattern.subn(replacement, text, count=1)
    if count != 1:
        fail(f"{label}: expected exactly one match, got {count}")
    print(f"ok   {label}")
    return updated


def render_notes(note: dict) -> tuple[str, str, str]:
    version = str(note.get("version") or "").strip()
    date = str(note.get("date") or "").strip()
    items = note.get("items")
    if not version:
        fail("notes.json: stable.version is required")
    if not isinstance(items, list) or not items:
        fail("notes.json: stable.items must be a non-empty array")
    heading = f"v{version} 更新说明【稳定】"
    sub = f"{date} 发布" if date else ""
    body = "".join(f"<li>{html.escape(str(item))}</li>" for item in items)
    return heading, sub, body


def patch(index_text: str, note: dict) -> str:
    heading, sub, body = render_notes(note)
    match = STABLE_PANEL.search(index_text)
    if not match:
        fail("index.html: stable channel panel not found")
    start, inner, end = match.groups()
    inner = replace_once(HEADING, lambda m: f"{m.group(1)}{heading}{m.group(2)}", inner, "heading")
    if sub:
        inner = replace_once(SUB, lambda m: f"{m.group(1)}{sub}{m.group(2)}", inner, "date")
    inner = replace_once(LIST, lambda m: f"{m.group(1)}{body}{m.group(2)}", inner, "items")
    return index_text[: match.start()] + start + inner + end + index_text[match.end():]


def self_test() -> None:
    note = {"version": "1.5.0", "date": "2026-10-30", "items": ["a < b & c", '"quoted"']}
    heading, sub, body = render_notes(note)
    assert heading == "v1.5.0 更新说明【稳定】"
    assert sub == "2026-10-30 发布"
    assert "a &lt; b &amp; c" in body and "&quot;quoted&quot;" in body
    sample = (
        '<div data-channel-panel="stable">'
        '<h2 class="changelog-title">old</h2>'
        '<p class="changelog-sub">old</p>'
        '<ul class="changelog-list"><li>old</li></ul>'
        '<details class="changelog-history"></details></div>'
    )
    patched = patch(sample, note)
    assert "v1.5.0 更新说明【稳定】" in patched
    assert "2026-10-30 发布" in patched
    assert "a &lt; b &amp; c" in patched
    print("self-test ok")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--notes", type=Path, default=DEFAULT_NOTES)
    parser.add_argument("--index", type=Path, default=DEFAULT_INDEX)
    args = parser.parse_args()

    if args.self_test:
        self_test()
        return

    try:
        note = json.loads(args.notes.read_text(encoding="utf-8")).get("stable")
    except (OSError, ValueError) as error:
        fail(f"cannot read {args.notes}: {error}")
    if not note:
        fail(f"{args.notes}: missing stable section")
    original = args.index.read_text(encoding="utf-8")
    updated = patch(original, note)
    if args.check:
        if updated != original:
            fail("index.html is out of sync with notes.json")
        print("check ok")
        return
    if updated != original:
        args.index.write_text(updated, encoding="utf-8")
        print(f"wrote {args.index}")
    else:
        print("already in sync")


if __name__ == "__main__":
    main()
