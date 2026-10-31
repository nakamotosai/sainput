# -*- coding: utf-8 -*-
"""T0.3 回归:引擎的确定性上屏入口(`sainput selftest compose`)真的能把文本打到目标窗口。

为什么需要它:`sainput` 平时靠"按住热键说话"才上屏,自动化回归绝不能这么干
(抢前台 + 要麦克风)。`selftest compose` 把同一套 `StreamWriter` 算术(前缀 / 退格 / 补打)
直接喂一段文本,并允许指定目标窗口。

**两边都得在私有桌面上跑**(2026-10-31 实测):窗口句柄是**桌面作用域**的 ——
只有创建/打开了那张桌面句柄的进程才寻址得到上面的窗口(`IsWindow` 返回 FALSE、
`PostMessage` 报 1400 ERROR_INVALID_WINDOW_HANDLE),它的子进程都不行。所以:
宿主壳用 `lpDesktop` 起在那张桌上,引擎也用同一个 `lpDesktop` 起。

跑法:
    python tools/test_compose_path.py
"""
import pathlib
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hdesk  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parent.parent
HOST_CANDIDATES = [
    REPO / "target-test/x86_64-pc-windows-gnu/release/sainput-tsf-host.exe",
    REPO / "target-test/release/sainput-tsf-host.exe",
    REPO / "target/x86_64-pc-windows-gnu/release/sainput-tsf-host.exe",
]
SAINPUT_CANDIDATES = [
    REPO / "target-test/x86_64-pc-windows-gnu/release/sainput.exe",
    REPO / "target-test/release/sainput.exe",
    REPO / "target/x86_64-pc-windows-gnu/release/sainput.exe",
]
DESKTOP = "SAI-ComposePath"
CN = "你好,这是确定性入口。(abc123)"

# T4.5 已于 2026-10-31 修好(退格风味按控件类分:经典 Edit 用 WM_CHAR(0x08),
# RichEdit/Qt 用 WM_KEYDOWN/UP)。保留下面的用例作为**回归**:两种风味各一条,
# 任何一条坏掉都必须是 FAIL,不再有"已知失败"豁免。


def first(cands, what):
    for c in cands:
        if c.exists():
            return c
    print(f"FAIL: {what} not built")
    sys.exit(2)


def wait_edit(pid, h, control="edit", timeout=10.0):
    cls = "RICHEDIT50W" if control.startswith("rich") else "Edit"
    t0 = time.time()
    while time.time() - t0 < timeout:
        e = hdesk.find_edit(pid, h, cls)
        if e:
            return e
        time.sleep(0.2)
    return None


def main():
    host_exe = first(HOST_CANDIDATES, "sainput-tsf-host.exe")
    sainput = first(SAINPUT_CANDIDATES, "sainput.exe")
    fails = []

    pre = CN[:6] + "错错错"
    cases = [
        ("一次投递(无前缀)", CN, [], CN, "edit"),
        ("流式 4 段", CN, ["--frames", "4"], CN, "edit"),
        ("句首补空格", CN, ["--prefix", "space"], " " + CN, "edit"),
        ("整句一次(最终态)", CN, ["--mode", "wmchar-final"], CN, "edit"),
        # T4.5 两种退格风味各一条:经典 Edit 走 WM_CHAR(0x08),RichEdit 走 WM_KEYDOWN/UP
        ("退格改写/经典 Edit 风味", CN, ["--pre", pre], CN, "edit"),
        ("退格改写/RichEdit 风味", CN, ["--pre", pre], CN, "rich50"),
    ]

    for name, text, extra, expect, control in cases:
        h = hdesk.open_desktop(DESKTOP)
        hp = ht = None
        try:
            hp, ht, pid = hdesk.launch(
                str(host_exe),
                f"--life 20000 --log {REPO / 'tmp' / 'compose-host.log'} --control {control}",
                DESKTOP)
            edit = wait_edit(pid, h, control)
            if not edit:
                print(f"[FAIL] {name}: 宿主壳的 EDIT 没出现")
                fails.append(name)
                continue
            # 引擎也起在同一张私有桌面上(见文件头:窗口句柄是桌面作用域的)
            ep, et, _ = hdesk.launch(
                str(sainput),
                "selftest compose " + text + f" --hwnd {hex(edit)} --mode wmchar " + " ".join(extra),
                DESKTOP)
            code = hdesk.wait_exit(ep, 30000)
            hdesk.close(et)
            hdesk.close(ep)
            got = ""
            t0 = time.time()
            while time.time() - t0 < 5:
                got = hdesk.read_edit(edit)
                if got == expect:
                    break
                time.sleep(0.1)
            ok = code == 0 and got == expect
            print(f"[{'PASS' if ok else 'FAIL'}] {name}: exit={code} text={got!r}"
                  + ("" if ok else f" expect={expect!r}"))
            if not ok:
                fails.append(name)
        finally:
            if hp:
                hdesk.terminate(pid)
                hdesk.close(hp)
            if ht:
                hdesk.close(ht)
            hdesk.close(h)

    print("\n" + ("ALL PASS" if not fails else f"FAILED: {', '.join(fails)}"))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
