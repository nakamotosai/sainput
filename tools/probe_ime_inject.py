# -*- coding: utf-8 -*-
"""上屏方式探针:键盘注入 vs PostMessage(WM_CHAR),以及「输入法把字母收走」的现场。

为什么需要这个探针(2026-10-28 实测):目标窗口所在线程挂着中文/日文输入法时,
`SendInput` 注入的**字母**会被输入法当成它自己的输入(落进组字串、弹出候选面板),
程序一个字都收不到;同一窗口注入**汉字**却照常落地 —— 所以只有英文这一路坏掉。
`WM_CHAR` 走的是窗口消息,不经过键盘那条链,输入法看不到它。

这个探针做两件事:
  1. **隐藏窗口自检(默认,无副作用)**:建一个从不显示、从不获焦点的经典 EDIT 控件,
     PostMessage(WM_CHAR) 打一串 ASCII,读回来必须一字不差;再用 WM_CHAR(0x08)
     退 3 个字符。这一条验证 `inject.rs` 里 WM_CHAR 那条路依赖的 Win32 语义。
  2. **现场对比(要你自己给窗口句柄,--hwnd 0x…)**:对同一个窗口先用 SendInput
     打一串字母,再用 WM_CHAR 打一遍,分别读回内容 —— 一眼看出这个窗口吃哪一种。
     读不到内容的窗口(自绘/终端)只能看它自己出不出来字。

自绘输入框(微信 / Qt 窗口)那一路的现场取证在 `tools/probe_wechat_inject.py` ——
那边多了「读回自绘输入框(Ctrl+A/Ctrl+C + 剪贴板)」和「三条通道打同一串字」两件事。

跑法:
  python tools/probe_ime_inject.py                       # 隐藏窗口自检
  python tools/probe_ime_inject.py --hwnd 0x000000 --selftest-input "hello TOKEN"
"""

import ctypes
import ctypes.wintypes as w
import sys
import threading
import time

u32 = ctypes.windll.user32
k32 = ctypes.windll.kernel32
WM_CHAR, WM_GETTEXT, WM_GETTEXTLENGTH, WM_SETTEXT = 0x0102, 0x000D, 0x000E, 0x000C
WS_CHILD, WS_BORDER, ES_MULTILINE = 0x40000000, 0x00800000, 0x0004
LRESULT = ctypes.c_ssize_t


def _wndproc(h, m, wp, lp):
    u32.DefWindowProcW.restype = LRESULT
    u32.DefWindowProcW.argtypes = [w.HWND, ctypes.c_uint, w.WPARAM, w.LPARAM]
    return u32.DefWindowProcW(h, m, wp, lp)


WNDPROC = ctypes.WINFUNCTYPE(LRESULT, w.HWND, ctypes.c_uint, w.WPARAM, w.LPARAM)
CB = WNDPROC(_wndproc)


class WNDCLASSEXW(ctypes.Structure):
    _fields_ = [("cbSize", ctypes.c_uint), ("style", ctypes.c_uint), ("lpfnWndProc", WNDPROC),
                ("cbClsExtra", ctypes.c_int), ("cbWndExtra", ctypes.c_int),
                ("hInstance", w.HINSTANCE), ("hIcon", w.HICON), ("hCursor", w.HANDLE),
                ("hbrBackground", w.HBRUSH), ("lpszMenuName", w.LPCWSTR),
                ("lpszClassName", w.LPCWSTR), ("hIconSm", w.HICON)]


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


def send_unicode(ch):
    """sainput 默认那条路:SendInput + KEYEVENTF_UNICODE(字母会被输入法收走)。"""
    ev = []
    raw = ch.encode("utf-16-le")
    for i in range(0, len(raw), 2):
        unit = int.from_bytes(raw[i:i + 2], "little")
        for up in (False, True):
            x = _INPUT()
            x.type = 1
            x.u.ki = _KEYBDINPUT(0, unit, 4 | (2 if up else 0), 0, None)
            ev.append(x)
    arr = (_INPUT * len(ev))(*ev)
    u32.SendInput(len(ev), arr, ctypes.sizeof(_INPUT))


def cls_of(h):
    b = ctypes.create_unicode_buffer(256)
    u32.GetClassNameW(h, b, 256)
    return b.value


def read(h):
    """控件里的文本;这类窗口答不上 WM_GETTEXT 时返回 None(别把「读不到」当成「没打进去」)。"""
    n = u32.SendMessageW(h, WM_GETTEXTLENGTH, 0, 0)
    if n < 0 or n > 1_000_000:
        return None
    b = ctypes.create_unicode_buffer(n + 1)
    u32.SendMessageW(h, WM_GETTEXT, n + 1, ctypes.byref(b))
    return b.value


