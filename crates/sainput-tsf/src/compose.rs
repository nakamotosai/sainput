//! 组合态(preedit):把文本写进**应用自己的编辑上下文**。
//!
//! 这是 TSF 那条路真正的价值 —— 文本先进组合串、可以被反复整体改写,定稿时才落进文档。
//! 屏幕上不会出现半句被改残的字,而且**一个按键都不发**(对比 SendInput / WM_CHAR 那两条路)。
//!
//! # 走法(每一步都落日志,失败时那就是唯一的现场)
//!
//! ```text
//!   ITfContext::RequestEditSession(tid, 我们的会话, TF_ES_SYNC|TF_ES_READWRITE, &hr)
//!        ↓ msctf 向应用要锁,锁到手后跑我们的会话
//!   我们的 ITfEditSession::DoEditSession(ec):
//!        StartComposition(ec, sink, &composition)        // 第一次 set 时开
//!        composition.GetRange(&range)
//!        range.SetText(ec, 0, 文本, 长度)                 // 每帧**整体替换**
//!        composition.EndComposition(ec)                  // 定稿(取消时先 SetText(空))
//! ```
//!
//! # 两条硬规矩
//!
//! 1. **不持有自己的锁去回调 TSF**:`SetText`/`EndComposition` 可能同步回调我们的
//!    `ITfCompositionSink`(它也要读同一份状态)—— 拿锁去调就是自己锁死自己。
//!    所以状态只在两次 TSF 调用**之间**短暂加锁。
//! 2. 指针一律存 `AtomicUsize`,不拆高低位(32 位上 `>> 32` 直接编译不过,薄壳里踩过)。
//!
//! IID 与槽位顺序全部照本机 SDK 的 `um/msctf.idl`:
//! `ITfContext` = `aa80e7fd-…`(RequestEditSession 是**第一个**方法、槽位 3)、
//! `ITfContextComposition` = `D40C8AAE-…`、`ITfComposition` = `20168D64-…`、
//! `ITfRange` = `aa80e7ff-…`(SetText 是第二个方法)、
//! `ITfEditSession` = `aa80e803-…`、`ITfCompositionSink` = `A781718C-…`。

use crate::{trace, Guid};
use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

#[link(name = "kernel32")]
extern "system" {
    /// 只为日志取证:同步编辑会话卡住时,得知道"谁在等谁"。
    fn GetCurrentThreadId() -> u32;
    fn GetModuleHandleExW(flags: u32, addr: *const u32, out: *mut isize) -> i32;
}

type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;

#[repr(C)]
struct WndClassW {
    style: u32,
    lpfn_wnd_proc: Option<WndProc>,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: isize,
    h_icon: isize,
    h_cursor: isize,
    hbr_background: isize,
    menu_name: *const u16,
    class_name: *const u16,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(c: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex: u32, cls: *const u16, title: *const u16, style: u32,
        x: i32, y: i32, w: i32, h: i32,
        parent: isize, menu: isize, inst: isize, param: isize,
    ) -> isize;
    fn DefWindowProcW(h: isize, m: u32, w: usize, l: isize) -> isize;
    fn PostMessageW(h: isize, m: u32, w: usize, l: isize) -> i32;
}

