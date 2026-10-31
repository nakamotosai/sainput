//! sainput_tsf —— sainput 的 **TSF 文本服务**(TIP,Text Input Processor)。
//!
//! # 它是什么,为什么长这样
//!
//! TSF 文本服务是一个**进程内 COM 服务器**:Windows 把它加载进**目标应用自己的进程**,
//! 由那个进程里的 msctf 调 `DllGetClassObject` → `CreateInstance` → `ITfTextInputProcessor::Activate`。
//! 之后我们就可以在那个应用的**文档上下文**里做组合态(preedit):文本先进组合串、
//! 被反复改写,最后一次性提交 —— 屏幕上不会出现半句被改残的字,也不会产生一个按键。
//!
//! 这正是 SendInput/`WM_CHAR` 两条路都做不到的事:
//! * SendInput 会被 UIPI 挡在**管理员权限窗口**外面;
//! * `WM_CHAR` 那条路要自己算退格(`PLAN-NEXT.md` T4.5 那条既有缺陷就出在这里)。
//! TSF 不经过键盘、也不经过消息,它走的是应用自己的编辑上下文。
//!
//! # 关于这里的手写 COM
//!
//! 本仓 Win32 一律手写 `extern "system"`(不引 windows crate 的 COM 层),这里同样如此,
//! 好处是 ABI 完全显式、DLL 体积小、且**可以在任何宿主进程里安全运行**(见下)。
//! 结构体里第一个字段就是 vtable 指针 —— 这就是 COM 二进制契约的全部。
//!
//! # 在别人进程里跑,所以规矩更严
//!
//! 1. **任何跨 FFI 边界的 panic 都必须被吃掉**:`catch_unwind` 兜住,返回 `E_FAIL`。
//!    我们的代码跑在 Word/飞书/浏览器……里面,崩一次是崩别人的应用。
//! 2. **绝不写 stdout/stderr**(GUI 进程里没有控制台):诊断走日志文件,
//!    路径 = 环境变量 `SAINPUT_TSF_LOG`,默认 `%TEMP%\sainput-tsf.log`。
//! 3. 不做任何在激活阶段就可能失败的重量级动作:激活失败会让**整个输入法加载链**回退,
//!    用户看到的是"这个应用里输入法不见了"。
//!
//! 权威定义来自本机 Windows SDK 的 `um/msctf.idl`(IID 与 vtable 顺序照抄,不靠记忆):
//! `ITfTextInputProcessor` = `aa80e7f7-2021-11d2-93e0-0060b067b86e`,
//! `ITfTextInputProcessorEx` = `6e4e2102-f9cd-433d-b496-303ce03a6507`。

#![allow(non_snake_case)]

use std::ffi::c_void;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU32, Ordering};

// ------------------------------------------------------------------ 基础类型

type Hresult = i32;

const S_OK: Hresult = 0;
const S_FALSE: Hresult = 1;
const E_NOINTERFACE: Hresult = 0x8000_4002u32 as i32;
const E_FAIL: Hresult = 0x8000_4005u32 as i32;
const E_POINTER: Hresult = 0x8000_4003u32 as i32;
const CLASS_E_CLASSNOTAVAILABLE: Hresult = 0x8004_0111u32 as i32;

/// `GUID` 的内存布局(与 COM 一致):不是 `[u8; 16]`,前三个字段是**主机序整数**。
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Guid {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
    Guid { d1, d2, d3, d4 }
}

/// 本服务的 CLSID。注册表与"按进程激活"都按它认人,**一旦发布不能再改**。
pub const CLSID_SAINPUT_TSF: Guid = guid(
    0xFCC4_5A5E,
    0x09C4,
    0x410A,
    [0xB6, 0x96, 0xBD, 0xD2, 0xDA, 0x66, 0xA2, 0x1F],
);