def hidden_edit():
    """A hidden EDIT control created AND pumped by one worker thread (never shown/focused)."""
    box = {}

    def worker():
        hinst = k32.GetModuleHandleW(None)
        wc = WNDCLASSEXW()
        wc.cbSize = ctypes.sizeof(WNDCLASSEXW)
        wc.lpfnWndProc = CB
        wc.hInstance = hinst
        wc.lpszClassName = "SainputHiddenProbe"
        u32.RegisterClassExW(ctypes.byref(wc))
        top = u32.CreateWindowExW(0, "SainputHiddenProbe", "hidden", 0, 0, 0, 400, 200, None, None, hinst, None)
        edit = u32.CreateWindowExW(0, "EDIT", "", WS_CHILD | WS_BORDER | ES_MULTILINE, 0, 0, 380, 180, top, None, hinst, None)
        box["edit"] = edit
        box["ready"] = True
        msg = w.MSG()
        while u32.GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
            u32.TranslateMessage(ctypes.byref(msg))
            u32.DispatchMessageW(ctypes.byref(msg))

    threading.Thread(target=worker, daemon=True).start()
    for _ in range(40):
        if box.get("ready"):
            break
        time.sleep(0.1)
    time.sleep(0.3)
    return box["edit"]


def notepad_edit():
    found = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, w.HWND, w.LPARAM)
    def cb(h, l):
        n = u32.GetWindowTextLengthW(h)
        if n:
            b = ctypes.create_unicode_buffer(n + 1)
            u32.GetWindowTextW(h, b, n + 1)
            if "Notepad" in b.value or "记事本" in b.value:
                found.append(h)
        return True

    u32.EnumWindows(cb, 0)
    if not found:
        return None
    kids = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, w.HWND, w.LPARAM)
    def cb2(c, l):
        cl = cls_of(c)
        if "RichEdit" in cl or cl == "Edit":
            kids.append(c)
        return True

    u32.EnumChildWindows(found[0], cb2, 0)
    return kids[0] if kids else None


def live(hwnd, text):
    """同一个窗口上,键盘注入 vs WM_CHAR 的现场对比。"""
    print(f"== live target {hex(hwnd)}  class={cls_of(hwnd)}")
    u32.SendMessageW(hwnd, WM_SETTEXT, 0, ctypes.c_wchar_p(""))
    for ch in text:
        send_unicode(ch)
    time.sleep(0.6)
    a = read(hwnd)
    print("   SendInput(KEYEVENTF_UNICODE) ->", repr(a) if a is not None else "(读不到内容)")
    u32.SendMessageW(hwnd, WM_SETTEXT, 0, ctypes.c_wchar_p(""))
    for ch in text:
        u32.PostMessageW(hwnd, WM_CHAR, ord(ch), 0)
    time.sleep(0.6)
    b = read(hwnd)
    print("   PostMessage(WM_CHAR)        ->", repr(b) if b is not None else "(读不到内容)")


def main():
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--hwnd", default=None, help="现场对比用的目标窗口句柄(0x…)")
    ap.add_argument("--text", default="hello TOKEN 123")
    a = ap.parse_args()

    print("== A) hidden EDIT control (never shown, never focused)")
    e = hidden_edit()
    print("   class:", cls_of(e), "visible:", bool(u32.IsWindowVisible(e)))
    u32.SendMessageW(e, WM_SETTEXT, 0, ctypes.c_wchar_p(""))
    for ch in a.text:
        u32.PostMessageW(e, WM_CHAR, ord(ch), 0)
    time.sleep(0.4)
    got = read(e)
    ok1 = got == a.text
    print(f"   sent {a.text!r} -> got {got!r}", "EXACT" if ok1 else "LOSSY")
    for _ in range(3):
        u32.PostMessageW(e, WM_CHAR, 0x08, 0)
    time.sleep(0.3)
    got2 = read(e)
    ok2 = got2 == a.text[:-3]
    print(f"   after 3x WM_CHAR(0x08) -> {got2!r}", "EXACT" if ok2 else "LOSSY")

    if a.hwnd:
        live(int(a.hwnd, 16), a.text)
    print()
    print("=== %s ===" % ("WM_CHAR 路径可用" if ok1 and ok2 else "WM_CHAR 路径有问题"))
    return 0 if ok1 and ok2 else 1


if __name__ == "__main__":
    raise SystemExit(main())