/// 组合态**必须在拥有该上下文的线程上执行**。
///
/// 为什么(2026-10-31 实测,不是推断):薄壳的管道线程直接拿主线程取到的 TSF 接口去调,
/// `ITfContextComposition::StartComposition` 会**死锁** —— 会话确实是在调用者线程上跑
/// (日志:本线程 = 管道线程)、状态锁也没问题,但公寓模型下 msctf 在那儿等属主线程。
/// 于是做法改成:在属主线程(激活我们的那条线程)建一个**消息窗口**,管道线程把活儿
/// `PostMessage` 过去、等回执。真输入法本来就是这个结构 —— 引擎的数据面永远不该直接碰 TSF。
static OWNER_TID: AtomicU32 = AtomicU32::new(0);
static OWNER_HWND: AtomicUsize = AtomicUsize::new(0);
static OWNER_CLASS_ATOM: AtomicU32 = AtomicU32::new(0);
const WM_COMPOSE_JOB: u32 = 0x8000 + 0x51; // WM_APP + 0x51
/// 等属主线程的回执上限。超时就回 E_FAIL,让引擎按 T1.6 退一档 —— **绝不能无限等**。
const JOB_TIMEOUT_MS: u64 = 2500;
/// **熔断**:一旦超时,接下来这段时间里直接回 E_FAIL,不再投递、不再等。
///
/// 为什么必须有它:引擎是**逐帧**发 set 的。属主线程要是卡住(比如 `StartComposition`
/// 在那儿等一个永远不来的东西),每帧都等 2.5 秒 = 一句话几十秒 —— 这比退化更糟。
/// 熔断之后这一句立刻退回旧通道,用户只感觉到"这一句不是组合态上的",不感觉卡。
static STUCK_UNTIL: AtomicUsize = AtomicUsize::new(0);
const STUCK_COOLDOWN_MS: u64 = 30_000;

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

struct JobSlot {
    done: Mutex<bool>,
    cv: Condvar,
    result: AtomicI32,
}

struct Job {
    intent: Intent,
    slot: Arc<JobSlot>,
}

unsafe extern "system" fn owner_wndproc(h: isize, m: u32, w: usize, l: isize) -> isize {
    if m == WM_COMPOSE_JOB {
        // 回到属主线程了:在这里直接做 —— 接口是这条线程取的,不再跨公寓。
        let job = unsafe { Box::from_raw(l as *mut Job) };
        let r = request_inline(job.intent.clone());
        job.slot.result.store(r, Ordering::SeqCst);
        if let Ok(mut g) = job.slot.done.lock() {
            *g = true;
            job.slot.cv.notify_all();
        }
        return 0;
    }
    unsafe { DefWindowProcW(h, m, w, l) }
}

type Hresult = i32;
const S_OK: Hresult = 0;
const E_FAIL: Hresult = 0x8000_4005u32 as i32;
const E_NOINTERFACE: Hresult = 0x8000_4002u32 as i32;

const TF_ES_SYNC: u32 = 0x1;
const TF_ES_READWRITE: u32 = 0x6;

const IID_IUNKNOWN: Guid =
    Guid::from_parts(0x0000_0000, 0x0000, 0x0000, [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46]);
const IID_ITF_CONTEXT_COMPOSITION: Guid =
    Guid::from_parts(0xD40C_8AAE, 0xAC92, 0x4FC7, [0x9A, 0x11, 0x0E, 0xE0, 0xE2, 0x3A, 0xA3, 0x9B]);
/// `ITfCompositionSink`(msctf.idl:`uuid(A781718C-579A-4B15-A280-32B8577ACC5E)` = 本机 SDK 抄的)。
const IID_ITF_COMPOSITION_SINK: Guid =
    Guid::from_parts(0xA781_718C, 0x579A, 0x4B15, [0xA2, 0x80, 0x32, 0xB8, 0x57, 0x7A, 0xCC, 0x5E]);
const IID_ITF_EDIT_SESSION: Guid =
    Guid::from_parts(0xAA80_E803, 0x2021, 0x11D2, [0x93, 0xE0, 0x00, 0x60, 0xB0, 0x67, 0xB8, 0x6E]);

// ---------------------------------------------------------------- 只在用到的槽位

#[repr(C)]
struct UnknownVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> Hresult,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

/// `ITfThreadMgr`:只要 GetFocus(第 5 个)与 SetFocus(第 6 个)那两个槽。
#[repr(C)]
pub(crate) struct ThreadMgrPrefix {
    pub query_interface: usize,
    pub add_ref: usize,
    pub release: usize,
    pub activate: usize,
    pub deactivate: usize,
    pub create_document_mgr: usize,
    pub enum_document_mgrs: usize,
    pub get_focus: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> Hresult,
}