const IID_IUNKNOWN: Guid = guid(0x0000_0000, 0x0000, 0x0000, [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46]);
/// `IClassFactory`(host 测试要按同一个 GUID 去 QI,所以公开)。
pub const IID_ICLASSFACTORY: Guid = guid(0x0000_0001, 0x0000, 0x0000, [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46]);
/// `ITfTextInputProcessor`(msctf.idl)。
pub const IID_ITF_TEXT_INPUT_PROCESSOR: Guid =
    guid(0xAA80_E7F7, 0x2021, 0x11D2, [0x93, 0xE0, 0x00, 0x60, 0xB0, 0x67, 0xB8, 0x6E]);
/// `ITfTextInputProcessorEx`(msctf.idl)。比上面多一个 `ActivateEx`。
pub const IID_ITF_TEXT_INPUT_PROCESSOR_EX: Guid =
    guid(0x6E4E_2102, 0xF9CD, 0x433D, [0xB4, 0x96, 0x30, 0x3C, 0xE0, 0x3A, 0x65, 0x07]);

// ------------------------------------------------------------------ 诊断日志

/// 进程内没有控制台,诊断只能落文件。每次打开-写-关,图的是"崩了也留下线索"。
fn log_path() -> std::path::PathBuf {
    match std::env::var_os("SAINPUT_TSF_LOG") {
        Some(p) if !p.is_empty() => std::path::PathBuf::from(p),
        _ => {
            let tmp = std::env::var_os("TEMP")
                .or_else(|| std::env::var_os("TMP"))
                .unwrap_or_else(|| "C:\\Windows\\Temp".into());
            std::path::PathBuf::from(tmp).join("sainput-tsf.log")
        }
    }
}

fn trace(msg: &str) {
    let line = format!(
        "pid={} tid={} {msg}\n",
        std::process::id(),
        unsafe { GetCurrentThreadId() }
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThreadId() -> u32;
    fn DisableThreadLibraryCalls(h: isize) -> i32;
}

// ------------------------------------------------------------------ COM 骨架

#[repr(C)]
struct IUnknownVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> Hresult,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct IClassFactoryVtbl {
    base: IUnknownVtbl,
    create_instance:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> Hresult,
    lock_server: unsafe extern "system" fn(*mut c_void, i32) -> Hresult,
}

#[repr(C)]
struct ITextInputProcessorVtbl {
    base: IUnknownVtbl,
    activate: unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> Hresult,
    deactivate: unsafe extern "system" fn(*mut c_void) -> Hresult,
    /// `ITfTextInputProcessorEx::ActivateEx`。放在后面,于是同一个对象**既是**
    /// `ITfTextInputProcessor` **也是** `ITfTextInputProcessorEx`(前两个方法位置相同,
    /// 只认前者的调用方看不到这一项)。
    activate_ex: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32) -> Hresult,
}

/// 还活着的 COM 对象数 —— `DllCanUnloadNow` 就报它。
static LIVE_OBJECTS: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
struct ClassFactory {
    vtbl: *const IClassFactoryVtbl,
    refs: AtomicU32,
}

#[repr(C)]
struct TextService {
    vtbl: *const ITextInputProcessorVtbl,
    refs: AtomicU32,
    /// 激活时拿到的线程管理器(后续做组合态要用,现在只记着)。
    thread_mgr: AtomicU32, // 低 32 位占位:真拿指针见 `active_ptr`
    active: AtomicU32,
    /// 线程管理器指针(`ITfThreadMgr*`)。用 `usize` 存,避免在原子量里放指针类型。
    active_ptr: AtomicU32,
    active_ptr_hi: AtomicU32,
}

impl TextService {
    unsafe fn set_thread_mgr(&self, tm: *mut c_void) {
        let v = tm as usize;
        self.active_ptr.store((v & 0xFFFF_FFFF) as u32, Ordering::SeqCst);
        self.active_ptr_hi.store((v >> 32) as u32, Ordering::SeqCst);
        self.active.store(1, Ordering::SeqCst);
    }
    unsafe fn get_thread_mgr(&self) -> *mut c_void {
        let lo = self.active_ptr.load(Ordering::SeqCst) as usize;
        let hi = self.active_ptr_hi.load(Ordering::SeqCst) as usize;
        ((hi << 32) | lo) as *mut c_void
    }
}

// ---- ClassFactory ----

