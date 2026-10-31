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
# 收尾硬上限触发的那一行(见 main.rs `final_wait_ms`):
#   ! 收尾硬上限 1200ms 到点(实际等 619ms)→ 先把当前最好的一版打上屏(reason=cap,...)
# 两个数都要:配置的阈值 + 这一句**实际等了多久**(后者才是用户真实等待)。
# 它紧跟在同一句的 T 行之后,所以按"到下一句 T 行为止"配对。
RE_CAP = re.compile(r"收尾硬上限 (\d+)ms 到点\(实际等 (\d+)ms\)")
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
        hits = list(RE_TAIL.finditer(txt))
        for i, m in enumerate(hits):
            tail, stall = int(m.group(1)), int(m.group(2))
            if stall > OUTLIER_MS or tail > OUTLIER_MS:
                skipped += 1
                continue
            # 这一句的收尾上限行:在它自己的 T 行之后、下一句 T 行之前。
            nxt = hits[i + 1].start() if i + 1 < len(hits) else len(txt)
            c = RE_CAP.search(txt, m.end(), nxt)
            rows.append((tail, stall, (int(c.group(1)), int(c.group(2))) if c else None))
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
    total = sorted(t + s for t, s, _ in rows)
    # 收尾上限生效后的"实际等待":上限那一次到点就把字打上屏了,所以用户等的是
    # `松键→收尾 + min(模型停吐, 上限实际等)` —— 没触发的句子保持原样。
    # 这是**从同一批日志算出来的反事实**,不需要用户换个版本再录一遍。
    hit = [(t, s, c) for t, s, c in rows if c is not None]
    adjusted = sorted(t + (min(s, c[1]) if c else s) for t, s, c in rows)
    fired_at = sorted({c[0] for _, _, c in hit})
    # 上限**没有**帮上忙的句子:上限到点时模型其实已经吐完了(等了 c[1] 却等于没提前)。
    no_gain = sum(1 for t, s, c in hit if c[1] >= s)

    if a.json:
        print(json.dumps({
            "files": len(files), "sentences": len(rows), "skipped_outliers": skipped,
            "total_ms": {f"p{int(q*100)}": pct(total, q) for q in (0.5, 0.75, 0.9, 0.95, 0.99)},
            "tail_ms": {"p50": int(st.median(tails)), "p90": pct(tails, 0.9)},
            "stall_ms": {"p50": int(st.median(stalls)), "p90": pct(stalls, 0.9)},
            "over": {str(t): round(sum(1 for x in total if x > t) * 100.0 / len(total), 1)
                     for t in (800, 1200, 2000, 3000)},
            "cap": {
                "fired_thresholds_ms": fired_at,
                "sentences": len(hit),
                "no_gain": no_gain,
                "total_ms_after": {f"p{int(q*100)}": pct(adjusted, q) for q in (0.5, 0.75, 0.9, 0.95, 0.99)},
                "over_after": {str(t): round(sum(1 for x in adjusted if x > t) * 100.0 / len(adjusted), 1)
                               for t in (800, 1200, 2000, 3000)},
            },
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
    # 收尾上限(松手后不再干等模型):同一批日志算出的反事实 —— 上限到点那一刻字就上屏了。
    print("\n收尾上限生效后(同一批日志的反事实,不是另录一遍):")
    if not hit:
        print("  这批日志里没有触发过收尾上限(旧版本,或上限没到点就已经定稿了)")
    else:
        print(f"  触发 {len(hit)} 句(占 {len(hit) * 100.0 / len(rows):.1f}%),"
              f"其中 {no_gain} 句其实没提前(到点时模型已经吐完了);"
              f"阈值 {'/'.join(str(x) for x in fired_at)}ms")
        print("  {:<8}{:>8}{:>8}{:>8}{:>8}{:>8}".format("", "p50", "p75", "p90", "p95", "p99"))
        print("  {:<8}{:>8}{:>8}{:>8}{:>8}{:>8}".format(
            "上限前", *(pct(total, q) for q in (0.5, 0.75, 0.9, 0.95, 0.99))))
        print("  {:<8}{:>8}{:>8}{:>8}{:>8}{:>8}".format(
            "上限后", *(pct(adjusted, q) for q in (0.5, 0.75, 0.9, 0.95, 0.99))))
        for t in (800, 1200, 2000, 3000):
            b = sum(1 for x in total if x > t) * 100.0 / len(total)
            a2 = sum(1 for x in adjusted if x > t) * 100.0 / len(adjusted)
            print(f"  超过 {t:>5}ms: {b:5.1f}% → {a2:5.1f}%")
    return 0


if __name__ == "__main__":
    sys.exit(main())
