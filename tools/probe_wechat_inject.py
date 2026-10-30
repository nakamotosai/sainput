# -*- coding: utf-8 -*-
"""微信(以及任何自绘输入框)上屏取证:三条通道打同一串字,读回输入框比一比。

为什么需要它(2026-10-30):目标窗口挂着中文输入法时,`SendInput` 注入的**字母**
会被输入法当成自己的输入收走 —— 落进组字串、弹出候选面板,程序一个字都收不到。
微信 4.x(`Weixin.exe`,Qt `Qt51514QWindowIcon`)实测就是这个现场:
`helloTOKENabc` 一个字符都没进输入框,而 `PostMessage(WM_CHAR)` 逐字落地。
`inject.rs` 的 `auto` 判据(`class_needs_message_channel`)靠这把尺子量出来。

读回方式:自绘输入框答不上 `WM_GETTEXT`,所以走 **Ctrl+A / Ctrl+C + 剪贴板**。
⚠️ 两个坑(都踩过):
  1. 读之前先按 `Esc` 收掉输入法的组字串 —— 组字态下 Ctrl+A 会被输入法截走,
     把字母打进输入框(实测多出来一个 `A`);
  2. 读完必须按 `End` 把选择收起来,否则下一次退格会**把整段选中内容一次删掉**。
剪贴板用完还原(只还原文本格式)。

跑法(先让目标窗口在前台;`focus` 会自己抢):
  python tools/probe_wechat_inject.py read                 # 读输入框
  python tools/probe_wechat_inject.py clear                # 清空
  python tools/probe_wechat_inject.py keys  "helloTOKENabc"  # SendInput(默认路)
  python tools/probe_wechat_inject.py wmchar "helloTOKENabc" # WM_CHAR(消息通道)
  python tools/probe_wechat_inject.py bs 3                 # 3 次退格(WM_KEYDOWN/UP VK_BACK)
  python tools/probe_wechat_inject.py drive "helloTOKEN混排OK" pebrel
        # 走**生产代码**(examples/injectprobe)打完再读回,报 OK/MISMATCH —— 这条才算验收
"""

import ctypes
import ctypes.wintypes as w
import subprocess
import sys
import time

u32 = ctypes.windll.user32
k32 = ctypes.windll.kernel32
imm32 = ctypes.windll.imm32

WM_CHAR, WM_KEYDOWN, WM_KEYUP = 0x0102, 0x0100, 0x0101
VK_BACK, VK_CONTROL, VK_A, VK_C, VK_END, VK_DELETE, VK_ESCAPE = 0x08, 0x11, 0x41, 0x43, 0x23, 0x2E, 0x1B
INPUT_KEYBOARD, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE = 1, 0x0002, 0x0004
CF_UNICODETEXT = 13
DEFAULT_TITLE = "WeChat"  # 微信 4.x 主窗口标题;旧版是 "Weixin"

# ctypes 默认把返回值当 int(32 位),指针会被截断 —— 这几个必须显式声明
k32.GlobalAlloc.restype = ctypes.c_void_p
k32.GlobalAlloc.argtypes = [ctypes.c_uint, ctypes.c_size_t]
k32.GlobalLock.restype = ctypes.c_void_p
k32.GlobalLock.argtypes = [ctypes.c_void_p]
k32.GlobalUnlock.argtypes = [ctypes.c_void_p]
u32.OpenClipboard.argtypes = [ctypes.c_void_p]
u32.GetClipboardData.restype = ctypes.c_void_p
u32.SetClipboardData.restype = ctypes.c_void_p
u32.SetClipboardData.argtypes = [ctypes.c_uint, ctypes.c_void_p]
u32.GetForegroundWindow.restype = ctypes.c_void_p
u32.GetWindowThreadProcessId.restype = ctypes.c_uint
u32.GetKeyboardLayout.restype = ctypes.c_void_p


class _KEYBDINPUT(ctypes.Structure):
    _fields_ = [("wVk", w.WORD), ("wScan", w.WORD), ("dwFlags", w.DWORD),
                ("time", w.DWORD), ("dwExtraInfo", ctypes.c_void_p)]


