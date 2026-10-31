# -*- coding: utf-8 -*-
"""T6.3 回归:采集回调**稳态零分配**(帧缓冲真的在循环,不是在反复新开)。

为什么必须这么测:第一版"复用 resampled + 帧池"我自以为是通的 —— 单测过了、
真设备探针也只看到 1 次新开。但**真麦路径**实测"池未命中"随句数线性涨
(46→83→121→…),因为生产路径上有三处取帧的下游没把缓冲还回去
(录音主循环、空闲排堆积×2)。这类漏洞只有把**真麦 + 完整 run 循环**跑起来才看得见,
所以这条回归就照那个口径做:私有桌面 + 真设备 + 进程内自动按键,一句一句看数字。

判据(都可机检):
  1. `回收失败` 必须恒为 0;
  2. 稳态:**未命中不再随句数增长**(末句 - 首句 ≤ 16),且末句 `复用/出帧 ≥ 0.9`;
  3. 每句都出过帧(证明链路真的在跑,而不是"没采到所以没错")。

⚠️ 会打开**默认麦克风**若干秒(和产品本身一样,只统计不落盘)。
⚠️ 引擎跑在私有桌面 + `SAINPUT_SINGLE_NS` 隔离,不打扰、也不接管正在用的实例。

跑法:
    cargo build --release
    python tools/test_frame_pool.py
"""
import os
import pathlib
import re
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hdesk  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parent.parent
DESKTOP = "SAI-FramePool"
NS = "sainput-framepool"
ENV_KEYS = ("SAINPUT_SINGLE_NS", "SAINPUT_DIAG_AUTOSTART", "SAINPUT_DIAG_TARGET_HWND")
POOL_RE = re.compile(r"帧池 出(\d+) 未命中(\d+) 复用(\d+) 回收失败(\d+)")
CAND = [
    "target/x86_64-pc-windows-gnu/release",
    "target/release",
]


def first(rel, what):
    for d in CAND:
        p = REPO / d / rel
        if p.exists():
            return p
    print(f"FAIL: {what} 不存在,先 cargo build --release")
    sys.exit(2)


def main():
    sainput = first("sainput.exe", "引擎")
    host = first("sainput-tsf-host.exe", "靶子宿主")
    d = hdesk.open_desktop(DESKTOP)
    saved = {k: os.environ.get(k) for k in ENV_KEYS}
    hh = hp = None
    hpid = epid = 0
    try:
        os.environ["SAINPUT_SINGLE_NS"] = NS
        os.environ["SAINPUT_DIAG_AUTOSTART"] = "1500"
        hh, ht, hpid = hdesk.launch(
            str(host), f"--life 60000 --log {REPO / 'tmp' / 'framepool-host.log'}", DESKTOP)
        edit = None
        t0 = time.time()
        while time.time() - t0 < 10:
            edit = hdesk.find_edit(hpid, d)
            if edit:
                break
            time.sleep(0.2)
        if not edit:
            print("FAIL: 宿主窗口没起来")
            return 1
        os.environ["SAINPUT_DIAG_TARGET_HWND"] = hex(edit)

        before = {p.name for p in (REPO / "logs").glob("*.log")}
        hp, hpt, epid = hdesk.launch(
            str(sainput), "run --hotkey rightctrl --no-tray --no-overlay --dry-run", DESKTOP)
        # 跑到攒够 4 句(自动按键:按住 1500ms / 间隔 1500ms)
        log = None
        rows = []
        t0 = time.time()
        while time.time() - t0 < 40 and len(rows) < 4:
            new = [p for p in (REPO / "logs").glob("*.log") if p.name not in before]
            if new:
                log = max(new, key=lambda p: p.stat().st_mtime)
                txt = log.read_text(encoding="utf-8", errors="replace")
                rows = [tuple(int(x) for x in m.groups()) for m in POOL_RE.finditer(txt)]
            time.sleep(0.5)
        hdesk.close(hpt)

        if not rows:
            print("FAIL: 一句都没跑出来(麦克风没起来?看 tmp/framepool-host.log 与 logs/)")
            return 1
        fails = []
        for i, (out, miss, reuse, failed) in enumerate(rows, 1):
            print(f"  第 {i} 句:出帧 {out} 未命中 {miss} 复用 {reuse} 回收失败 {failed}")
            if out == 0:
                fails.append(f"第{i}句没出帧")
            if failed != 0:
                fails.append(f"第{i}句回收失败 {failed}")
        first_miss = rows[0][1]
        last_out, last_miss, last_reuse, _ = rows[-1]
        if last_miss - first_miss > 16:
            fails.append(f"未命中仍在增长({first_miss}→{last_miss})=下游没把缓冲还回来")
        if last_out and last_reuse / last_out < 0.9:
            fails.append(f"末句复用率只有 {last_reuse}/{last_out}")
        print(
            f"\n稳态检查:未命中 {first_miss}→{last_miss}(应停住)| "
            f"末句复用 {last_reuse}/{last_out} = "
            f"{(last_reuse / last_out * 100 if last_out else 0):.1f}%"
        )
        if fails:
            print("FAIL: " + "; ".join(fails))
            return 1
        print("PASS: 回调稳态零分配")
        return 0
    finally:
        if hp:
            hdesk.terminate(epid)
            hdesk.close(hp)
        if hh:
            hdesk.close(hh)
        hdesk.close(d)
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


if __name__ == "__main__":
    sys.exit(main())
