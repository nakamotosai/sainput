# -*- coding: utf-8 -*-
"""T6.1/T6.6 回归:边缘光的帧率/带宽/亮度/速度能在托盘里现场改,并且写回 sainput.conf。

为什么这么测(和仓库既有的 `tools/test_tray_menu.ps1` 同一套原理):
截图只能看"长相",验证不了"点完配置有没有变"。这里直接给托盘窗口发
`WM_COMMAND(id)`,然后读配置文件 —— 判据是**可机检**的:
  帧率 30fps(370+1) → edge_fps  = 30
  带宽 宽(340+2) → edge_spread = 76
  亮度 亮(350+2) → edge_peak   = 0.62
  速度 慢(360+0) → edge_speed  = 0.6

⚠️ 会改 `sainput.conf`:跑前备份、跑后还原(否则会动用户真实配置)。
⚠️ 引擎带 `SAINPUT_SINGLE_NS` 跑,绝不接管用户正在用的实例。
⚠️ 引擎跑在**私有桌面**上,用户看不见。

跑法:
    cargo build --release --target-dir target-test -p sainput-cli
    python tools/test_edge_params.py
"""
import os
import pathlib
import re
import shutil
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hdesk  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parent.parent
DIRS = ["target-test/x86_64-pc-windows-gnu/release", "target-test/release",
        "target/x86_64-pc-windows-gnu/release", "target/release"]
DESKTOP = "SAI-EdgeParams"
NS = "sainput-edgetest"
WM_COMMAND = 0x0111
TRAY_CLASS = "SainputTrayWnd"
# 与 tray.rs 里的基址一致(改了那边这里也要改)
ID_EDGE_SPREAD_BASE = 340
ID_EDGE_PEAK_BASE = 350
ID_EDGE_SPEED_BASE = 360
ID_EDGE_FPS_BASE = 370


def find(rel, what):
    for d in DIRS:
        p = REPO / d / rel
        if p.exists():
            return p
    print(f"FAIL: {what} not built")
    sys.exit(2)


def conf_value(path, key):
    """读 sainput.conf 里某个键的值(没有就 None)。"""
    if not path.exists():
        return None
    txt = path.read_text(encoding="utf-8", errors="replace")
    m = re.search(rf"^\s*{re.escape(key)}\s*=\s*(\S+)\s*$", txt, re.M)
    return m.group(1) if m else None


def find_tray(d, pid):
    """在那张桌面上找托盘窗口。"""
    for hwnd, cls, _title, _vis in hdesk.enum_windows(pid, d):
        if cls == TRAY_CLASS:
            return hwnd
    return None


def main():
    sainput = find("sainput.exe", "engine")
    conf = REPO / "sainput.conf"
    backup = None
    if conf.exists():
        backup = conf.with_suffix(".conf.bak-edge-test")
        shutil.copy2(conf, backup)

    saved_env = {k: os.environ.get(k) for k in ("SAINPUT_SINGLE_NS",)}
    fails = []
    d = hdesk.open_desktop(DESKTOP)
    ep = None
    epid = 0
    try:
        os.environ["SAINPUT_SINGLE_NS"] = NS
        ep, ept, epid = hdesk.launch(str(sainput), "run", DESKTOP)
        tray = None
        t0 = time.time()
        while time.time() - t0 < 40:
            tray = find_tray(d, epid)
            if tray:
                break
            time.sleep(0.5)
        if not tray:
            print("FAIL: 托盘窗口没起来(引擎没跑到托盘那一步?)")
            return 1
        print(f"托盘窗口 = {tray:#x} (pid {epid})")

        cases = [
            ("帧率 → 30fps(省电)", ID_EDGE_FPS_BASE + 1, "edge_fps", "30"),
            ("带宽 → 宽(76px)", ID_EDGE_SPREAD_BASE + 2, "edge_spread", "76"),
            ("亮度 → 亮(0.62)", ID_EDGE_PEAK_BASE + 2, "edge_peak", "0.62"),
            ("速度 → 慢(0.6×)", ID_EDGE_SPEED_BASE + 0, "edge_speed", "0.6"),
        ]
        for name, cid, key, want in cases:
            hdesk.u32.SendMessageW(tray, WM_COMMAND, cid, 0)
            got = None
            t0 = time.time()
            while time.time() - t0 < 5:
                got = conf_value(conf, key)
                if got == want:
                    break
                time.sleep(0.2)
            ok = got == want
            print(f"[{'PASS' if ok else 'FAIL'}] {name}: {key} = {got!r}(期望 {want!r})")
            if not ok:
                fails.append(name)
        hdesk.close(ept)
    finally:
        if ep:
            hdesk.terminate(epid)
            hdesk.close(ep)
        hdesk.close(d)
        # 还原用户的配置(测试改过它)
        if backup and backup.exists():
            shutil.copy2(backup, conf)
            backup.unlink()
        for k, v in saved_env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    print("\n" + ("ALL PASS" if not fails else f"FAILED: {', '.join(fails)}"))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