class _MOUSEINPUT(ctypes.Structure):
    _fields_ = [("dx", w.LONG), ("dy", w.LONG), ("mouseData", w.DWORD),
                ("dwFlags", w.DWORD), ("time", w.DWORD), ("dwExtraInfo", ctypes.c_void_p)]


class _UNION(ctypes.Union):
    _fields_ = [("ki", _KEYBDINPUT), ("mi", _MOUSEINPUT), ("pad", ctypes.c_byte * 32)]


class _INPUT(ctypes.Structure):
    _fields_ = [("type", w.DWORD), ("u", _UNION)]


def send_unicode(text):
    """sainput 默认那条路:SendInput + KEYEVENTF_UNICODE(字母会被输入法收走)。"""
    ev = []
    for ch in text:
        raw = ch.encode("utf-16-le")
        for i in range(0, len(raw), 2):
            unit = int.from_bytes(raw[i:i + 2], "little")
            for up in (False, True):
                x = _INPUT()
                x.type = INPUT_KEYBOARD
                x.u.ki = _KEYBDINPUT(0, unit, KEYEVENTF_UNICODE | (KEYEVENTF_KEYUP if up else 0), 0, None)
                ev.append(x)
    arr = (_INPUT * len(ev))(*ev)
    u32.SendInput(len(ev), arr, ctypes.sizeof(_INPUT))


def send_vk(vk, down=True):
    x = _INPUT()
    x.type = INPUT_KEYBOARD
    x.u.ki = _KEYBDINPUT(vk, 0, 0 if down else KEYEVENTF_KEYUP, 0, None)
    u32.SendInput(1, (_INPUT * 1)(x), ctypes.sizeof(_INPUT))


def chord(vk):
    send_vk(VK_CONTROL, True)
    send_vk(vk, True)
    send_vk(vk, False)
    send_vk(VK_CONTROL, False)
    time.sleep(0.2)


def cls_of(h):
    b = ctypes.create_unicode_buffer(256)
    u32.GetClassNameW(h, b, 256)
    return b.value


def title_of(h):
    n = u32.GetWindowTextLengthW(h)
    b = ctypes.create_unicode_buffer(n + 1)
    u32.GetWindowTextW(h, b, n + 1)
    return b.value


def pid_of(h):
    p = w.DWORD()
    u32.GetWindowThreadProcessId(h, ctypes.byref(p))
    return p.value


def find_window(title=DEFAULT_TITLE):
    out = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, w.HWND, w.LPARAM)
    def cb(h, l):
        if u32.IsWindowVisible(h) and title_of(h) == title:
            out.append(h)
            return False
        return True

    u32.EnumWindows(cb, 0)
    return out[0] if out else None


def focus(hwnd):
    """抢前台(SetForegroundWindow 常被系统拒绝,所以带 ALT 敲门 + 验证)。"""
    for _ in range(10):
        if u32.GetForegroundWindow() == hwnd:
            return True
        u32.ShowWindow(hwnd, 9)  # SW_RESTORE
        send_vk(0x12, True)      # ALT
        send_vk(0x12, False)
        u32.SetForegroundWindow(hwnd)
        time.sleep(0.35)
    return u32.GetForegroundWindow() == hwnd


def read_clipboard():
    for _ in range(30):
        if u32.OpenClipboard(None):
            break
        time.sleep(0.1)
    else:
        return None
    try:
        h = u32.GetClipboardData(CF_UNICODETEXT)
        if not h:
            return None
        p = k32.GlobalLock(h)
        try:
            return ctypes.c_wchar_p(p).value
        finally:
            k32.GlobalUnlock(h)
    finally:
        u32.CloseClipboard()


def set_clipboard(text):
    u32.OpenClipboard(None)
    try:
        u32.EmptyClipboard()
        n = (len(text) + 1) * 2
        h = k32.GlobalAlloc(0x0042, n)
        p = k32.GlobalLock(h)
        ctypes.memmove(p, ctypes.create_unicode_buffer(text), n)
        k32.GlobalUnlock(h)
        u32.SetClipboardData(CF_UNICODETEXT, h)
    finally:
        u32.CloseClipboard()


