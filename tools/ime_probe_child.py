"""判断 IMM32 的组合串(输入法缓冲层)能不能**跨进程**写。

自己在中文输入法布局下建一个窗口,先把「本进程内能不能设组合串」验出来,
再把 hwnd 打给父进程,让父进程试同样的事 —— 以此区分:
  (a) 跨进程被挡(父进程拿到 NULL / 设不进去)
  (b) 只是这个窗口没有 IME 上下文
"""
import ctypes, ctypes.wintypes as w, threading, time, sys, json

u32 = ctypes.windll.user32
k32 = ctypes.windll.kernel32
imm = ctypes.windll.imm32
HIMC = ctypes.c_void_p
LRESULT = ctypes.c_ssize_t
SCS_SETSTR = 0x0009
GCS_COMPSTR = 0x0008
WS_CHILD, WS_BORDER, ES_MULTILINE = 0x40000000, 0x00800000, 0x0004
KLF_ACTIVATE = 0x00000001
WM_IME_SETCONTEXT = 0x0281
ISC_SHOWUICOMPOSITIONWINDOW = 0x80000000
WM_GETTEXT = 0x000D

imm.ImmGetContext.restype = HIMC
imm.ImmGetContext.argtypes = [w.HWND]
imm.ImmReleaseContext.restype = ctypes.c_bool
imm.ImmReleaseContext.argtypes = [w.HWND, HIMC]
imm.ImmSetCompositionStringW.restype = ctypes.c_bool
imm.ImmSetCompositionStringW.argtypes = [HIMC, ctypes.c_uint, ctypes.c_void_p, ctypes.c_uint,
                                        ctypes.c_void_p, ctypes.c_uint]
imm.ImmGetCompositionStringW.restype = ctypes.c_long
imm.ImmGetCompositionStringW.argtypes = [HIMC, ctypes.c_uint, ctypes.c_void_p, ctypes.c_uint]
imm.ImmGetOpenStatus.restype = ctypes.c_bool
imm.ImmGetOpenStatus.argtypes = [HIMC]
imm.ImmSetOpenStatus.restype = ctypes.c_bool
imm.ImmSetOpenStatus.argtypes = [HIMC, ctypes.c_bool]
imm.ImmGetDescriptionW.restype = ctypes.c_uint
imm.ImmGetDescriptionW.argtypes = [HIMC, w.LPWSTR, ctypes.c_uint]
u32.LoadKeyboardLayoutW.restype = ctypes.c_void_p
u32.LoadKeyboardLayoutW.argtypes = [w.LPCWSTR, ctypes.c_uint]
u32.ActivateKeyboardLayout.restype = ctypes.c_void_p
u32.ActivateKeyboardLayout.argtypes = [ctypes.c_void_p, ctypes.c_uint]


def wp(h, m, wp_, lp):
    u32.DefWindowProcW.restype = LRESULT
    u32.DefWindowProcW.argtypes = [w.HWND, ctypes.c_uint, w.WPARAM, w.LPARAM]
    return u32.DefWindowProcW(h, m, wp_, lp)


CB = ctypes.WINFUNCTYPE(LRESULT, w.HWND, ctypes.c_uint, w.WPARAM, w.LPARAM)(wp)


class WC(ctypes.Structure):
    _fields_ = [('cbSize', ctypes.c_uint), ('style', ctypes.c_uint), ('lpfnWndProc', type(CB)),
                ('cbClsExtra', ctypes.c_int), ('cbWndExtra', ctypes.c_int), ('hInstance', w.HINSTANCE),
                ('hIcon', w.HICON), ('hCursor', w.HANDLE), ('hbrBackground', w.HBRUSH),
                ('lpszMenuName', w.LPCWSTR), ('lpszClassName', w.LPCWSTR), ('hIconSm', w.HICON)]


box = {}


def worker():
    hkl = u32.LoadKeyboardLayoutW('00000804', KLF_ACTIVATE)   # 简体中文(会选到已装的输入法)
    u32.ActivateKeyboardLayout(ctypes.c_void_p(hkl), 0)
    hi = k32.GetModuleHandleW(None)
    wc = WC(); wc.cbSize = ctypes.sizeof(WC); wc.lpfnWndProc = CB; wc.hInstance = hi
    wc.lpszClassName = 'SainputImeChild'
    u32.RegisterClassExW(ctypes.byref(wc))
    box['top'] = u32.CreateWindowExW(0, 'SainputImeChild', 'child', 0, 0, 0, 600, 400, None, None, hi, None)
    box['edit'] = u32.CreateWindowExW(0, 'EDIT', '', WS_CHILD | WS_BORDER | ES_MULTILINE,
                                      0, 0, 580, 380, box['top'], None, hi, None)
    u32.SendMessageW(box['top'], WM_IME_SETCONTEXT, 1, ISC_SHOWUICOMPOSITIONWINDOW)
    box['hkl'] = hkl
    box['ready'] = True
    msg = w.MSG()
    while u32.GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
        u32.TranslateMessage(ctypes.byref(msg)); u32.DispatchMessageW(ctypes.byref(msg))


def set_comp(h, s):
    b = s.encode('utf-16-le')
    return bool(imm.ImmSetCompositionStringW(h, SCS_SETSTR, b, len(b), None, 0))


def get_comp(h):
    n = imm.ImmGetCompositionStringW(h, GCS_COMPSTR, None, 0)
    if n <= 0:
        return None
    buf = ctypes.create_string_buffer(n)
    imm.ImmGetCompositionStringW(h, GCS_COMPSTR, buf, n)
    return buf.raw.decode('utf-16-le')


def probe(win, tag):
    himc = imm.ImmGetContext(win)
    out = {'tag': tag, 'himc': hex(himc) if himc else None}
    if himc:
        d = ctypes.create_unicode_buffer(128)
        imm.ImmGetDescriptionW(himc, d, 128)
        out['ime'] = d.value
        out['open'] = bool(imm.ImmGetOpenStatus(himc))
        out['set_comp'] = set_comp(himc, '因为我看其他的输入法')
        out['read_back'] = get_comp(himc)
        out['set_comp2'] = set_comp(himc, '因为我看其他的输入法能做到上屏后的文字还能实时变化')
        out['read_back2'] = get_comp(himc)
    return out


threading.Thread(target=worker, daemon=True).start()
for _ in range(50):
    if box.get('ready'):
        break
    time.sleep(0.1)
time.sleep(0.6)
tid = k32.GetCurrentThreadId()
print(json.dumps({
    'pid': k32.GetCurrentProcessId(),
    'tid': tid,
    'hkl': hex(u32.GetKeyboardLayout(tid) or 0),
    'top': box['top'],
    'edit': box['edit'],
    'self_probe': probe(box['edit'], 'child-edit'),
}, ensure_ascii=False))
sys.stdout.flush()
time.sleep(25)
