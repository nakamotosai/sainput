# -*- coding: utf-8 -*-
"""T1.2 回归:TSF 薄壳能在真实进程里被装起来、走完整条 COM 激活链。

判据(`PLAN-NEXT.md` T1.2):
  1. `LoadLibrary` → `DllGetClassObject` → `IClassFactory::CreateInstance`
     → `ITfTextInputProcessor::Activate(ptim, tid)` 全部 hr=0;
  2. 引用计数正确:还有活对象时 `DllCanUnloadNow` = `S_FALSE`,全释放后 = `S_OK`;
  3. 薄壳**自己写的日志**里确实出现那三行(证明回调真的进了我们的代码,不是宿主自说自话)。

为什么不用真机注册就能测:msctf 装 TIP 走的就是上面这几步,我们原样走一遍;
差别只在"谁调 DllGetClassObject"。所以这一步不需要 UAC、也不碰用户任何状态。

换一套位数组合来跑(32 位薄壳 + 32 位宿主):
    cargo build --release --target i686-pc-windows-msvc --target-dir target-test-msvc -p sainput-tsf -p sainput-tsf-host
    SAINPUT_TSF_DIR=target-test-msvc/i686-pc-windows-msvc/release python tools/test_tsf_activate.py

跑法:
    cargo build --release --target-dir target-test -p sainput-tsf-host -p sainput-tsf
    python tools/test_tsf_activate.py
"""
import os
import os
import pathlib
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hdesk  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parent.parent
DIRS = ["target-test/x86_64-pc-windows-gnu/release", "target-test/release",
        "target/x86_64-pc-windows-gnu/release", "target/release"]
DESKTOP = "SAI-TipLoad"
MARKERS = ["DllGetClassObject -> ClassFactory", "CreateInstance -> TextService", "Activate tid="]


def find(rel, what):
    # `SAINPUT_TSF_DIR` = 换一套"薄壳 + 宿主"来跑(例如 32 位那套:
    # target-test-msvc/i686-pc-windows-msvc/release)。引擎不跟着换 —— 它本来就是另一个进程。
    override = os.environ.get("SAINPUT_TSF_DIR")
    if override and rel != "sainput.exe":
        p = pathlib.Path(override) / rel
        if p.exists():
            return p
    for d in DIRS:
        p = REPO / d / rel
        if p.exists():
            return p
    print(f"FAIL: {what} not built (cargo build --release --target-dir target-test -p sainput-tsf-host -p sainput-tsf)")
    sys.exit(2)


def main():
    host = find("sainput-tsf-host.exe", "host")
    dll = find("sainput_tsf.dll", "sainput_tsf.dll")
    tmp = REPO / "tmp"
    tmp.mkdir(exist_ok=True)
    hlog = tmp / "tip-host.log"
    dlog = pathlib.Path(os.environ.get("TEMP", r"C:\Windows\Temp")) / "sainput-tsf.log"
    for p in (hlog, dlog):
        if p.exists():
            p.unlink()

    h = hdesk.open_desktop(DESKTOP)
    hp = ht = None
    try:
        hp, ht, pid = hdesk.launch(str(host), f"--life 1500 --log {hlog} --load-tip {dll}", DESKTOP)
        code = hdesk.wait_exit(hp, 30000)
    finally:
        if hp:
            hdesk.close(hp)
        if ht:
            hdesk.close(ht)
        hdesk.close(h)

    host_txt = hlog.read_text(encoding="utf-8", errors="replace") if hlog.exists() else ""
    dll_txt = dlog.read_text(encoding="utf-8", errors="replace") if dlog.exists() else ""
    checks = [
        ("宿主退出码 0", code == 0),
        ("宿主判定 PASS", "tip RESULT PASS" in host_txt),
        ("COM 链每步 hr=0", host_txt.count("OK (hr=0x00000000") >= 5),
        ("活对象时不可卸载", "DllCanUnloadNow(有活对象): OK" in host_txt),
        ("释放后可卸载", "DllCanUnloadNow(释放后): OK" in host_txt),
    ] + [(f"薄壳日志含 {m!r}", m in dll_txt) for m in MARKERS]

    for name, ok in checks:
        print(f"[{'PASS' if ok else 'FAIL'}] {name}")
    bad = [n for n, ok in checks if not ok]
    if bad:
        print("\n--- host log ---\n" + host_txt)
        print("--- dll log ---\n" + dll_txt)
    print("\n" + ("ALL PASS" if not bad else f"FAILED: {', '.join(bad)}"))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
