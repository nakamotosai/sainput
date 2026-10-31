# -*- coding: utf-8 -*-
"""T2.1 回归:薄壳里的 panic 不会带崩宿主应用。

为什么这条是硬要求:薄壳被加载进**目标应用自己的进程**(Word / 飞书 / 浏览器……)。
我们崩一次,崩的是别人正在用的程序 —— 所以每个跨 FFI 的入口都必须 `catch_unwind` 兜住,
失败只丢这一次调用(`E_FAIL`),进程继续活着。

做法:薄壳里有个诊断开关(环境变量 `SAINPUT_TSF_PANIC_TEST`),会让 `Activate` 直接 panic。
判据:
  1. `Activate` 返回 `E_FAIL`(0x80004005),不是崩溃、也不是 S_OK;
  2. 宿主进程**正常退出**(exit 0)—— 它活着走完了后面每一步;
  3. 薄壳日志里没有任何"进程没了"的痕迹(日志是它自己写的,说明它还写过别的行)。

换一套位数组合来跑(32 位薄壳 + 32 位宿主):
    cargo build --release --target i686-pc-windows-msvc --target-dir target-test-msvc -p sainput-tsf -p sainput-tsf-host
    SAINPUT_TSF_DIR=target-test-msvc/i686-pc-windows-msvc/release python tools/test_tsf_activate.py

跑法:
    cargo build --release --target-dir target-test -p sainput-tsf -p sainput-tsf-host
    python tools/test_tsf_panic.py
"""
import os
import os
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hdesk  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parent.parent
DIRS = ["target-test/x86_64-pc-windows-gnu/release", "target-test/release",
        "target/x86_64-pc-windows-gnu/release", "target/release"]
DESKTOP = "SAI-TsfPanic"


def find(rel):
    override = os.environ.get("SAINPUT_TSF_DIR")
    if override and rel != "sainput.exe":
        p = pathlib.Path(override) / rel
        if p.exists():
            return p
    for d in DIRS:
        p = REPO / d / rel
        if p.exists():
            return p
    print(f"FAIL: {rel} not built")
    sys.exit(2)


def main():
    host = find("sainput-tsf-host.exe")
    dll = find("sainput_tsf.dll")
    tmp = REPO / "tmp"
    tmp.mkdir(exist_ok=True)
    hlog = tmp / "panic-host.log"
    if hlog.exists():
        hlog.unlink()

    os.environ["SAINPUT_TSF_PANIC_TEST"] = "1"
    h = hdesk.open_desktop(DESKTOP)
    hp = ht = None
    try:
        hp, ht, pid = hdesk.launch(str(host), f"--life 1500 --log {hlog} --load-tip {dll}", DESKTOP)
        code = hdesk.wait_exit(hp, 30000)
    finally:
        os.environ.pop("SAINPUT_TSF_PANIC_TEST", None)
        if hp:
            hdesk.close(hp)
        if ht:
            hdesk.close(ht)
        hdesk.close(h)

    txt = hlog.read_text(encoding="utf-8", errors="replace") if hlog.exists() else ""
    checks = [
        ("宿主进程活着并正常退出", code == 0, f"exit={code}"),
        ("Activate 返回 E_FAIL(不是崩溃、也不是 S_OK)",
         "ITfTextInputProcessor::Activate: FAIL (hr=0x80004005" in txt, ""),
        ("宿主继续走完了后续步骤", "Deactivate" in txt, ""),
    ]
    for name, ok, detail in checks:
        print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    bad = [n for n, ok, _ in checks if not ok]
    if bad:
        print("\n--- host log ---\n" + txt)
    print("\n" + ("ALL PASS" if not bad else f"FAILED: {', '.join(bad)}"))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
