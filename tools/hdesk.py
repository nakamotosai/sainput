# -*- coding: utf-8 -*-
"""在私有 Win32 桌面上跑东西:用户桌面上看不见、也抢不到焦点。

为什么需要它(2026-10-31):上屏通道(M3/TSF)要反复起宿主窗口做验证,而用户
日常就在同一个会话里工作 —— 任何弹到桌面、抢前台、合成按键的验证都会打断他。
私有桌面把窗口关在另一张桌上:console 桌面**枚举不到**它的窗口,`SetFocus`/
`SetForegroundWindow` 也只作用于那张桌,碰不到用户。

用法:
    python tools/hdesk.py run    --exe <exe> [--args "…"] [--desktop NAME] [--timeout N]
    python tools/hdesk.py serve  --exe <exe> [--args "…"] [--desktop NAME]   # 常驻,任它跑
    python tools/hdesk.py prove  --exe <exe> [--args "…"] [--dwell MS] [--min-windows N]
    python tools/hdesk.py windows --pid N [--desktop NAME]

`prove` 是给测试用的自证:子进程的窗口在**私有桌面**上数得到(>0),在 **console 桌面**上
数不到(必须是 0)—— 「确实建了窗口」与「用户看不见」两个结论一次拿到。

三条已踩过的坑(别改回去):
  * **运行时输出一律 ASCII**。本机控制台是 CP936,中文 print 出去会被下游按错误编码
    解成乱码(2026-10-31 实测)。注释与文档字符串照旧中文。
  * `dwCreationFlags` 必须为 0。带 `CREATE_NO_WINDOW(0x8)` 时,私有桌面上的
    `powershell.exe` 会在 0.1 秒内静默退出(退出码 0、什么都不执行)。
  * `lpApplicationName` 必须给 exe 全路径,传空会 `ERROR_PATH_NOT_FOUND(3)`。
  * 启动器进程必须活着 —— 它持有桌面句柄,一退出桌面即销毁、子进程被带走。
"""
import argparse
import ctypes
import ctypes.wintypes as wt
import os
import sys
import time

u32 = ctypes.WinDLL("user32", use_last_error=True)
k32 = ctypes.WinDLL("kernel32", use_last_error=True)

GENERIC_ALL = 0x10000000
INFINITE = 0xFFFFFFFF
DEFAULT_DESKTOP = "SAI-HDesk"


class STARTUPINFOW(ctypes.Structure):
    _fields_ = [
        ("cb", wt.DWORD), ("lpReserved", wt.LPWSTR), ("lpDesktop", wt.LPWSTR), ("lpTitle", wt.LPWSTR),
        ("dwX", wt.DWORD), ("dwY", wt.DWORD), ("dwXSize", wt.DWORD), ("dwYSize", wt.DWORD),
        ("dwXCountChars", wt.DWORD), ("dwYCountChars", wt.DWORD), ("dwFillAttribute", wt.DWORD),
        ("dwFlags", wt.DWORD), ("wShowWindow", wt.WORD), ("cbReserved2", wt.WORD),
        ("lpReserved2", ctypes.c_void_p),
        ("hStdInput", wt.HANDLE), ("hStdOutput", wt.HANDLE), ("hStdError", wt.HANDLE),
    ]


class PROCESS_INFORMATION(ctypes.Structure):
    _fields_ = [("hProcess", wt.HANDLE), ("hThread", wt.HANDLE),
                ("dwProcessId", wt.DWORD), ("dwThreadId", wt.DWORD)]


WNDENUMPROC = ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)

u32.CreateDesktopW.restype = wt.HANDLE
u32.CreateDesktopW.argtypes = [wt.LPCWSTR, wt.LPCWSTR, ctypes.c_void_p, wt.DWORD, wt.DWORD, ctypes.c_void_p]
u32.EnumDesktopWindows.restype = wt.BOOL
u32.EnumDesktopWindows.argtypes = [wt.HANDLE, WNDENUMPROC, wt.LPARAM]
u32.EnumWindows.restype = wt.BOOL
u32.EnumWindows.argtypes = [WNDENUMPROC, wt.LPARAM]
k32.CreateProcessW.restype = wt.BOOL
k32.CreateProcessW.argtypes = [wt.LPCWSTR, wt.LPWSTR, ctypes.c_void_p, ctypes.c_void_p, wt.BOOL,
                               wt.DWORD, ctypes.c_void_p, wt.LPCWSTR,
                               ctypes.POINTER(STARTUPINFOW), ctypes.POINTER(PROCESS_INFORMATION)]


def open_desktop(name):
    h = u32.CreateDesktopW(name, None, None, 0, GENERIC_ALL, None)
    if not h:
        raise OSError(f"CreateDesktop({name}) failed: err={ctypes.get_last_error()}")
    return h