/// `ITfDocumentMgr`:只要 GetTop(第 5 个)。
#[repr(C)]
struct DocMgrPrefix {
    query_interface: usize,
    add_ref: usize,
    release: usize,
    create_context: usize,
    duplicate_context: usize,
    pop: usize,
    push: usize,
    get_top: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> Hresult,
}

/// `ITfContext`:我们只用**第一个**方法(RequestEditSession)。
#[repr(C)]
struct ContextVtbl {
    base: UnknownVtbl,
    request_edit_session:
        unsafe extern "system" fn(*mut c_void, u32, *mut c_void, u32, *mut Hresult) -> Hresult,
}

/// `ITfContextComposition`:StartComposition 是第一个方法。
#[repr(C)]
struct ContextCompositionVtbl {
    base: UnknownVtbl,
    start_composition:
        unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut *mut c_void) -> Hresult,
}

/// `ITfComposition`:GetRange / ShiftStart / ShiftEnd / EndComposition。
#[repr(C)]
struct CompositionVtbl {
    base: UnknownVtbl,
    get_range: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> Hresult,
    shift_start: usize,
    shift_end: usize,
    end_composition: unsafe extern "system" fn(*mut c_void, u32) -> Hresult,
}

/// `ITfRange`:GetText / **SetText**。
#[repr(C)]
struct RangeVtbl {
    base: UnknownVtbl,
    get_text: usize,
    set_text: unsafe extern "system" fn(*mut c_void, u32, u32, *const u16, u32) -> Hresult,
}

/// `ITfEditSession`:只有 DoEditSession。
#[repr(C)]
struct EditSessionVtbl {
    base: UnknownVtbl,
    do_edit_session: unsafe extern "system" fn(*mut c_void, u32) -> Hresult,
}

/// `ITfCompositionSink`:组合态被外部结束时会回调。
#[repr(C)]
struct CompositionSinkVtbl {
    base: UnknownVtbl,
    on_composition_terminated: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> Hresult,
}

// ---------------------------------------------------------------- 状态

#[derive(Clone)]
enum Intent {
    Set(String),
    Commit(String),
    Cancel,
}

struct EditSession {
    vtbl: *const EditSessionVtbl,
    refs: AtomicU32,
    intent: Mutex<Intent>,
    result: AtomicI32,
}

#[repr(C)]
struct CompositionSink {
    vtbl: *const CompositionSinkVtbl,
    refs: AtomicU32,
}

#[derive(Default)]
struct State {
    composition: usize,
    text: String,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
static CURRENT_CTX: AtomicUsize = AtomicUsize::new(0);
static CURRENT_TID: AtomicU32 = AtomicU32::new(0);
static SINK: LazyLock<usize> = LazyLock::new(|| {
    let s = Box::new(CompositionSink {
        vtbl: &SINK_VTBL,
        refs: AtomicU32::new(1),
    });
    Box::into_raw(s) as usize
});

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

fn hr_string(h: Hresult) -> String {
    format!("0x{:08X}", h as u32)
}

// ---------------------------------------------------------------- 会话对象

unsafe extern "system" fn session_qi(this: *mut c_void, riid: *const Guid, ppv: *mut *mut c_void) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    let want = unsafe { *riid };
    if want == IID_IUNKNOWN || want == IID_ITF_EDIT_SESSION {
        unsafe { *ppv = this };
        unsafe { session_add_ref(this) };
        S_OK
    } else {
        unsafe { *ppv = std::ptr::null_mut() };
        E_NOINTERFACE
    }
}

