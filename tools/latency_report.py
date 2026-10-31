# -*- coding: utf-8 -*-
"""把"用户实际等待"算成一张表 —— 数据全部来自引擎自己的会话日志,不需要任何额外测试。

为什么用它:用户要的第一件事是"速度能不能更快",而在改动之前必须先有基线。
引擎每句都会在 `logs/sainput-*.log` 里记两行:

    T  <ms>  松键→收尾 226ms | ... | 模型停吐 1036ms | ...
    F  <ms>  (finish)  <识别的文本>

`松键→收尾` = 等静音那半段;`模型停吐` = 松键之后等模型把最后一个字吐完那半段。
两段之和 = 用户"说完到看见字"的等待。

用法:
    python tools/latency_report.py                 # 全部日志
    python tools/latency_report.py --last 20       # 只看最近 20 份(改前/改后各跑一次做 A/B)
    python tools/latency_report.py --dir logs --json
"""
import argparse
import glob
import json
import os
import re
import statistics as st
import sys

RE_TAIL = re.compile(r"松键→收尾 (\d+)ms.*?模型停吐 (\d+)ms")
RE_FINAL = re.compile(r"^F\s+(\d+)ms", re.M)
# 超过这个值的样本按"日志残留/挂住"处理:实测见过 19,398,607ms(=5.4 小时)这种值,
# 它是上一次挂住的会话留下的,混进来会把 p99 直接带偏。
OUTLIER_MS = 30_000
# **必须排除测试运行**:假侧车(`fake-asr`)、`--feed` 喂文件、以及长静音探针,
# 它们会产生"模型停吐几秒"的假样本 —— 2026-10-31 实测踩到:只看最近 40 份日志时
# p50 变成 4242ms,全是自己的测试留在 logs/ 里污染出来的 ✗。真实使用不含这些标记。
TEST_MARKERS = ("fake-asr", "feed 模式", "probe", "selftest")


def collect(directory, last):
    files = sorted(glob.glob(os.path.join(directory, "sainput-*.log")))
    if last and last > 0:
        files = files[-last:]
    rows, skipped, skipped_files = [], 0, 0
    for f in files:
        try:
            txt = open(f, encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        if any(mk in txt for mk in TEST_MARKERS):
            skipped_files += 1
            continue
        for m in RE_TAIL.finditer(txt):
            tail, stall = int(m.group(1)), int(m.group(2))
            if stall > OUTLIER_MS or tail > OUTLIER_MS:
                skipped += 1
                continue
            rows.append((tail, stall))
    return files, rows, skipped, skipped_files


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(len(xs) * p))]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", default="logs", help="会话日志目录(默认 logs)")
    ap.add_argument("--last", type=int, default=0, help="只看最近 N 份日志(0=全部)")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    directory = a.dir if os.path.isabs(a.dir) else os.path.join(root, a.dir)

    files, rows, skipped, skipped_files = collect(directory, a.last)
    if not rows:
        print(f"没在 {directory} 里找到可用样本(日志里有 '松键→收尾'/'模型停吐' 才算)")
        return 2
    tails = [r[0] for r in rows]
    stalls = [r[1] for r in rows]
    total = sorted(t + s for t, s in rows)

    if a.json:
        print(json.dumps({
            "files": len(files), "sentences": len(rows), "skipped_outliers": skipped,
            "total_ms": {f"p{int(q*100)}": pct(total, q) for q in (0.5, 0.75, 0.9, 0.95, 0.99)},
            "tail_ms": {"p50": int(st.median(tails)), "p90": pct(tails, 0.9)},
            "stall_ms": {"p50": int(st.median(stalls)), "p90": pct(stalls, 0.9)},
            "over": {str(t): round(sum(1 for x in total if x > t) * 100.0 / len(total), 1)
                     for t in (800, 1200, 2000, 3000)},
        }, ensure_ascii=False, indent=1))
        return 0

    print(f"日志 {len(files)} 份(已排除测试运行 {skipped_files} 份)| 真实句 {len(rows)} 句"
          + (f" | 排掉异常 {skipped} 句(>{OUTLIER_MS}ms,日志残留)" if skipped else ""))
    print("\n说完(松键)之后到出字 —— 用户实际等待:")
    print("  {:<8}{:>8}{:>8}{:>8}{:>8}{:>8}".format("", "p50", "p75", "p90", "p95", "p99"))
    print("  {:<8}{:>8}{:>8}{:>8}{:>8}{:>8}".format(
        "合计", *(pct(total, q) for q in (0.5, 0.75, 0.9, 0.95, 0.99))))
    print("\n拆成两段(改成什么样才算好,看这两行):")
    print(f"  等静音   中位 {int(st.median(tails))}ms  p90 {pct(tails, 0.9)}ms")
    print(f"  等模型   中位 {int(st.median(stalls))}ms  p90 {pct(stalls, 0.9)}ms"
          f"   ← 慢的那 10% 基本都在这里")
    print("\n分布:")
    for t in (800, 1200, 2000, 3000):
        n = sum(1 for x in total if x > t)
        print(f"  超过 {t:>5}ms: {n:>5} 句 ({n * 100.0 / len(total):5.1f}%)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
