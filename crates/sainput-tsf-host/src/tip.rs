//! 在宿主进程里把 TSF 薄壳加载起来、走一遍**真实的 COM 激活链**。
//!
//! 为什么要在宿主里做而不是等真机注册:注册那一步要一次 UAC,不该成为"能不能用"的门。
//! msctf 加载 TIP 走的就是这几步 —— `LoadLibrary` → `DllGetClassObject` →
//! `IClassFactory::CreateInstance` → `ITfTextInputProcessor::Activate(ptim, tid)` ——
//! 我们在这里**原样走一遍**,就能在私有桌面上把 T1.2 的验收做成全自动:
//! 薄壳被装起来了没有、激活回调进没进、引用计数对不对、卸载干不干净。
//!
//! 真机注册之后的差别只有两点:谁调 `DllGetClassObject`(由 msctf 调)、
//! 以及 `tid` 是那个应用的线程 id。其余完全一致。

use crate::{wide, Log};
use sainput_tsf::{Guid, CLSID_SAINPUT_TSF, IID_ICLASSFACTORY, IID_ITF_TEXT_INPUT_PROCESSOR};
use std::ffi::c_void;
use std::time::{Duration, Instant};

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> isize;
    fn GetProcAddress(module: isize, name: *const u8) -> usize;
    fn FreeLibrary(module: isize) -> i32;
}

/// `msctf.dll` 的导入库在 w64devkit(GNU 工具链)里没有,所以**运行时取函数**。
/// 反正仓库本来就手写 FFI,这里只多一次 `GetProcAddress`,换来不依赖 SDK 的 .lib。
type TfCreateThreadMgrFn = unsafe extern "system" fn(*mut *mut c_void) -> i32;

unsafe fn resolve_msctf() -> Option<TfCreateThreadMgrFn> {
    let name = wide("msctf.dll");
    let m = unsafe { LoadLibraryW(name.as_ptr()) };
    if m == 0 {
        return None;
    }
    let f = unsafe { GetProcAddress(m, b"TF_CreateThreadMgr\0".as_ptr()) };
    if f == 0 {
        return None;
    }
    Some(unsafe { std::mem::transmute(f) })
}

/// COM 对象的开头永远是一个 vtable 指针;这里只声明用得着的那几个槽位。
#[repr(C)]
struct ThreadMgrVtbl {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    activate: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
    deactivate: unsafe extern "system" fn(*mut c_void) -> i32,
    create_document_mgr: usize,
    enum_document_mgrs: usize,
    /// `ITfThreadMgr::GetFocus`:返回**当前焦点文档管理器**。
    /// 它非空 = 那个应用已经有一个 TSF 文本存储在跑 —— 组合态上屏(T1.4)的前提。
    get_focus: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
}

#[repr(C)]
struct ClassFactoryVtbl {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    create_instance:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32,
}

#[repr(C)]
struct TipVtbl {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    activate: unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> i32,
    deactivate: unsafe extern "system" fn(*mut c_void) -> i32,
}

type DllGetClassObjectFn =
    unsafe extern "system" fn(*const Guid, *const Guid, *mut *mut c_void) -> i32;
type DllCanUnloadNowFn = unsafe extern "system" fn() -> i32;

fn hr(v: i32) -> String {
    format!("0x{v:08X}")
}

const S_OK: i32 = 0;
const S_FALSE: i32 = 1;

/// 问一次"这个线程现在的焦点文档管理器是谁"。
/// 非空 = 已经有 TSF 文本存储 —— 组合态上屏(T1.4)的前提。
pub unsafe fn focused_document(tm: *mut c_void) -> Option<*mut c_void> {
    if tm.is_null() {
        return None;
    }
    let vt = unsafe { *(tm as *const *const ThreadMgrVtbl) };
    let mut doc: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { ((*vt).get_focus)(tm, &mut doc) };
    if hr == 0 && !doc.is_null() {
        Some(doc)
    } else {
        None
    }
}