unsafe extern "system" fn session_add_ref(this: *mut c_void) -> u32 {
    let s = unsafe { &*(this as *const EditSession) };
    s.refs.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn session_release(this: *mut c_void) -> u32 {
    let s = unsafe { &*(this as *const EditSession) };
    let n = s.refs.fetch_sub(1, Ordering::SeqCst) - 1;
    if n == 0 {
        unsafe { drop(Box::from_raw(this as *mut EditSession)) };
    }
    n
}

/// 真正干活的地方:TSF 已经把锁给了我们,`ec` 是这次写事务的 cookie。
unsafe extern "system" fn session_do_edit(this: *mut c_void, ec: u32) -> Hresult {
    // 每步都记 tid:同步会话一旦卡住,"谁停在哪个调用"是唯一能查的东西。
    macro_rules! step {
        ($what:expr) => {
            trace(&format!("compose 步骤[{}] 线程={}", $what, unsafe { GetCurrentThreadId() }))
        };
    }
    step!("DoEditSession 进入");
    let s = unsafe { &*(this as *const EditSession) };
    let intent = s.intent.lock().map(|g| g.clone()).unwrap_or(Intent::Cancel);
    // 从这里开始,**任何 TSF 调用之前不要持有 STATE 的锁**(它会回调我们)。
    let ctx = CURRENT_CTX.load(Ordering::SeqCst) as *mut c_void;
    if ctx.is_null() {
        trace("compose DoEditSession:没有上下文(attach 没成功)");
        s.result.store(E_FAIL, Ordering::SeqCst);
        return E_FAIL;
    }
    let cvt = unsafe { *(ctx as *const *const ContextVtbl) };
    let mut comp_obj: *mut c_void = std::ptr::null_mut();
    let iid = IID_ITF_CONTEXT_COMPOSITION;
    let hr = unsafe { ((*cvt).base.query_interface)(ctx, &iid, &mut comp_obj) };
    trace(&format!(
        "compose QI(ITfContextComposition) hr={} obj={comp_obj:p}",
        hr_string(hr)
    ));
    if hr != 0 || comp_obj.is_null() {
        s.result.store(hr, Ordering::SeqCst);
        return hr;
    }

    // 已经开着组合态就复用,否则开一个。
    step!("准备取 composition 状态锁");
    let mut comp = with_state(|st| st.composition) as *mut c_void;
    step!("状态锁已放开");
    if comp.is_null() {
        let ccvt = unsafe { *(comp_obj as *const *const ContextCompositionVtbl) };
        let sink = *SINK as *mut c_void;
        step!("准备 StartComposition");
        let hr = unsafe { ((*ccvt).start_composition)(comp_obj, ec, sink, &mut comp) };
        trace(&format!("compose StartComposition hr={}", hr_string(hr)));
        if hr != 0 || comp.is_null() {
            s.result.store(hr, Ordering::SeqCst);
            return hr;
        }
        with_state(|st| st.composition = comp as usize);
    }

    let comp_vt = unsafe { *(comp as *const *const CompositionVtbl) };
    let mut range: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { ((*comp_vt).get_range)(comp, &mut range) };
    trace(&format!("compose GetRange hr={}", hr_string(hr)));
    if hr != 0 || range.is_null() {
        s.result.store(hr, Ordering::SeqCst);
        return hr;
    }
    let range_vt = unsafe { *(range as *const *const RangeVtbl) };

    let is_commit = text_is_commit(&intent);
    match intent {
        Intent::Set(text) | Intent::Commit(text) => {
            let wide: Vec<u16> = text.encode_utf16().collect();
            let hr = unsafe { ((*range_vt).set_text)(range, ec, 0, wide.as_ptr(), wide.len() as u32) };
            trace(&format!(
                "compose SetText {text:?}({} 字) hr={}",
                text.chars().count(),
                hr_string(hr)
            ));
            with_state(|st| st.text = text);
            if is_commit {
                let hr = unsafe { ((*comp_vt).end_composition)(comp, ec) };
                trace(&format!("compose EndComposition(定稿) hr={}", hr_string(hr)));
                with_state(|st| {
                    st.composition = 0;
                    st.text.clear();
                });
            }
        }
        Intent::Cancel => {
            // 取消 = 清空组合串再结束 —— 文档里不该留下任何东西。
            let empty: Vec<u16> = Vec::new();
            let hr = unsafe { ((*range_vt).set_text)(range, ec, 0, empty.as_ptr(), 0) };
            trace(&format!("compose SetText(空) hr={}", hr_string(hr)));
            let hr = unsafe { ((*comp_vt).end_composition)(comp, ec) };
            trace(&format!("compose EndComposition(取消) hr={}", hr_string(hr)));
            with_state(|st| {
                st.composition = 0;
                st.text.clear();
            });
        }
    }
    s.result.store(S_OK, Ordering::SeqCst);
    S_OK
}

fn text_is_commit(intent: &Intent) -> bool {
    matches!(intent, Intent::Commit(_))
}

static SESSION_VTBL: EditSessionVtbl = EditSessionVtbl {
    base: UnknownVtbl {
        query_interface: session_qi,
        add_ref: session_add_ref,
        release: session_release,
    },
    do_edit_session: session_do_edit,
};

// ---------------------------------------------------------------- 组合态接收端(sink)

unsafe extern "system" fn sink_qi(this: *mut c_void, riid: *const Guid, ppv: *mut *mut c_void) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    // 只应答我们**真的是**的接口。之前这里对任何 IID 都回 `this` —— msctf 拿到一个
    // 名不副实的指针之后行为不可预期(它按自己的接口布局去调,而我们的 vtable 更短)。
    let want = unsafe { *riid };
    if want == IID_IUNKNOWN || want == IID_ITF_COMPOSITION_SINK {
        unsafe { *ppv = this };
        unsafe { sink_add_ref(this) };
        S_OK
    } else {
        unsafe { *ppv = std::ptr::null_mut() };
        E_NOINTERFACE
    }
}