static CLASS_FACTORY_VTBL: IClassFactoryVtbl = IClassFactoryVtbl {
    base: IUnknownVtbl {
        query_interface: factory_qi,
        add_ref: factory_add_ref,
        release: factory_release,
    },
    create_instance: factory_create_instance,
    lock_server: factory_lock_server,
};

unsafe extern "system" fn factory_qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    let r = catch_unwind(AssertUnwindSafe(|| {
        if riid.is_null() || ppv.is_null() {
            return E_POINTER;
        }
        let iid = unsafe { *riid };
        if iid == IID_IUNKNOWN || iid == IID_ICLASSFACTORY {
            unsafe { factory_add_ref(this) };
            unsafe { *ppv = this };
            S_OK
        } else {
            unsafe { *ppv = std::ptr::null_mut() };
            E_NOINTERFACE
        }
    }));
    r.unwrap_or(E_FAIL)
}

unsafe extern "system" fn factory_add_ref(this: *mut c_void) -> u32 {
    let f = unsafe { &*(this as *const ClassFactory) };
    f.refs.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn factory_release(this: *mut c_void) -> u32 {
    let f = unsafe { &*(this as *const ClassFactory) };
    let n = f.refs.fetch_sub(1, Ordering::SeqCst) - 1;
    if n == 0 {
        unsafe { drop(Box::from_raw(this as *mut ClassFactory)) };
        LIVE_OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
    n
}

unsafe extern "system" fn factory_create_instance(
    this: *mut c_void,
    _outer: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    let r = catch_unwind(AssertUnwindSafe(|| {
        if riid.is_null() || ppv.is_null() {
            return E_POINTER;
        }
        let iid = unsafe { *riid };
        let supported = iid == IID_IUNKNOWN
            || iid == IID_ITF_TEXT_INPUT_PROCESSOR
            || iid == IID_ITF_TEXT_INPUT_PROCESSOR_EX;
        if !supported {
            unsafe { *ppv = std::ptr::null_mut() };
            return E_NOINTERFACE;
        }
        let svc = Box::new(TextService {
            vtbl: &TEXT_SERVICE_VTBL,
            refs: AtomicU32::new(1),
            thread_mgr: AtomicU32::new(0),
            active: AtomicU32::new(0),
            active_ptr: AtomicU32::new(0),
            active_ptr_hi: AtomicU32::new(0),
        });
        LIVE_OBJECTS.fetch_add(1, Ordering::SeqCst);
        trace("CreateInstance -> TextService");
        unsafe { *ppv = Box::into_raw(svc) as *mut c_void };
        S_OK
    }));
    let _ = this;
    r.unwrap_or(E_FAIL)
}

unsafe extern "system" fn factory_lock_server(_this: *mut c_void, _lock: i32) -> Hresult {
    S_OK
}

// ---- TextService ----

static TEXT_SERVICE_VTBL: ITextInputProcessorVtbl = ITextInputProcessorVtbl {
    base: IUnknownVtbl {
        query_interface: svc_qi,
        add_ref: svc_add_ref,
        release: svc_release,
    },
    activate: svc_activate,
    deactivate: svc_deactivate,
    activate_ex: svc_activate_ex,
};

unsafe extern "system" fn svc_qi(this: *mut c_void, riid: *const Guid, ppv: *mut *mut c_void) -> Hresult {
    let r = catch_unwind(AssertUnwindSafe(|| {
        if riid.is_null() || ppv.is_null() {
            return E_POINTER;
        }
        let iid = unsafe { *riid };
        if iid == IID_IUNKNOWN || iid == IID_ITF_TEXT_INPUT_PROCESSOR || iid == IID_ITF_TEXT_INPUT_PROCESSOR_EX {
            unsafe { svc_add_ref(this) };
            unsafe { *ppv = this };
            S_OK
        } else {
            unsafe { *ppv = std::ptr::null_mut() };
            E_NOINTERFACE
        }
    }));
    r.unwrap_or(E_FAIL)
}

unsafe extern "system" fn svc_add_ref(this: *mut c_void) -> u32 {
    let s = unsafe { &*(this as *const TextService) };
    s.refs.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn svc_release(this: *mut c_void) -> u32 {
    let s = unsafe { &*(this as *const TextService) };
    let n = s.refs.fetch_sub(1, Ordering::SeqCst) - 1;
    if n == 0 {
        unsafe { drop(Box::from_raw(this as *mut TextService)) };
        LIVE_OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
    n
}

/// `ITfTextInputProcessor::Activate(ptim, tid)`。
///
/// 走到这里说明:**目标进程里的 msctf 认了我们、也把我们装起来了**。这是 T1.2 的验收点。
unsafe extern "system" fn svc_activate(this: *mut c_void, ptim: *mut c_void, tid: u32) -> Hresult {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let s = unsafe { &*(this as *const TextService) };
        unsafe { s.set_thread_mgr(ptim) };
        trace(&format!("Activate tid={tid} ptim={ptim:p}"));
        S_OK
    }));
    r.unwrap_or(E_FAIL)
}

unsafe extern "system" fn svc_activate_ex(
    this: *mut c_void,
    ptim: *mut c_void,
    tid: u32,
    flags: u32,
) -> Hresult {
    let r = catch_unwind(AssertUnwindSafe(|| {
        trace(&format!("ActivateEx tid={tid} flags={flags:#x}"));
        unsafe { svc_activate(this, ptim, tid) }
    }));
    r.unwrap_or(E_FAIL)
}

unsafe extern "system" fn svc_deactivate(this: *mut c_void) -> Hresult {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let s = unsafe { &*(this as *const TextService) };
        let tm = unsafe { s.get_thread_mgr() };
        s.active.store(0, Ordering::SeqCst);
        trace(&format!("Deactivate (had ptim={tm:p})"));
        S_OK
    }));
    r.unwrap_or(E_FAIL)
}