def launch(exe, args, desktop_name, cwd=None):
    """在 `desktop_name` 上起进程。返回 (hProcess, hThread, pid)。"""
    if not os.path.isabs(exe):
        raise ValueError("lpApplicationName must be an absolute path")
    si = STARTUPINFOW()
    si.cb = ctypes.sizeof(si)
    si.lpDesktop = desktop_name
    pi = PROCESS_INFORMATION()
    cmdline = f'"{exe}"' + (f" {args}" if args else "")
    buf = ctypes.create_unicode_buffer(cmdline)
    ok = k32.CreateProcessW(exe, buf, None, None, False, 0, None, cwd, ctypes.byref(si), ctypes.byref(pi))
    if not ok:
        raise OSError(f"CreateProcess failed: err={ctypes.get_last_error()}")
    return pi.hProcess, pi.hThread, pi.dwProcessId


def wait_exit(hprocess, timeout_ms=None):
    """等子进程退出。返回退出码;超时返回 None(进程还在跑)。"""
    r = k32.WaitForSingleObject(hprocess, INFINITE if timeout_ms is None else timeout_ms)
    if r != 0:
        return None
    code = wt.DWORD()
    k32.GetExitCodeProcess(hprocess, ctypes.byref(code))
    return code.value


def close(h):
    if h:
        k32.CloseHandle(h)


def terminate(pid):
    PROCESS_TERMINATE = 0x0001
    h = k32.OpenProcess(PROCESS_TERMINATE, False, pid)
    if h:
        k32.TerminateProcess(h, 1)
        close(h)
        return True
    return False


def enum_windows(pid, hdesk=None):
    """枚举顶层窗口。hdesk=None 数 console 桌面;给句柄则数那张私有桌面。"""
    out = []

    def cb(hwnd, _):
        wpid = wt.DWORD()
        u32.GetWindowThreadProcessId(hwnd, ctypes.byref(wpid))
        if wpid.value == pid:
            cls = ctypes.create_unicode_buffer(256)
            title = ctypes.create_unicode_buffer(256)
            u32.GetClassNameW(hwnd, cls, 256)
            u32.GetWindowTextW(hwnd, title, 256)
            out.append((hwnd, cls.value, title.value, bool(u32.IsWindowVisible(hwnd))))
        return True

    fn = WNDENUMPROC(cb)
    if hdesk:
        u32.EnumDesktopWindows(hdesk, fn, 0)
    else:
        u32.EnumWindows(fn, 0)
    return out


WM_GETTEXT = 0x000D
WM_GETTEXTLENGTH = 0x000E

u32.FindWindowExW.restype = wt.HWND
u32.FindWindowExW.argtypes = [wt.HWND, wt.HWND, wt.LPCWSTR, wt.LPCWSTR]
u32.SendMessageW.restype = ctypes.c_ssize_t
u32.SendMessageW.argtypes = [wt.HWND, wt.UINT, ctypes.c_size_t, ctypes.c_ssize_t]


def find_edit(pid, hdesk=None):
    """找该 pid 的顶层窗口里的 EDIT 控件(宿主壳的读回口)。"""
    for hwnd, _cls, _title, _vis in enum_windows(pid, hdesk):
        edit = u32.FindWindowExW(hwnd, None, "Edit", None)
        if edit:
            return edit
    return None


def read_edit(edit):
    """跨进程读 EDIT 内容。WM_GETTEXT 由 user32 负责搬运字符串(实测跨桌面可用)。"""
    n = u32.SendMessageW(edit, WM_GETTEXTLENGTH, 0, 0)
    buf = ctypes.create_unicode_buffer(int(n) + 2)
    u32.SendMessageW(edit, WM_GETTEXT, len(buf), ctypes.cast(buf, ctypes.c_void_p).value)
    return buf.value



def _print_win(prefix, ws):
    for hwnd, cls, title, vis in ws:
        cls = cls.encode("ascii", "replace").decode("ascii")
        title = title.encode("ascii", "replace").decode("ascii")
        print(f"    {prefix} hwnd={hwnd:#x} class={cls!r} visible={vis} title={title!r}")


def cmd_run(a):
    hdesk = open_desktop(a.desktop)
    hp, ht, pid = launch(a.exe, a.args, a.desktop, a.cwd)
    print(f"[hdesk] desktop={a.desktop} pid={pid}", flush=True)
    try:
        code = wait_exit(hp, int(a.timeout * 1000) if a.timeout else None)
        if code is None:
            print(f"[hdesk] timeout {a.timeout}s, killing pid={pid}", flush=True)
            terminate(pid)
            return 124
        print(f"[hdesk] exit_code={code}", flush=True)
        return code
    finally:
        close(ht); close(hp); close(hdesk)


def cmd_serve(a):
    hdesk = open_desktop(a.desktop)
    hp, ht, pid = launch(a.exe, a.args, a.desktop, a.cwd)
    print(f"[hdesk] desktop={a.desktop} pid={pid}", flush=True)
    code = wait_exit(hp)
    print(f"[hdesk] exit_code={code}", flush=True)
    close(ht); close(hp); close(hdesk)
    return code