MARKER = "<probe-marker>"


def read_input():
    """读输入框(必须已经在前台):Esc 收组字 → Ctrl+A → Ctrl+C → 剪贴板 → End 收起选择。"""
    set_clipboard(MARKER)
    send_vk(VK_ESCAPE, True)
    send_vk(VK_ESCAPE, False)
    time.sleep(0.2)
    chord(VK_A)
    chord(VK_C)
    send_vk(VK_END, True)
    send_vk(VK_END, False)
    time.sleep(0.3)
    got = read_clipboard()
    return None if got == MARKER else got


def clear_input():
    chord(VK_A)
    send_vk(VK_DELETE, True)
    send_vk(VK_DELETE, False)
    time.sleep(0.3)


def state(hwnd):
    fg = u32.GetForegroundWindow()
    tid = u32.GetWindowThreadProcessId(hwnd, None)
    hkl = u32.GetKeyboardLayout(tid)
    print(f"target={hex(hwnd)} class={cls_of(hwnd)!r} title={title_of(hwnd)!r}")
    print(f"  foreground={hex(fg)} ({'hit' if fg == hwnd else 'MISS'}) "
          f"hkl={hex(hkl or 0)} ImmIsIME={imm32.ImmIsIME(hkl)}")


def drive(text, policy):
    """走生产代码(examples/injectprobe.exe):决定模式 → 打出去 → 读回比一比。"""
    import os
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    exe = os.path.join(root, r"target\x86_64-pc-windows-gnu\release\examples\injectprobe.exe")
    if not os.path.exists(exe):
        exe = os.path.join(root, r"target\release\examples\injectprobe.exe")
    if not os.path.exists(exe):
        print("先构建探针:cargo build --release -p sainput-core --example injectprobe")
        return 2
    out = subprocess.run([exe, text, policy, DEFAULT_TITLE], capture_output=True, text=True,
                         encoding="utf-8", errors="replace")
    print(out.stdout.strip())
    hwnd = find_window()
    got = read_input() if hwnd and focus(hwnd) else None
    expect = " " + text  # 探针按主循环那样补前置空格
    print(f"输入框读回 = {got!r}")
    ok = got == expect
    print("RESULT:", "OK" if ok else f"MISMATCH expect={expect!r}")
    return 0 if ok else 1


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    cmd = sys.argv[1]
    hwnd = find_window()
    if not hwnd:
        print(f"找不到可见的 {DEFAULT_TITLE!r} 窗口")
        return 1
    if cmd == "state":
        state(hwnd)
        return 0
    if cmd == "drive":
        return drive(sys.argv[2] if len(sys.argv) > 2 else "helloTOKEN混排OK",
                     sys.argv[3] if len(sys.argv) > 3 else "pebrel")

    saved = read_clipboard()
    try:
        if not focus(hwnd):
            print("拿不到前台,放弃")
            return 1
        time.sleep(0.4)
        if cmd == "read":
            print("input ->", repr(read_input()))
        elif cmd == "clear":
            clear_input()
            print("cleared ->", repr(read_input()))
        elif cmd == "keys":
            send_unicode(sys.argv[2])
            time.sleep(0.8)
            print("sent(SendInput/UNICODE) ->", repr(read_input()))
        elif cmd == "wmchar":
            for ch in sys.argv[2]:
                u32.PostMessageW(hwnd, WM_CHAR, ord(ch), 0)
            time.sleep(0.8)
            print("sent(WM_CHAR) ->", repr(read_input()))
        elif cmd == "bs":
            for _ in range(int(sys.argv[2])):
                u32.PostMessageW(hwnd, WM_KEYDOWN, VK_BACK, 0)
                u32.PostMessageW(hwnd, WM_KEYUP, VK_BACK, 0)
                time.sleep(0.15)
            time.sleep(0.4)
            print(f"sent {sys.argv[2]}x WM_KEYDOWN/UP VK_BACK ->", repr(read_input()))
        else:
            print(__doc__)
            return 2
    finally:
        if saved is not None:
            set_clipboard(saved)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
