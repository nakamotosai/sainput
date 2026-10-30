# -*- coding: utf-8 -*-
"""消息通道(`WM_CHAR`)的「退格之后紧跟的那个字符会被吞」现场探针。

为什么需要它(2026-10-30):微信 4.x(Qt 5.15,挂水杉输入法)上,
`PostMessage(WM_KEYDOWN/UP, VK_BACK)` 之后**同一批**投出去的字符会被目标程序丢掉 ——
用户读到的是「我的验证码是903721」变成「我的验证码是03721」(少一个字符)。
这条只有真机能量出来,单测钉不住;`inject.rs` 里那个间隔
(`WMCHAR_BACKSPACE_SETTLE`,2026-10-31 体检后由 40ms 收到 15ms)就是照这个探针的结果定的:
同一批投出去的字符 5/5 次全丢,隔 10ms 以上 5/5 全对。

跑法(先让目标窗口开着,输入框能拿焦点;脚本自己抢前台 + 点输入框):
  python tools/probe_wmchar_backspace.py                 # 默认微信
  python tools/probe_wmchar_backspace.py --title Notepad --reps 3

⚠️ 脚本会往输入框里打字(每轮按字符数原样退格还回去),剪贴板用完还原。
   目标窗口的输入框请先清空 —— 否则读回永远不等于期望。

⛔ 安全红线(2026-10-30 踩过,用户键盘被卡住):
   **绝不用合成按键去触发热键**。热键事件被 sainput 的钩子吃掉之后,keyup 也得靠同一个钩子
   送达;一旦那个实例被硬杀,keyup 就永久丢失 —— 系统/目标程序认为修饰键一直按着,
   而用户键盘上可能根本没有那个键,只能 `sainput release-keys` 救。
   需要无人值守地触发「按住 → 松开」,用 `SAINPUT_DIAG_AUTOSTART=<ms>`(进程内部触发),
   别用 SendInput 发真按键。本脚本只发窗口消息(WM_CHAR / WM_KEYDOWN/UP),
   但读回用的是 Ctrl+A/Ctrl+C 合成按键 —— 跑之前确认没有别的 sainput 实例在用这些键。
"""
import argparse
import ctypes
import sys
import time

import probe_wechat_inject as P

u32 = ctypes.windll.user32
WM_CHAR, WM_KEYDOWN, WM_KEYUP, VK_BACK = 0x0102, 0x0100, 0x0101, 0x08


def read():
    """读输入框:不按 Esc(别去动输入法的组字状态),Ctrl+A/C + 剪贴板 + End。"""
    P.set_clipboard(P.MARKER)
    P.chord(P.VK_A)
    P.chord(P.VK_C)
    P.send_vk(P.VK_END, True)
    P.send_vk(P.VK_END, False)
    time.sleep(0.25)
    got = P.read_clipboard()
    return None if got == P.MARKER else got


def post(hwnd, s):
    for ch in s:
        u32.PostMessageW(hwnd, WM_CHAR, ord(ch), 0)


def backspace(hwnd, n):
    for _ in range(n):
        u32.PostMessageW(hwnd, WM_KEYDOWN, VK_BACK, 1 | (0x0E << 16))
        u32.PostMessageW(hwnd, WM_KEYUP, VK_BACK, 1 | (0x0E << 16) | (1 << 30) | (1 << 31))
        time.sleep(0.05)
    time.sleep(0.4)


def click_input(hwnd):
    """点进输入框并**用哨兵验证**它真的握着键盘焦点(窗口在前台 ≠ 输入框有焦点)。"""
    class R(ctypes.Structure):
        _fields_ = [("l", ctypes.c_long), ("t", ctypes.c_long),
                    ("r", ctypes.c_long), ("b", ctypes.c_long)]
    rc = R()
    u32.GetWindowRect(hwnd, ctypes.byref(rc))
    width = rc.r - rc.l
    for fx in (0.45, 0.55):
        for dy in (45, 75, 105, 140):
            u32.SetCursorPos(rc.l + int(width * fx), rc.b - dy)
            time.sleep(0.15)
            u32.mouse_event(0x0002, 0, 0, 0, 0)
            u32.mouse_event(0x0004, 0, 0, 0, 0)
            time.sleep(0.35)
            for ch in "ZQ7":
                u32.PostMessageW(hwnd, WM_CHAR, ord(ch), 0)
            time.sleep(0.5)
            if read() == "ZQ7":
                backspace(hwnd, 3)
                return True
    return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--title", default="WeChat", help="目标窗口标题(微信 4.x 是 WeChat)")
    ap.add_argument("--reps", type=int, default=5)
    ap.add_argument("--gaps", default="0,10,20,40", help="退格到下一个字符之间的毫秒数")
    ap.add_argument("--code", default="903721")
    a = ap.parse_args()

    hwnd = P.find_window(a.title)
    print(f"target: {hex(hwnd) if hwnd else None} ({a.title})")
    if not hwnd:
        print("找不到窗口(标题对不上?)")
        return 1
    saved = P.read_clipboard()
    try:
        # 只做 SetForegroundWindow:**不敲 ALT**。敲门那一下也是合成按键,
        # 万一 keyup 丢了就是又一个卡住的修饰键(2026-10-30 的事故教训)。
        u32.SetForegroundWindow(hwnd)
        time.sleep(0.6)
        if u32.GetForegroundWindow() != hwnd:
            print("拿不到前台:手动点一下目标窗口,让光标停在输入框里,再跑本脚本")
            return 1
        time.sleep(0.4)
        if not click_input(hwnd):
            print("输入框没拿到焦点:先把目标窗口的输入框点开、清空,再跑")
            return 1
        base = read()
        if base:
            print("框里原有内容(先清掉):", repr(base))
            backspace(hwnd, len(base))

        gaps = [int(x) for x in a.gaps.split(",") if x.strip()]
        print(f"{'间隔(ms)':>8}  正确/次数   异常样本")
        rows = []
        for gap in gaps:
            bad = []
            for _ in range(a.reps):
                post(hwnd, "Z")
                backspace(hwnd, 1)
                if gap:
                    time.sleep(gap / 1000.0)
                post(hwnd, "9")
                time.sleep(0.7)
                got = read()
                if got != "9":
                    bad.append(got)
                backspace(hwnd, len(got or ""))
                time.sleep(0.2)
            rows.append(bad)
            print(f"{gap:>8}  {a.reps - len(bad)}/{a.reps}      {bad if bad else ''}")

        # 真实现场:ITN 把「九」改写成「9」时会退一次格,紧接着打整串数字
        real_bad = []
        for _ in range(a.reps):
            post(hwnd, "我的验证码是九")
            time.sleep(0.15)
            backspace(hwnd, 1)
            post(hwnd, a.code)
            time.sleep(0.7)
            got = read()
            if got != "我的验证码是" + a.code:
                real_bad.append(got)
            backspace(hwnd, len(got or ""))
            time.sleep(0.2)
        print(f"\n真实现场(是九 → 退格 → {a.code}): "
              f"{a.reps - len(real_bad)}/{a.reps} 正确 {real_bad if real_bad else ''}")

        if gaps and gaps[0] == 0 and rows[0]:
            print("\n✓ 复现了:同一批消息里退格后面的字符会被吞(所以要留间隔)")
        else:
            print("\n这个窗口没有复现(或者第一个间隔不是 0)—— 换窗口/换输入法再量")
    finally:
        if saved is not None:
            P.set_clipboard(saved)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
