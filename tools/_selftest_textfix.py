# -*- coding: utf-8 -*-
import sys, io, os
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")
import textfix

cases = [
    # (输入, 期望)  —— 大小写修复
    ("MONDAY TODAY IS THE DAY", "monday today is the day"),
    ("我用 API 调的", "我用 API 调的"),
    ("VPS 上跑的 CODEX", "VPS 上跑的 CODEX"),
    ("HELLO WORLD", "hello world"),
    ("这个 AI 和 API 都要", "这个 AI 和 API 都要"),
    ("AFTER TWO DAYS", "after two days"),
    ("Random Person Name", "Random Person Name"),   # 名单外 -> 原样
    # 和既有逻辑叠加
    ("我CODEX还在做R E A任务", "我CODEX还在做REA任务"),
]
ok = bad = 0
for src, want in cases:
    got = textfix.normalize(src)
    if got == want: ok += 1; print("OK  ", repr(src), "->", repr(got))
    else:
        bad += 1
        print("FAIL", repr(src))
        print("     期望:", repr(want))
        print("     实得:", repr(got))
print()
print("=== %d passed / %d failed ===" % (ok, bad))
sys.exit(1 if bad else 0)
