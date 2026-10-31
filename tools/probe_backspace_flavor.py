# -*- coding: utf-8 -*-
"""退格风味探针:目标控件到底吃哪种退格消息?(T4.5 的判据就靠它量出来)

为什么需要它(2026-10-31,T0.3 现场抓到):`inject.rs` 的消息通道退格用的是
`PostMessage(WM_KEYDOWN/UP, VK_BACK)`。在经典 `Edit` 上实测发现:
  * 关掉 `TranslateMessage`(**`--no-translate`**)→ 退格**一次都不生效**;
  * 开着 `TranslateMessage`(所有正常程序的默认行为)→ 控件的删除来自
    应用自己把这条 KEYDOWN 翻译出来的 `WM_CHAR(0x08)`,而那条消息会被排到
    **我们随后投出的字符之后** —— 于是删掉的是句尾,不是该删的那几个字。
反过来,Qt(微信)那类自己处理 `WM_KEYDOWN` 的窗口不会出这个问题(仓库另有真机探针)。

所以判据必须**按控件类分别量**,不能推理。这个探针用私有桌面上的宿主壳做靶子,
不需要前台、不需要用户在场,也不碰任何真实应用。

跑法:
    python tools/probe_backspace_flavor.py
输出:每个控件类 × (translate 开/关) 下,KEYDOWN 与 WM_CHAR(0x08) 各自删掉几个字符。
"""
import ctypes
import pathlib
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hdesk  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parent.parent
DIRS = ["target-test/x86_64-pc-windows-gnu/release", "target-test/release",
        "target/x86_64-pc-windows-gnu/release", "target/release"]
DESKTOP = "SAI-BsFlavor"

WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_SETTEXT, WM_GETTEXT = 0x0102, 0x0100, 0x0101, 0x000C, 0x000D
VK_BACK = 0x08
SCAN = 0x0E
DOWN = 1 | (SCAN << 16)
UP = 1 | (SCAN << 16) | (1 << 30) | (1 << 31)


def find_host():
    for d in DIRS:
        p = REPO / d / "sainput-tsf-host.exe"
        if p.exists():
            return p
    print("FAIL: sainput-tsf-host.exe not built")
    sys.exit(2)


def set_text(edit, s):
    """把控件内容置成 `s`,**光标停在末尾**。

    不能用 `WM_SETTEXT`:那会把光标放到开头,退格在位置 0 删不掉任何东西,
    量出来两个通道都是"无效"(2026-10-31 踩过一回)。清空后用 `WM_CHAR` 打进去,
    光标自然走到末尾 —— 这也是真实上屏的样子。
    """
    empty = ctypes.create_unicode_buffer("")
    hdesk.u32.SendMessageW(edit, WM_SETTEXT, 0, ctypes.cast(empty, ctypes.c_void_p).value)
    for ch in s:
        hdesk.u32.PostMessageW(edit, WM_CHAR, ord(ch), 0)
    time.sleep(0.3)


def probe(host, control, translate):
    """返回 (KEYDOWN 删掉几个, WM_CHAR(0x08) 删掉几个)。"""
    args = f"--life 9000 --log {REPO / 'tmp' / 'flavor-host.log'} --control {control} --dump-every 500"
    if not translate:
        args += " --no-translate"
    h = hdesk.open_desktop(DESKTOP)
    hp = ht = None
    try:
        hp, ht, pid = hdesk.launch(str(host), args, DESKTOP)
        cls = "RICHEDIT50W" if control.startswith("rich") else "Edit"
        edit = None
        t0 = time.time()
        while time.time() - t0 < 8:
            edit = hdesk.find_edit(pid, h, cls)
            if edit:
                break
            time.sleep(0.2)
        if not edit:
            return None
        out = []
        for flavor in ("keydown", "wmchar"):
            set_text(edit, "abcdef")
            if flavor == "keydown":
                hdesk.u32.PostMessageW(edit, WM_KEYDOWN, VK_BACK, DOWN)
                hdesk.u32.PostMessageW(edit, WM_KEYUP, VK_BACK, UP)
            else:
                hdesk.u32.PostMessageW(edit, WM_CHAR, VK_BACK, 0)
            time.sleep(0.4)
            out.append(6 - len(hdesk.read_edit(edit)))
        return out
    finally:
        if hp:
            hdesk.terminate(pid)
            hdesk.close(hp)
        if ht:
            hdesk.close(ht)
        hdesk.close(h)


def main():
    host = find_host()
    (REPO / "tmp").mkdir(exist_ok=True)
    print("被删掉的字符数(基线 6 个字符 abcdef;1 = 按预期删除,0 = 完全不生效)\n")
    print(f"{'控件类':<12}{'translate':<11}{'KEYDOWN/UP':<12}{'WM_CHAR(0x08)'}")
    summary = {}
    for control in ("edit", "rich50"):
        for translate in (True, False):
            r = probe(host, control, translate)
            if r is None:
                print(f"{control:<12}{str(translate):<11}(控件没起来)")
                continue
            kd, wc = r
            summary[(control, translate)] = (kd, wc)
            print(f"{control:<12}{str(translate):<11}{kd:<12}{wc}")
    print("\n结论(自动判定):")
    for (control, translate), (kd, wc) in summary.items():
        usable = "KEYDOWN 可用" if kd == 1 else ("KEYDOWN 无效" if kd == 0 else f"KEYDOWN 删了 {kd}")
        usable2 = "WM_CHAR(0x08) 可用" if wc == 1 else ("WM_CHAR(0x08) 无效" if wc == 0 else f"WM_CHAR 删了 {wc}")
        print(f"  {control} translate={translate}: {usable}; {usable2}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