// ------------------------------------------------------------------ DLL 导出

/// msctf 用它拿类厂:我们只服务自己的那个 CLSID。
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const Guid,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    let r = catch_unwind(AssertUnwindSafe(|| {
        if rclsid.is_null() || riid.is_null() || ppv.is_null() {
            return E_POINTER;
        }
        if unsafe { *rclsid } != CLSID_SAINPUT_TSF {
            trace("DllGetClassObject: unknown CLSID");
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let iid = unsafe { *riid };
        if iid != IID_ICLASSFACTORY && iid != IID_IUNKNOWN {
            return E_NOINTERFACE;
        }
        let f = Box::new(ClassFactory {
            vtbl: &CLASS_FACTORY_VTBL,
            refs: AtomicU32::new(1),
        });
        LIVE_OBJECTS.fetch_add(1, Ordering::SeqCst);
        trace("DllGetClassObject -> ClassFactory");
        unsafe { *ppv = Box::into_raw(f) as *mut c_void };
        S_OK
    }));
    r.unwrap_or(E_FAIL)
}

/// 还有活对象时不能卸载(对象是堆上的,卸载后回调就悬空了)。
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> Hresult {
    if LIVE_OBJECTS.load(Ordering::SeqCst) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[no_mangle]
pub extern "system" fn DllMain(hinst: isize, reason: u32, _reserved: *mut c_void) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason == DLL_PROCESS_ATTACH {
        // 我们不起线程,不需要线程通知 —— 少一次开销、也少一个出错面。
        unsafe { DisableThreadLibraryCalls(hinst) };
        trace("DllMain PROCESS_ATTACH");
    }
    1
}

/// 调试/测试用:把日志文件路径回给宿主,免得测试去猜 `%TEMP%`。
#[no_mangle]
pub unsafe extern "system" fn SainputTsfLogPath(buf: *mut u16, cap: usize) -> u32 {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let p = log_path();
        let w: Vec<u16> = p.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        if buf.is_null() || cap == 0 || w.len() > cap {
            return w.len() as u32;
        }
        unsafe { std::ptr::copy_nonoverlapping(w.as_ptr(), buf, w.len()) };
        w.len() as u32
    }));
    r.unwrap_or(0)
}