def cmd_windows(a):
    hdesk = open_desktop(a.desktop) if a.desktop else None
    ws = enum_windows(a.pid, hdesk)
    where = a.desktop if a.desktop else "console"
    print(f"[hdesk] pid={a.pid} top-level windows on {where}: {len(ws)}")
    _print_win("", ws)
    if hdesk:
        close(hdesk)
    return 0


def cmd_readtext(a):
    """读回宿主壳 EDIT 里的内容 —— TSF 组合态断言的唯一入口。"""
    hdesk = open_desktop(a.desktop) if a.desktop else None
    try:
        edit = find_edit(a.pid, hdesk)
        if not edit:
            print(f"[hdesk] pid={a.pid}: no EDIT control found")
            return 2
        text = read_edit(edit)
        print(f"[hdesk] edit={edit:#x} text={text!r}")
        return 0
    finally:
        if hdesk:
            close(hdesk)


def cmd_prove(a):
    """自证:窗口确实建了(私有桌面数得到),但用户看不见(console 桌面数不到)。"""
    if a.desktop == "Default":
        print("[hdesk] prove refuses the Default desktop -- that is the thing we avoid")
        return 2
    hdesk = open_desktop(a.desktop)
    try:
        hp, ht, pid = launch(a.exe, a.args, a.desktop, a.cwd)
        print(f"[hdesk] desktop={a.desktop} pid={pid}", flush=True)
        deadline = time.time() + a.dwell / 1000.0
        mine, theirs = [], []
        while time.time() < deadline:
            time.sleep(0.2)
            theirs = enum_windows(pid, None)      # console desktop: must be 0
            mine = enum_windows(pid, hdesk)       # private desktop: must be >= min_windows
            if len(mine) >= a.min_windows and len(theirs) == 0:
                break
        print(f"[hdesk] private_windows={len(mine)}  console_windows={len(theirs)}", flush=True)
        _print_win("private:", mine)
        _print_win("!!CONSOLE:", theirs)
        text_ok, got = True, None
        if a.expect_text is not None:
            edit = find_edit(pid, hdesk)
            if not edit:
                print("[hdesk] no EDIT control found on that pid", flush=True)
                text_ok, got = False, None
            else:
                while time.time() < deadline:
                    got = read_edit(edit)
                    if got == a.expect_text:
                        break
                    time.sleep(0.2)
                text_ok = (got == a.expect_text)
                shown = got.encode("unicode_escape").decode("ascii")
                want = a.expect_text.encode("unicode_escape").decode("ascii")
                print(f"[hdesk] edit_text={shown}  expect={want}  ({'match' if text_ok else 'MISMATCH'})",
                      flush=True)
        code = wait_exit(hp, 3000)
        ok = len(theirs) == 0 and len(mine) >= a.min_windows and text_ok
        if code is None:
            terminate(pid)
        print(f"[hdesk] child exit_code={code}", flush=True)
        print("[hdesk] " + ("PASS -- windows live on the private desktop, console sees 0" if ok
                            else "FAIL -- isolation did not hold, see the list above"), flush=True)
        return 0 if ok else 2
    finally:
        close(hdesk)


def main():
    p = argparse.ArgumentParser(description="run things on a private Win32 desktop (invisible to the user)")
    sub = p.add_subparsers(dest="cmd", required=True)

    def common(sp):
        sp.add_argument("--exe", required=True)
        sp.add_argument("--args", default="")
        sp.add_argument("--cwd", default=None)
        sp.add_argument("--desktop", default=DEFAULT_DESKTOP)

    sp = sub.add_parser("run", help="launch on a private desktop and wait for exit")
    common(sp)
    sp.add_argument("--timeout", type=float, default=0)
    sp.set_defaults(fn=cmd_run)

    sp = sub.add_parser("serve", help="launch and hold the desktop until the child exits")
    common(sp)
    sp.set_defaults(fn=cmd_serve)

    sp = sub.add_parser("windows", help="count top-level windows of a pid")
    sp.add_argument("--pid", type=int, required=True)
    sp.add_argument("--desktop", default=None)
    sp.set_defaults(fn=cmd_windows)

    sp = sub.add_parser("prove", help="self-check isolation: visible on the private desktop, invisible on console")
    common(sp)
    sp.add_argument("--dwell", type=int, default=6000)
    sp.add_argument("--min-windows", type=int, default=1)
    sp.add_argument("--expect-text", default=None,
                    help="若给了,还要断言该 pid 的 EDIT 内容等于这个值(宿主壳的读回口)")
    sp.set_defaults(fn=cmd_prove)

    sp = sub.add_parser("readtext", help="read back the EDIT content of a pid (WM_GETTEXT)")
    sp.add_argument("--pid", type=int, required=True)
    sp.add_argument("--desktop", default=None)
    sp.set_defaults(fn=cmd_readtext)

    a = p.parse_args()
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
