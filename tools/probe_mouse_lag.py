# -*- coding: utf-8 -*-
"""量「鼠标事件从产生到钩子看到它」的延迟。

为什么需要它：低级鼠标钩子（WH_MOUSE_LL）会让**每一个**鼠标事件等待钩子过程返回。
钩子回调是在钩子所属线程的消息循环里派发的，所以那条循环里**任何睡眠都会变成
鼠标移动的延迟**。实测：循环里 `sleep(40ms)` → 延迟 p50 = 31ms（1000Hz 鼠标掉到
~30Hz，手感肉眼可见地卡）；换成 `MsgWaitForMultipleObjectsEx` → p50 = 0ms。

判据：p50 < 5ms。

注意实现细节：合成鼠标事件必须**在消息泵运行期间**由另一个线程发出，
否则事件发完了才开始 pump，一个都收不到（会误报 no-events）。
"""
import ctypes
import ctypes.wintypes as w
import sys
import threading
import time

u32 = ctypes.WinDLL("user32", use_last_error=True)
k32 = ctypes.WinDLL("kernel32")

WH_MOUSE_LL = 14
WM_MOUSEMOVE = 0x0200
QS_ALLINPUT = 0x04FF
MWMO_INPUTAVAILABLE = 0x0004
PM_REMOVE = 0x0001


class MSLL(ctypes.Structure):
    _fields_ = [
        ("pt", w.POINT),
        ("mouseData", w.DWORD),
        ("flags", w.DWORD),
        ("time", w.DWORD),
        ("extra", ctypes.c_void_p),
    ]


HOOKPROC = ctypes.WINFUNCTYPE(ctypes.c_long, ctypes.c_int, w.WPARAM, ctypes.c_void_p)
seen = []


def proc(code, wp, lp):
    if code >= 0 and wp == WM_MOUSEMOVE:
        t = ctypes.cast(lp, ctypes.POINTER(MSLL)).contents
        seen.append((k32.GetTickCount(), t.time))
    return u32.CallNextHookEx(0, code, wp, ctypes.c_void_p(lp))


class MI(ctypes.Structure):
    _fields_ = [
        ("dx", ctypes.c_long),
        ("dy", ctypes.c_long),
        ("mouseData", w.DWORD),
        ("dwFlags", w.DWORD),
        ("time", w.DWORD),
        ("extra", ctypes.c_void_p),
    ]


class IU(ctypes.Union):
    _fields_ = [("mi", MI)]


class INPUT(ctypes.Structure):
    _fields_ = [("type", w.DWORD), ("u", IU)]


def synth_loop(stop):
    while not stop[0]:
        i = INPUT()
        i.type = 0
        i.u.mi.dx = 1
        i.u.mi.dwFlags = 0x0001  # MOUSEEVENTF_MOVE
        u32.SendInput(1, ctypes.byref(i), ctypes.sizeof(INPUT))
        time.sleep(0.005)


def main():
    cb = HOOKPROC(proc)
    h = u32.SetWindowsHookExW(WH_MOUSE_LL, cb, None, 0)
    if not h:
        print("hook-failed")
        return 1

    stop = [False]
    th = threading.Thread(target=synth_loop, args=(stop,))
    th.start()

    # 用和 sainput 相同的循环写法（MsgWait 版），保证测的是"当前实现的样子"
    msg = w.MSG()
    end = time.time() + 2.0
    while time.time() < end:
        while u32.PeekMessageW(ctypes.byref(msg), 0, 0, 0, PM_REMOVE):
            u32.TranslateMessage(ctypes.byref(msg))
            u32.DispatchMessageW(ctypes.byref(msg))
        u32.MsgWaitForMultipleObjectsEx(0, None, 20, QS_ALLINPUT, MWMO_INPUTAVAILABLE)

    stop[0] = True
    th.join()
    u32.UnhookWindowsHookEx(h)

    d = sorted(now - ev for now, ev in seen)
    if not d:
        print("no-events")
        return 1
    p50 = d[len(d) // 2]
    p90 = d[int(len(d) * 0.9)]
    print("n={} p50={} p90={} max={}".format(len(d), p50, p90, d[-1]))
    return 0 if p50 < 5 else 2


if __name__ == "__main__":
    sys.exit(main())
