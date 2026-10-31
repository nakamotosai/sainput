# -*- coding: utf-8 -*-
"""T6.6 边缘光帧率的 CPU 代价实测:**隔离出边缘光自己吃多少核**。

为什么这么测:直接比"60fps 总 CPU"没有意义 —— 里面混着 ASR 解码(占大头)。
所以三组对照:
    off     边缘光根本不显示(只有 ASR)
    edge60  边缘光 60fps
    edge30  边缘光 30fps
**边缘光自己的代价 = edgeX − off**;60 与 30 的差 = 降帧率省下来的那部分。

窗口是**整屏**的,所以这一项是"全屏重绘 × 每秒帧数",量出来就是实打实的省多少。

跑法:
    cargo build --release
    python tools/probe_edge_cpu.py            # 三组各 ~45 秒

⚠️ 引擎跑在**私有桌面**(你看不见),`SAINPUT_SINGLE_NS` 隔离,不接管你正在用的实例。
⚠️ 会用进程内自动按键让引擎**连续录音 40 秒**(这样边缘光一直是亮的,便于量),
   不合成任何按键事件;`--dry-run` 保证不往任何窗口打字。
"""
import ctypes
import ctypes.wintypes as wt
import os
import pathlib
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hdesk  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parent.parent
DESKTOP = "SAI-EdgeCpu"
NS = "sainput-edgecpu"
ENV_KEYS = ("SAINPUT_SINGLE_NS", "SAINPUT_DIAG_AUTOSTART", "SAINPUT_DIAG_TARGET_HWND")
k32 = ctypes.WinDLL("kernel32", use_last_error=True)


def cpu_seconds(pid):
    """进程累计 CPU 时间(秒)= GetProcessTimes 的 user+kernel。"""
    h = k32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not h:
        return None
    try:
        c, e, k, u = (wt.FILETIME(), wt.FILETIME(), wt.FILETIME(), wt.FILETIME())
        if not k32.GetProcessTimes(h, ctypes.byref(c), ctypes.byref(e), ctypes.byref(k), ctypes.byref(u)):
            return None
        f = lambda t: (t.dwHighDateTime << 32) | t.dwLowDateTime  # noqa: E731
        return (f(k) + f(u)) / 1e7
    finally:
        k32.CloseHandle(h)


def one(sainput, host, overlay_mode, fps, settle=12.0, window=20.0):
    d = hdesk.open_desktop(DESKTOP)
    saved = {k: os.environ.get(k) for k in ENV_KEYS}
    hh = hp = None
    hpid = epid = 0
    try:
        os.environ["SAINPUT_SINGLE_NS"] = NS
        os.environ["SAINPUT_DIAG_AUTOSTART"] = "40000"  # 自动"按住"40 秒:边缘光一直亮
        hh, _ht, hpid = hdesk.launch(
            str(host), f"--life 90000 --log {REPO / 'tmp' / 'edgecpu-host.log'}", DESKTOP)
        edit = None
        t0 = time.time()
        while time.time() - t0 < 10:
            edit = hdesk.find_edit(hpid, d)
            if edit:
                break
            time.sleep(0.2)
        os.environ["SAINPUT_DIAG_TARGET_HWND"] = hex(edit)
        args = f"run --no-tray --dry-run --overlay-mode {overlay_mode}"
        if fps:
            args += f" --edge-fps {fps}"
        hp, _hpt, epid = hdesk.launch(str(sainput), args, DESKTOP)
        time.sleep(settle)  # 让模型加载 + 第一句跑起来(稳态)
        c0 = None
        t0 = time.time()
        while time.time() - t0 < 5 and c0 is None:
            c0 = cpu_seconds(epid)
            time.sleep(0.3)
        time.sleep(window)
        c1 = cpu_seconds(epid)
        if c0 is None or c1 is None:
            raise RuntimeError("拿不到 CPU 时间")
        return (c1 - c0) / window  # 平均占用几个核
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


def main():
    sainput = REPO / "target/x86_64-pc-windows-gnu/release/sainput.exe"
    host = REPO / "target/x86_64-pc-windows-gnu/release/sainput-tsf-host.exe"
    for p, what in ((sainput, "引擎"), (host, "宿主")):
        if not p.exists():
            print(f"FAIL: {what} 没构建({p})")
            return 2
    res = {}
    for label, mode, fps in (("off", "off", None), ("edge60", "edge", 60), ("edge30", "edge", 30)):
        cores = one(sainput, host, mode, fps)
        res[label] = cores
        print(f"  {label:8s} 平均占用 {cores:.3f} 核")
    base = res["off"]
    e60 = res["edge60"] - base
    e30 = res["edge30"] - base
    print(f"\n边缘光自身开销:60fps = {e60:.3f} 核,30fps = {e30:.3f} 核")
    if e60 > 0.001:
        print(f"降帧率省下 {(1 - e30 / e60) * 100:.0f}%(绝对值 {e60 - e30:.3f} 核)")
    else:
        print("没量出边缘光开销(窗口可能没显示:检查 --overlay-mode edge 是否被配置覆盖)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