unsafe extern "system" fn sink_add_ref(this: *mut c_void) -> u32 {
    let s = unsafe { &*(this as *const CompositionSink) };
    s.refs.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn sink_release(this: *mut c_void) -> u32 {
    let s = unsafe { &*(this as *const CompositionSink) };
    s.refs.fetch_sub(1, Ordering::SeqCst) - 1
}

unsafe extern "system" fn sink_on_terminated(_this: *mut c_void, _ec: u32, _comp: *mut c_void) -> Hresult {
    // 应用/别的东西结束了组合态(用户点了别处、应用自己收尾)。记住这件事,别再用旧指针。
    trace("compose OnCompositionTerminated(外部结束了组合态)");
    with_state(|st| {
        st.composition = 0;
        st.text.clear();
    });
    S_OK
}

static SINK_VTBL: CompositionSinkVtbl = CompositionSinkVtbl {
    base: UnknownVtbl {
        query_interface: sink_qi,
        add_ref: sink_add_ref,
        release: sink_release,
    },
    on_composition_terminated: sink_on_terminated,
};

// ---------------------------------------------------------------- 对外接口

/// 记下"当前上下文"(`Activate` 时调;组合态就写进它)。
pub unsafe fn attach_context(tm: *mut c_void, tid: u32) {
    if tm.is_null() {
        return;
    }
    let tvt = unsafe { *(tm as *const *const ThreadMgrPrefix) };
    let mut dim: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { ((*tvt).get_focus)(tm, &mut dim) };
    if hr != 0 || dim.is_null() {
        trace(&format!(
            "compose attach:GetFocus hr={} dim=null(这个进程还没有 TSF 文本存储)",
            hr_string(hr)
        ));
        return;
    }
    let dvt = unsafe { *(dim as *const *const DocMgrPrefix) };
    let mut ctx: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { ((*dvt).get_top)(dim, &mut ctx) };
    trace(&format!("compose attach:GetTop hr={} ctx={ctx:p}", hr_string(hr)));
    if hr == 0 && !ctx.is_null() {
        CURRENT_CTX.store(ctx as usize, Ordering::SeqCst);
        CURRENT_TID.store(tid, Ordering::SeqCst);
        // 组合态要在这个线程上执行(见 `owner_wndproc` 的说明)。这里就是属主线程 ——
        // 建一个消息窗口当投递口。
        ensure_owner_window();
    }
}

/// 建"属主线程投递口"。只建一次;在哪个线程调就在哪个线程建 —— 那个线程就是属主。
fn ensure_owner_window() {
    let me = unsafe { GetCurrentThreadId() };
    if OWNER_HWND.load(Ordering::SeqCst) != 0 {
        // 已经在别的线程建过:记下"最新的那次激活在哪条线程",投递口跟着走。
        if OWNER_TID.load(Ordering::SeqCst) == me {
            return;
        }
        trace(&format!(
            "compose 属主窗口已在别的线程上(旧 tid={} 现在 tid={me}),沿用旧的",
            OWNER_TID.load(Ordering::SeqCst)
        ));
        return;
    }
    unsafe {
        let mut cls = WndClassW {
            style: 0,
            lpfn_wnd_proc: Some(owner_wndproc),
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: 0,
            h_icon: 0,
            h_cursor: 0,
            hbr_background: 0,
            menu_name: std::ptr::null(),
            class_name: std::ptr::null(),
        };
        // 类名要按进程唯一:`HWND_MESSAGE` 的窗口没有 Z 序,但类名是进程全局的,
        // 两个进程同名会互相干扰。
        let name: Vec<u16> = format!("sainput-tsf-owner-{}", std::process::id())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        cls.class_name = name.as_ptr();
        // hInstance 用"我们自己代码的地址"反查 —— DLL 里 `GetModuleHandleW(NULL)` 拿到的是
        // 宿主 exe 的实例,注册类会注册到别人名下。
        let mut hinst: isize = 0;
        const FROM_ADDRESS: u32 = 0x4;
        let addr = owner_wndproc as *const u32;
        let _ = GetModuleHandleExW(FROM_ADDRESS, addr, &mut hinst);
        cls.h_instance = hinst;
        let atom = RegisterClassW(&cls);
        OWNER_CLASS_ATOM.store(atom as u32, Ordering::SeqCst);
        const HWND_MESSAGE: isize = -3;
        let hwnd = CreateWindowExW(
            0,
            name.as_ptr(),
            name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            0,
            hinst,
            0,
        );
        if hwnd == 0 {
            trace("compose 属主消息窗口建不出来(组合态将退回本线程直调)");
            return;
        }
        OWNER_HWND.store(hwnd as usize, Ordering::SeqCst);
        OWNER_TID.store(me, Ordering::SeqCst);
        trace(&format!(
            "compose 属主投递口就绪 hwnd=0x{hwnd:x} tid={me}(组合态将在这条线程上执行)"
        ));
    }
}

/// 一次组合态动作:不在属主线程上就投递过去,在属主线程上就直接做。
fn request(intent: Intent) -> Hresult {
    let hwnd = OWNER_HWND.load(Ordering::SeqCst) as isize;
    let owner = OWNER_TID.load(Ordering::SeqCst);
    let me = unsafe { GetCurrentThreadId() };
    if hwnd == 0 || owner == me {
        return request_inline(intent);
    }
    if now_ms() < STUCK_UNTIL.load(Ordering::SeqCst) as u64 {
        // 上一次投递超时了:冷却是必须的(见 STUCK_UNTIL 的说明)。
        trace("compose 熔断中:跳过组合态,直接回 E_FAIL(让引擎走旧通道)");
        return E_FAIL;
    }
    // **盒子会被 wndproc 拿走**,所以回执与结果都放在共享槽里(不能读那个 Box)。
    let slot = Arc::new(JobSlot {
        done: Mutex::new(false),
        cv: Condvar::new(),
        result: AtomicI32::new(E_FAIL),
    });
    let job = Box::new(Job {
        intent: intent.clone(),
        slot: slot.clone(),
    });
    let sp = Box::into_raw(job);
    let posted = unsafe { PostMessageW(hwnd, WM_COMPOSE_JOB, 0, sp as isize) };
    if posted == 0 {
        // 投不进去 = 属主线程的队列有问题(进程在退出?)。把 Box 收回来,别泄漏。
        unsafe { drop(Box::from_raw(sp)) };
        trace("compose 投递属主线程失败(PostMessage=0),退回本线程直调");
        return request_inline(intent);
    }
    let mut done = slot.done.lock().unwrap_or_else(|e| e.into_inner());
    let (d2, _timeout) = slot
        .cv
        .wait_timeout(done, Duration::from_millis(JOB_TIMEOUT_MS))
        .unwrap_or_else(|e| e.into_inner());
    done = d2;
    if !*done {
        // 属主线程没回执(比如它自己也卡着)。**不能无限等** —— 回 E_FAIL,
        // 引擎按 T1.6 退一档(这一句退回旧的按键通道),用户那一句不会丢。
        STUCK_UNTIL.store((now_ms() + STUCK_COOLDOWN_MS) as usize, Ordering::SeqCst);
        trace(&format!(
            "compose 等属主线程回执超时({JOB_TIMEOUT_MS}ms)—— 回 E_FAIL 让引擎退一档,\
             并熔断 {STUCK_COOLDOWN_MS}ms(后面每帧不再白等)"
        ));
        return E_FAIL;
    }
    // 成功一次就把熔断清掉(属主线程恢复正常了)。
    STUCK_UNTIL.store(0, Ordering::SeqCst);
    slot.result.load(Ordering::SeqCst)
}

/// 在**当前线程**上真正做一次组合态动作(属主线程走这条)。
fn request_inline(intent: Intent) -> Hresult {
    let ctx = CURRENT_CTX.load(Ordering::SeqCst) as *mut c_void;
    if ctx.is_null() {
        trace("compose request:没有上下文(attach 没成功)");
        return E_FAIL;
    }
    let tid = CURRENT_TID.load(Ordering::SeqCst);
    let session = Box::new(EditSession {
        vtbl: &SESSION_VTBL,
        refs: AtomicU32::new(1),
        intent: Mutex::new(intent),
        result: AtomicI32::new(E_FAIL),
    });
    let sp = Box::into_raw(session) as *mut c_void;
    let cvt = unsafe { *(ctx as *const *const ContextVtbl) };
    let mut session_hr: Hresult = E_FAIL;
    // **请求前**就要把 tid 记下来:同步请求一旦没人服务,这一行之后就没有日志了 ——
    // "到底把会话投给了哪个线程"必须在这之前落盘(2026-10-31:卡在这一步查了半天)。
    trace(&format!(
        "compose RequestEditSession 投递 ctx={ctx:p} tid={tid} 本线程={} 同步=1",
        unsafe { GetCurrentThreadId() }
    ));
    let hr = unsafe {
        ((*cvt).request_edit_session)(ctx, tid, sp, TF_ES_SYNC | TF_ES_READWRITE, &mut session_hr)
    };
    let result = unsafe { (*(sp as *const EditSession)).result.load(Ordering::SeqCst) };
    trace(&format!(
        "compose RequestEditSession hr={} 会话结果={}",
        hr_string(hr),
        hr_string(result)
    ));
    unsafe { session_release(sp) };
    if hr == 0 {
        result
    } else {
        hr
    }
}

pub fn set_text(text: &str) -> Hresult {
    request(Intent::Set(text.to_string()))
}

pub fn commit(text: &str) -> Hresult {
    request(Intent::Commit(text.to_string()))
}

pub fn cancel() -> Hresult {
    request(Intent::Cancel)
}

/// 组合态当前文本(诊断/测试用)。
pub fn current_text() -> String {
    with_state(|st| st.text.clone())
}