/// 返回值:每一步是否都如预期。
pub unsafe fn run_mode(dll: &str, hold_ms: u64, activate_for_process: bool, activate_legacy: bool, log: &Log) -> bool {
    let trace_ref = if log.tracing { Some(log) } else { None };
    let path = wide(dll);
    let module = unsafe { LoadLibraryW(path.as_ptr()) };
    if module == 0 {
        log.line(&format!("tip FATAL LoadLibrary failed: {dll}"));
        return false;
    }
    log.line(&format!("tip LoadLibrary ok module=0x{module:x}"));

    let mut ok = true;
    let check = |what: &str, got: i32, want: i32, l: &Log| {
        let good = got == want;
        l.line(&format!(
            "tip {what}: {} (hr={} want={})",
            if good { "OK" } else { "FAIL" },
            hr(got),
            hr(want)
        ));
        good
    };

    // 1) 线程管理器 —— 真实 TSF 也是把它交给 TIP 的
    let Some(tf_create) = (unsafe { resolve_msctf() }) else {
        log.line("tip FATAL cannot resolve TF_CreateThreadMgr from msctf.dll");
        return false;
    };
    let mut tm: *mut c_void = std::ptr::null_mut();
    let h = unsafe { tf_create(&mut tm) };
    ok &= check("TF_CreateThreadMgr", h, S_OK, log);
    let mut tid: u32 = 0;
    if !tm.is_null() {
        let vt = unsafe { *(tm as *const *const ThreadMgrVtbl) };
        let h = unsafe { ((*vt).activate)(tm, &mut tid) };
        ok &= check("ITfThreadMgr::Activate", h, S_OK, log);
        log.line(&format!("tip tid={tid} ptim={tm:p}"));
    } else {
        log.line("tip FATAL no thread manager");
        return false;
    }

    // 1b) 焦点文档:T1.4 能不能开工就看这一条(空 = 这个进程还没有 TSF 文本存储)
    let tm_vt = unsafe { *(tm as *const *const ThreadMgrVtbl) };
    let mut focus: *mut c_void = std::ptr::null_mut();
    let h = unsafe { ((*tm_vt).get_focus)(tm, &mut focus) };
    log.line(&format!(
        "tip GetFocus: hr={} docmgr={} {}",
        hr(h),
        if focus.is_null() { "NULL" } else { "非空" },
        if focus.is_null() { "(没有文本存储 —— 组合态上屏还谈不上)" } else { "(有文本存储!)" }
    ));

    // 2) DllGetClassObject —— msctf 也是这么拿类厂的
    let name = b"DllGetClassObject\0";
    let gco: DllGetClassObjectFn = unsafe { std::mem::transmute(GetProcAddress(module, name.as_ptr())) };
    let mut factory: *mut c_void = std::ptr::null_mut();
    let clsid = CLSID_SAINPUT_TSF;
    let iid_cf = IID_ICLASSFACTORY;
    let h = unsafe { gco(&clsid, &iid_cf, &mut factory) };
    ok &= check("DllGetClassObject", h, S_OK, log);
    if factory.is_null() {
        log.line("tip FATAL no class factory");
        return false;
    }

    // 3) CreateInstance 拿 ITfTextInputProcessor
    let vt = unsafe { *(factory as *const *const ClassFactoryVtbl) };
    let mut svc: *mut c_void = std::ptr::null_mut();
    let iid_tip = IID_ITF_TEXT_INPUT_PROCESSOR;
    let h = unsafe { ((*vt).create_instance)(factory, std::ptr::null_mut(), &iid_tip, &mut svc) };
    ok &= check("IClassFactory::CreateInstance", h, S_OK, log);
    if svc.is_null() {
        log.line("tip FATAL no TIP instance");
        return false;
    }

    // 4) 激活 —— 这一句进得去,说明薄壳在真实进程里站住了。
    //    激活期间它的**命名管道服务端**是活的(`\\.\pipe\sainput-tsf-<本进程 pid>`),
    //    外部测试就趁这个窗口连进来(T1.5)。
    let svc_vt = unsafe { *(svc as *const *const TipVtbl) };
    let h = unsafe { ((*svc_vt).activate)(svc, tm, tid) };
    ok &= check("ITfTextInputProcessor::Activate", h, S_OK, log);
    if hold_ms > 0 {
        log.line(&format!(
            "tip 保持激活 {hold_ms}ms:管道 \\\\.\\pipe\\sainput-tsf-{} 现在可连",
            std::process::id()
        ));
        println!("[host] tip pipe live pid={}", std::process::id());
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(hold_ms) {
            // **必须继续泵消息**:激活窗口里投进来的 WM_CHAR 就排在队列上,
            // 不泵的话它们要等到主循环才开始处理 —— 测出来的时序全是假的
            // (2026-10-31 实测:字在队列里躺了 11 秒)。
            crate::pump(trace_ref, true);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // 4b) T1.1:按 TF_IPPMF_FORPROCESS 在本进程激活我们的 TIP,
    //     并且看焦点文档管理器到底有没有出现(有它才谈得上组合态)。
    if activate_legacy {
        let ok11 = unsafe { crate::profile::activate_legacy(log, tm) };
        ok &= ok11;
        log.line(&format!("profile T1.1(老 API)结果 {}", if ok11 { "PASS" } else { "FAIL" }));
    }
    if activate_for_process {
        let ok11 = unsafe { crate::profile::activate_for_process(log, tm) };
        ok &= ok11;
        log.line(&format!("profile T1.1 结果 {}", if ok11 { "PASS" } else { "FAIL" }));
    }

    // 5) 还活着的时候不允许卸载(引用计数没算错)
    let cun: DllCanUnloadNowFn =
        unsafe { std::mem::transmute(GetProcAddress(module, b"DllCanUnloadNow\0".as_ptr())) };
    let h = unsafe { cun() };
    ok &= check("DllCanUnloadNow(有活对象)", h, S_FALSE, log);

    // 6) 收尾:反激活 + 释放;全部释放后必须允许卸载
    let h = unsafe { ((*svc_vt).deactivate)(svc) };
    ok &= check("ITfTextInputProcessor::Deactivate", h, S_OK, log);
    unsafe { ((*svc_vt).release)(svc) };
    unsafe { ((*vt).release)(factory) };
    // 反激活之后管道线程要一点时间收摊,所以这里**轮询**等它允许卸载 ——
    // 顺带把"线程还活着就不许卸"这条规矩也验了:如果薄壳忘了算这件事,这里会超时。
    let mut h = unsafe { cun() };
    let t0 = Instant::now();
    while h != S_OK && t0.elapsed() < Duration::from_millis(2000) {
        std::thread::sleep(Duration::from_millis(20));
        h = unsafe { cun() };
    }
    ok &= check("DllCanUnloadNow(释放后)", h, S_OK, log);

    // 7) 线程管理器还回去
    unsafe { ((*tm_vt).deactivate)(tm) };
    unsafe { ((*tm_vt).release)(tm) };

    // 8) 日志:薄壳自己写的那些行必须真的出现过
    if let Ok(dir) = std::env::var("TEMP") {
        let p = std::path::PathBuf::from(dir).join("sainput-tsf.log");
        let seen = std::fs::read_to_string(&p).unwrap_or_default();
        let need = ["DllGetClassObject -> ClassFactory", "CreateInstance -> TextService", "Activate tid="];
        for n in need {
            let good = seen.contains(n);
            ok &= good;
            log.line(&format!(
                "tip 薄壳日志含 {n:?}: {}",
                if good { "OK" } else { "FAIL" }
            ));
        }
        log.line(&format!("tip 薄壳日志路径 {p:?}"));
    }

    // 给日志一点落盘时间。
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(50) {
        std::thread::sleep(Duration::from_millis(10));
    }
    // **只在薄壳说"可以卸"时才卸**。它说不行而我们硬卸,等于让它自己的线程
    // 跳进一片已解除映射的代码 —— 崩的是宿主进程,而且是那种最难查的崩。
    if unsafe { cun() } == S_OK {
        unsafe { FreeLibrary(module) };
        log.line("tip FreeLibrary 已执行");
    } else {
        log.line("tip 薄壳说还不能卸(线程仍在收尾)—— 保持加载,不 FreeLibrary");
    }
    ok
}
