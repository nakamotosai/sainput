# -*- coding: utf-8 -*-
"""把"用户实际等待"算成一张表 —— 数据全部来自引擎自己的会话日志,不需要任何额外测试。

引擎每句都会在 `logs/sainput-*.log` 里记两行:

    T  <ms>  松键→收尾 226ms | ... | 模型停吐 1036ms | ...
    F  <ms>  (finish)  <识别的文本>

## 等待怎么量(2026-10-31 修正)

**松键→出字 = `F` 的时刻 − 松键的时刻**,而 松键的时刻 = `T` 的时刻 − `松键→收尾`。
**直接量**,不要去加 `松键→收尾 + 模型停吐` —— 那两个数**在时间上是重叠的**
(尾巴期间模型已经在吐字了),相加会把等待放大好几倍。老版本就是这么算的,
量出来 p90「4271ms」,而直接量只有「694ms」—— 差了 6 倍,顺序调参全靠这个数,错不起。
`松键→收尾` 和 `模型停吐` 仍然打出来,但它们只是**诊断量**,不是可加的分解。

## 收尾上限(见 main.rs `final_wait_ms`)

松手后最多等 `final_wait_ms`(默认 1200)就把"当前最好的一版"先打上屏。所以:
* 日志里**已经有** `收尾硬上限 ... 到点(实际等 Nms)` 那一行的句子 → 用它实测的等待;
* 没有那一行的句子(改动前的日志,或上限没到点)→ 按 `min(实测等待, 松键→收尾 + 上限)` 算反事实。
  这样**同一批旧日志**也能回答"加上限会变成什么样",不必让用户重新说一遍。

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

# 尾巴那一行:第一段=松键→收尾,模型停吐=诊断量(与第一段重叠,不可相加)
RE_TAIL = re.compile(r"^T\s+(\d+)ms\s+松键→收尾 (\d+)ms.*?模型停吐 (\d+)ms", re.M)
RE_FINAL = re.compile(r"^F\s+(\d+)ms", re.M)
# 收尾上限触发那一行(见 main.rs `final_wait_ms`),紧跟在同一句的 T 行之后
RE_CAP = re.compile(r"收尾硬上限 (\d+)ms 到点\(实际等 (\d+)ms\)")
# 超过这个值的样本按"日志残留/挂住"处理:实测见过 19,398,607ms(=5.4 小时)这种值,
# 它是上一次挂住的会话留下的,混进来会把 p99 直接带偏。
OUTLIER_MS = 30_000
# **必须排除测试运行**:假侧车(`fake-asr`)、`--feed` 喂文件、以及长静音探针,
# 它们会产生"模型停吐几秒"的假样本 —— 2026-10-31 实测踩到:只看最近 40 份日志时
# p50 变成 4242ms,全是自己的测试留在 logs/ 里污染出来的 ✗。真实使用不含这些标记。
TEST_MARKERS = ("fake-asr", "feed 模式", "probe", "selftest")


def collect(directory, last, cap_ms=1200):
    """返回 (文件列表, 每句的样本, 丢掉的异常数, 跳过的测试文件数)。

    样本 = dict(wait, tail, stall, cap, after) —— `wait` 是实测等待,
    `after` 是按上限折算后的等待(触发了上限的句子 `after == wait`)。
    """
    files = sorted(glob.glob(os.path.join(directory, "sainput-*.log")))
    if last and last > 0:
        files = files[-last:]
    rows, skipped, skipped_files, orphan = [], 0, 0, 0
    for f in files:
        try:
            txt = open(f, encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        if any(mk in txt for mk in TEST_MARKERS):
            skipped_files += 1
            continue
        tails = list(RE_TAIL.finditer(txt))
        finals = sorted(int(m.group(1)) for m in RE_FINAL.finditer(txt))
        for i, m in enumerate(tails):
            t_ms, tail, stall = int(m.group(1)), int(m.group(2)), int(m.group(3))
            if stall > OUTLIER_MS or tail > OUTLIER_MS:
                skipped += 1
                continue
            # 松键时刻 = 尾巴结束 − 尾巴时长(尾巴结束的那一刻就是 T 行打印的时刻)
            release = t_ms - tail
            after = [x for x in finals if x >= release]
            if not after:
                # 这一句没有定稿(空句 / 被打断 / 焦点丢了):不参与等待统计,但要报个数,
                # 否则"句数"会和日志里的句数对不上,看起来像丢数据。
                orphan += 1
                continue
            wait = after[0] - release
            if wait <= 0 or wait > OUTLIER_MS:
                skipped += 1
                continue
            # 上限那一行在 T 行之后、下一个 T 行之前
            nxt = tails[i + 1].start() if i + 1 < len(tails) else len(txt)
            c = RE_CAP.search(txt, m.end(), nxt)
            cap = int(c.group(2)) if c else None
            # 触发了上限的句子:实测等待**已经**是上限后的结果,不能再折算一次
            folded = wait if cap is not None else min(wait, tail + cap_ms)
            rows.append(dict(wait=wait, tail=tail, stall=stall, cap=cap, after=folded))
    return files, rows, skipped, skipped_files, orphan


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(len(xs) * p))]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", default="logs", help="会话日志目录(默认 logs)")
    ap.add_argument("--last", type=int, default=0, help="只看最近 N 份日志(0=全部)")
    ap.add_argument("--cap", type=int, default=1200, help="收尾上限 ms(默认 1200,和引擎的 final_wait_ms 一致)")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    directory = a.dir if os.path.isabs(a.dir) else os.path.join(root, a.dir)

    files, rows, skipped, skipped_files, orphan = collect(directory, a.last, a.cap)
    if not rows:
        print(f"没在 {directory} 里找到可用样本(日志里要有 '松键→收尾' 和它的 F 行才算)")
        return 2
    waits = [r["wait"] for r in rows]
    after = [r["after"] for r in rows]
    tails = [r["tail"] for r in rows]
    stalls = [r["stall"] for r in rows]
    fired = [r for r in rows if r["cap"] is not None]
    thresholds = sorted({r["cap"] for r in fired})

    if a.json:
        print(json.dumps({
            "files": len(files), "sentences": len(rows), "skipped_outliers": skipped,
            "no_final": orphan,
            "wait_ms": {f"p{int(q*100)}": pct(waits, q) for q in (0.5, 0.75, 0.9, 0.95, 0.99)},
            "wait_ms_max": max(waits),
            "tail_ms": {"p50": int(st.median(tails)), "p90": pct(tails, 0.9)},
            "stall_ms_diagnostic": {"p50": int(st.median(stalls)), "p90": pct(stalls, 0.9)},
            "over": {str(t): round(sum(1 for x in waits if x > t) * 100.0 / len(waits), 1)
                     for t in (800, 1200, 2000, 3000)},
            "cap": {
                "assumed_ms": a.cap, "fired_sentences": len(fired),
                "fired_thresholds_ms": thresholds,
                "wait_ms_after": {f"p{int(q*100)}": pct(after, q) for q in (0.5, 0.75, 0.9, 0.95, 0.99)},
                "wait_ms_after_max": max(after),
                "over_after": {str(t): round(sum(1 for x in after if x > t) * 100.0 / len(after), 1)
                               for t in (800, 1200, 2000, 3000)},
            },
        }, ensure_ascii=False, indent=1))
        return 0

    print(f"日志 {len(files)} 份(已排除测试运行 {skipped_files} 份)| 真实句 {len(rows)} 句"
          + (f" | 排掉异常 {skipped} 句(>{OUTLIER_MS}ms,日志残留)" if skipped else "")
          + (f" | {orphan} 句没有定稿(不计)" if orphan else ""))
    print("\n说完(松键)之后到出字 —— 用户实际等待(**直接量**,不是两段相加):")
    print("  {:<8}{:>8}{:>8}{:>8}{:>8}{:>8}{:>9}".format("", "p50", "p75", "p90", "p95", "p99", "max"))
    print("  {:<8}{:>8}{:>8}{:>8}{:>8}{:>8}{:>9}".format(
        "实测", *(pct(waits, q) for q in (0.5, 0.75, 0.9, 0.95, 0.99)), max(waits)))
    print("  {:<8}{:>8}{:>8}{:>8}{:>8}{:>8}{:>9}".format(
        "上限后", *(pct(after, q) for q in (0.5, 0.75, 0.9, 0.95, 0.99)), max(after)))
    print("\n诊断量(**相互重叠,不可相加**):")
    print(f"  等静音   中位 {int(st.median(tails))}ms  p90 {pct(tails, 0.9)}ms")
    print(f"  模型停吐 中位 {int(st.median(stalls))}ms  p90 {pct(stalls, 0.9)}ms"
          f"   ← 慢的那几秒都在这儿")
    print("\n分布:")
    for t in (800, 1200, 2000, 3000):
        b = sum(1 for x in waits if x > t) * 100.0 / len(waits)
        a2 = sum(1 for x in after if x > t) * 100.0 / len(after)
        print(f"  超过 {t:>5}ms: 实测 {b:5.1f}%  →  上限后 {a2:5.1f}%")
    if fired:
        print(f"\n收尾上限真正触发过 {len(fired)} 句(阈值 {'/'.join(str(x) for x in thresholds)}ms)")
    else:
        print(f"\n这批日志里上限**没有**触发过(都是改动前的日志,或没到点就定稿了)"
              f" —— 「上限后」那一行是按 {a.cap}ms 折算的反事实。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
