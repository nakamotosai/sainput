//! 最小 `ITextStoreACP` —— 让测试宿主**自己当应用侧**。
//!
//! # 为什么要有它
//!
//! 组合态上屏(preedit)需要三件东西同时在场:一个**文本存储**(应用侧)、一个
//! **文档/上下文**、以及一个**被激活的 TIP**。前两件是应用的活;最后一件我们实测发现
//! **激活必然改掉用户的全局默认输入法**(`PLAN-NEXT.md` 的 R1)。
//! 所以测试不去激活,而是自己把应用侧做出来:宿主建存储、建上下文、把焦点给它,
//! 再把线程管理器交给薄壳。这样组合态可以**完全在私有桌面上**验,一个字节都不碰用户设置。
//!
//! # 契约(照本机 SDK 的 `um/textstor.h` 与 `um/msctf.idl`,不靠记忆)
//!
//! `ITextStoreACP` IID `28888fe3-c2a0-483a-a3ea-8cb1ce51ff3d`,26 个方法,顺序:
//! AdviseSink / UnadviseSink / **RequestLock** / GetStatus / QueryInsert /
//! GetSelection / SetSelection / **GetText** / **SetText** / GetFormattedText /
//! GetEmbedded / QueryInsertEmbedded / InsertEmbedded / **InsertTextAtSelection** /
//! InsertEmbeddedAtSelection / RequestSupportedAttrs / RequestAttrsAtPosition /
//! RequestAttrsTransitioningAtPosition / FindNextAttrTransition /
//! RetrieveRequestedAttrs / **GetEndACP** / GetActiveView / GetACPFromPoint /
//! GetTextExt / GetScreenExt / GetWnd
//!
//! `ITextStoreACPSink` IID `22d44c94-a419-4542-a272-ae26093ececf`,方法顺序:
//! OnTextChange / OnSelectionChange / OnLayoutChange / OnStatusChange /
//! OnAttrsChange / **OnLockGranted** / OnStartEditTransaction / OnEndEditTransaction
//!
//! 只有**加粗**那几个真正参与组合态,其余 `E_NOTIMPL` —— 但**每一次调用都落日志**:
//! 我没写过 TSF 应用侧,靠猜不如看 msctf 到底问什么。
//!
//! 锁的走法:`RequestLock` 里调 **TSF 给我们的 sink** 的 `OnLockGranted` ——
//! 由 TSF 自己跑挂起的编辑会话(我们不需要持有 `ITfEditSession` 指针),跑完放锁。

use crate::tip::ThreadMgrVtblPrefix;
use crate::Log;
use sainput_tsf::Guid;
use std::ffi::c_void;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

type Hresult = i32;
const S_OK: Hresult = 0;
const S_FALSE: Hresult = 1;
const E_NOTIMPL: Hresult = 0x8000_4001u32 as i32;
const E_INVALIDARG: Hresult = 0x8007_0057u32 as i32;

/// 只有一个视图。
const VIEW: u32 = 1;

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct TsStatus {
    pub dynamic_flags: u32,
    pub static_flags: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct TsSelectionAcp {
    pub acp_start: i32,
    pub acp_end: i32,
    /// TS_SELECTIONSTYLE { TsActiveSelEnd ase; BOOL fInterimChar; }
    pub active_sel_end: u32,
    pub interim_char: i32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct TsRunInfo {
    pub count: u32,
    pub run_type: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct TsTextChange {
    pub acp_start: i32,
    pub acp_old_end: i32,
    pub acp_new_end: i32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

struct Inner {
    text: String,
    sel_start: i32,
    sel_end: i32,
    sink: *mut c_void,
    /// 锁计数:组合态里 TSF 会嵌套请求锁。
    lock_depth: u32,
}

/// 存储对象。组合态写进来的东西最终都落在 `inner.text` 上 —— 断言就看它。
#[repr(C)]
pub struct Store {
    vtbl: *const StoreVtbl,
    refs: AtomicU32,
    log_path: PathBuf,
    inner: Mutex<Inner>,
}

// `sink` 是 msctf 给的裸指针,生命周期由它管;我们只在宿主进程内用。
unsafe impl Send for Store {}

impl Store {
    /// `seed` = 存储里的**初始文本**。
    ///
    /// 为什么不能空着:空上下文的写事务会被 TSF 直接驳回
    /// (`TF_E_EMPTYCONTEXT`,实测 `0x80040509`)—— 真实应用里文档总有内容,
    /// 我们这个是假存储,得自己给一段。选区放在末尾(真实应用里用户的光标就在那儿)。
    fn new(log_path: PathBuf, seed: &str) -> Box<Store> {
        Box::new(Store {
            vtbl: &STORE_VTBL,
            refs: AtomicU32::new(1),
            log_path,
            inner: Mutex::new(Inner {
                text: seed.to_string(),
                sel_start: seed.chars().count() as i32,
                sel_end: seed.chars().count() as i32,
                sink: std::ptr::null_mut(),
                lock_depth: 0,
            }),
        })
    }

    fn log(&self, line: &str) {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(f, "store {line}");
            let _ = f.flush();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn store<'a>(this: *mut c_void) -> &'a Store {
    unsafe { &*(this as *const Store) }
}

// ---------------------------------------------------------------- 工具

/// `ITextStoreACPSink` 的 vtable 前缀(我们只用 OnLockGranted)。
#[repr(C)]
struct SinkVtblPrefix {
    query_interface: usize,
    add_ref: usize,
    release: usize,
    on_text_change: usize,
    on_selection_change: usize,
    on_layout_change: usize,
    on_status_change: usize,
    on_attrs_change: usize,
    /// `OnLockGranted(DWORD dwLockFlags)`
    on_lock_granted: unsafe extern "system" fn(*mut c_void, u32) -> Hresult,
}

fn utf16_to_string(buf: *const u16, len: u32) -> String {
    if buf.is_null() || len == 0 {
        return String::new();
    }
    let s = unsafe { std::slice::from_raw_parts(buf, len as usize) };
    String::from_utf16_lossy(s)
}

// ---------------------------------------------------------------- 26 个方法

/// 我们不认的 IID 里有一个特别重要:`5F20AA40-…` = **`ITfContextOwnerCompositionSink`** ——
/// TSF 在 `CreateContext` 时会问应用"你能接组合态吗?"。这是**另一个接口**,
/// 得用另一个对象和另一张 vtable 回答,不能拿存储对象的 vtable 冒充。
/// 我们**只**认这两个 IID。
///
/// 第一版对任何 IID 都回 S_OK —— 那是错的:TSF 会据此以为我们支持
/// `ITextStoreACP2` / `ITfContextOwnerCompositionServices` 之类,然后按错误的
/// vtable 偏移调我们的方法。实测的后果是 `RequestEditSession` 回
/// `TF_E_EMPTYCONTEXT`,查半天才发现是自己 QI 谎报能力。
const IID_ITEXTSTORE_ACP: Guid =
    Guid::from_parts(0x2888_8FE3, 0xC2A0, 0x483A, [0xA3, 0xEA, 0x8C, 0xB1, 0xCE, 0x51, 0xFF, 0x3D]);
const IID_IUNKNOWN: Guid =
    Guid::from_parts(0x0000_0000, 0x0000, 0x0000, [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46]);

unsafe extern "system" fn qi(this: *mut c_void, riid: *const Guid, ppv: *mut *mut c_void) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_INVALIDARG;
    }
    let want = unsafe { *riid };
    let st = store(this);
    if want == IID_ITF_CONTEXT_OWNER_COMPOSITION_SINK {
        // 组合态通知走**另一个对象**(接口不同、vtable 不同)。
        let sink = *OWNER_SINK as *mut c_void;
        st.log(&format!("QueryInterface {0} -> 组合态通知接收端", want.to_hyphen_string()));
        unsafe {
            *ppv = sink;
        }
        unsafe { os_add_ref(sink) as usize };
        return S_OK;
    }
    let ok = want == IID_IUNKNOWN || want == IID_ITEXTSTORE_ACP;
    st.log(&format!(
        "QueryInterface {} -> {}",
        want.to_hyphen_string(),
        if ok { "S_OK" } else { "E_NOINTERFACE" }
    ));
    if ok {
        unsafe {
            *ppv = this;
        }
        st.refs.fetch_add(1, Ordering::SeqCst);
        S_OK
    } else {
        unsafe { *ppv = std::ptr::null_mut() };
        0x8000_4002u32 as i32
    }
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    store(this).refs.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let n = store(this).refs.fetch_sub(1, Ordering::SeqCst) - 1;
    n
}

unsafe extern "system" fn advise_sink(this: *mut c_void, _riid: *const Guid, punk: *mut c_void, mask: u32) -> Hresult {
    let st = store(this);
    st.log(&format!("AdviseSink mask=0x{mask:x} sink={punk:p}"));
    st.lock().sink = punk;
    S_OK
}

unsafe extern "system" fn unadvise_sink(this: *mut c_void, _punk: *mut c_void) -> Hresult {
    store(this).log("UnadviseSink");
    store(this).lock().sink = std::ptr::null_mut();
    S_OK
}

unsafe extern "system" fn request_lock(this: *mut c_void, flags: u32, phr: *mut Hresult) -> Hresult {
    let st = store(this);
    st.log(&format!("RequestLock flags=0x{flags:x}"));
    {
        let mut g = st.lock();
        g.lock_depth += 1;
    }
    // 把锁交给 **TSF 给的 sink**:由它自己去跑挂起的编辑会话。
    let sink = st.lock().sink;
    let mut session_hr: Hresult = S_OK;
    if !sink.is_null() {
        let vt = unsafe { *(sink as *const *const SinkVtblPrefix) };
        session_hr = unsafe { ((*vt).on_lock_granted)(sink, flags) };
        st.log(&format!("OnLockGranted 返回 {session_hr:#x}"));
    } else {
        st.log("RequestLock:还没有 sink(msctf 没装)");
    }
    {
        let mut g = st.lock();
        g.lock_depth = g.lock_depth.saturating_sub(1);
    }
    if !phr.is_null() {
        unsafe { *phr = session_hr };
    }
    S_OK
}

unsafe extern "system" fn get_status(this: *mut c_void, status: *mut TsStatus) -> Hresult {
    store(this).log("GetStatus");
    if !status.is_null() {
        unsafe { *status = TsStatus { dynamic_flags: 0, static_flags: 0 } };
    }
    S_OK
}

unsafe extern "system" fn query_insert(
    this: *mut c_void,
    acp_test_start: i32,
    acp_test_end: i32,
    _cch: u32,
    out_start: *mut i32,
    out_end: *mut i32,
) -> Hresult {
    store(this).log(&format!("QueryInsert {acp_test_start}..{acp_test_end}"));
    if !out_start.is_null() {
        unsafe { *out_start = acp_test_start };
    }
    if !out_end.is_null() {
        unsafe { *out_end = acp_test_end };
    }
    S_OK
}

unsafe extern "system" fn get_selection(
    this: *mut c_void,
    _index: u32,
    count: u32,
    sel: *mut TsSelectionAcp,
    fetched: *mut u32,
) -> Hresult {
    let st = store(this);
    let g = st.lock();
    st.log(&format!("GetSelection count={count} sel={}..{}", g.sel_start, g.sel_end));
    if !sel.is_null() && count > 0 {
        unsafe {
            *sel = TsSelectionAcp {
                acp_start: g.sel_start,
                acp_end: g.sel_end,
                active_sel_end: 0,
                interim_char: 0,
            }
        };
    }
    if !fetched.is_null() {
        unsafe { *fetched = 1 };
    }
    S_OK
}

unsafe extern "system" fn set_selection(this: *mut c_void, count: u32, sel: *const TsSelectionAcp) -> Hresult {
    let st = store(this);
    if sel.is_null() || count == 0 {
        return E_INVALIDARG;
    }
    let s = unsafe { *sel };
    {
        let mut g = st.lock();
        g.sel_start = s.acp_start;
        g.sel_end = s.acp_end;
        st.log(&format!("SetSelection {}..{}", s.acp_start, s.acp_end));
        // 必须通知:组合态靠它知道位置。
        let sink = g.sink;
        if !sink.is_null() {
            let vt = unsafe { *(sink as *const *const SinkVtblPrefix) };
            // OnSelectionChange 就在前缀里(第 4 个槽),但为了不误调,这里只记日志。
            let _ = vt;
        }
    }
    S_OK
}

unsafe extern "system" fn get_text(
    this: *mut c_void,
    acp_start: i32,
    acp_end: i32,
    plain: *mut u16,
    plain_req: u32,
    plain_out: *mut u32,
    run_info: *mut TsRunInfo,
    run_req: u32,
    run_out: *mut u32,
    next: *mut i32,
) -> Hresult {
    let st = store(this);
    let g = st.lock();
    let chars: Vec<u16> = g.text.encode_utf16().collect();
    let start = acp_start.max(0) as usize;
    let end = if acp_end < 0 { chars.len() } else { (acp_end as usize).min(chars.len()) };
    st.log(&format!("GetText {acp_start}..{acp_end} len={}", chars.len()));
    if start > chars.len() || end < start {
        return E_INVALIDARG;
    }
    let take = (end - start).min(plain_req as usize);
    if !plain.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(chars[start..].as_ptr(), plain, take) };
    }
    if !plain_out.is_null() {
        unsafe { *plain_out = take as u32 };
    }
    if !run_info.is_null() && run_req > 0 {
        unsafe { *run_info = TsRunInfo { count: take as u32, run_type: 0 } };
    }
    if !run_out.is_null() {
        unsafe { *run_out = if run_req > 0 { 1 } else { 0 } };
    }
    if !next.is_null() {
        unsafe { *next = (start + take) as i32 };
    }
    S_OK
}

unsafe extern "system" fn set_text(
    this: *mut c_void,
    _flags: u32,
    acp_start: i32,
    acp_end: i32,
    text: *const u16,
    cch: u32,
) -> Hresult {
    let st = store(this);
    let ins = utf16_to_string(text, cch);
    {
        let mut g = st.lock();
        let chars: Vec<char> = g.text.chars().collect();
        let start = (acp_start.max(0) as usize).min(chars.len());
        let end = if acp_end < 0 { chars.len() } else { (acp_end as usize).min(chars.len()).max(start) };
        let mut out: String = chars[..start].iter().collect();
        out.push_str(&ins);
        out.extend(chars[end..].iter());
        g.text = out;
        g.sel_start = (start + ins.chars().count()) as i32;
        g.sel_end = g.sel_start;
        st.log(&format!("SetText {acp_start}..{acp_end} 插入 {ins:?} -> 现在 {:?}", g.text));
    }
    S_OK
}

unsafe extern "system" fn get_formatted_text(_this: *mut c_void, _s: i32, _e: i32, _out: *mut *mut c_void) -> Hresult {
    store(_this).log("GetFormattedText");
    E_NOTIMPL
}

unsafe extern "system" fn get_embedded(_this: *mut c_void, _pos: i32, _svc: *const Guid, _iid: *const Guid, _out: *mut *mut c_void) -> Hresult {
    store(_this).log("GetEmbedded");
    E_NOTIMPL
}

unsafe extern "system" fn query_insert_embedded(_this: *mut c_void, _svc: *const Guid, _fmt: *const c_void, insertable: *mut i32) -> Hresult {
    store(_this).log("QueryInsertEmbedded");
    if !insertable.is_null() {
        unsafe { *insertable = 0 }; // 不支持嵌入对象
    }
    S_OK
}

unsafe extern "system" fn insert_embedded(_this: *mut c_void, _f: u32, _s: i32, _e: i32, _obj: *mut c_void) -> Hresult {
    store(_this).log("InsertEmbedded");
    E_NOTIMPL
}

unsafe extern "system" fn insert_text_at_selection(
    this: *mut c_void,
    _flags: u32,
    text: *const u16,
    cch: u32,
    out_start: *mut i32,
    out_end: *mut i32,
    change: *mut TsTextChange,
) -> Hresult {
    let st = store(this);
    let ins = utf16_to_string(text, cch);
    let mut g = st.lock();
    let chars: Vec<char> = g.text.chars().collect();
    let start = (g.sel_start.max(0) as usize).min(chars.len());
    let old_end = (g.sel_end.max(0) as usize).min(chars.len()).max(start);
    let mut out: String = chars[..start].iter().collect();
    out.push_str(&ins);
    out.extend(chars[old_end..].iter());
    let new_end = start + ins.chars().count();
    g.text = out;
    g.sel_start = new_end as i32;
    g.sel_end = new_end as i32;
    st.log(&format!("InsertTextAtSelection 插入 {ins:?} -> 现在 {:?}", g.text));
    if !out_start.is_null() {
        unsafe { *out_start = start as i32 };
    }
    if !out_end.is_null() {
        unsafe { *out_end = new_end as i32 };
    }
    if !change.is_null() {
        unsafe {
            *change = TsTextChange {
                acp_start: start as i32,
                acp_old_end: old_end as i32,
                acp_new_end: new_end as i32,
            }
        };
    }
    S_OK
}

unsafe extern "system" fn insert_embedded_at_selection(_this: *mut c_void, _f: u32, _obj: *mut c_void, _s: *mut i32, _e: *mut i32, _c: *mut TsTextChange) -> Hresult {
    store(_this).log("InsertEmbeddedAtSelection");
    E_NOTIMPL
}

unsafe extern "system" fn request_supported_attrs(_this: *mut c_void, _f: u32, _n: u32, _a: *const Guid) -> Hresult {
    store(_this).log("RequestSupportedAttrs");
    E_NOTIMPL
}

unsafe extern "system" fn request_attrs_at_position(_this: *mut c_void, _p: i32, _n: u32, _a: *const Guid, _f: u32) -> Hresult {
    store(_this).log("RequestAttrsAtPosition");
    E_NOTIMPL
}

unsafe extern "system" fn request_attrs_transitioning(_this: *mut c_void, _p: i32, _n: u32, _a: *const Guid, _f: u32) -> Hresult {
    store(_this).log("RequestAttrsTransitioningAtPosition");
    E_NOTIMPL
}

unsafe extern "system" fn find_next_attr_transition(
    _this: *mut c_void, _start: i32, _halt: i32, _n: u32, _a: *const Guid, _f: u32,
    _next: *mut i32, found: *mut i32, _offset: *mut i32,
) -> Hresult {
    store(_this).log("FindNextAttrTransition");
    if !found.is_null() {
        unsafe { *found = 0 };
    }
    E_NOTIMPL
}

unsafe extern "system" fn retrieve_requested_attrs(_this: *mut c_void, _n: u32, _vals: *mut c_void, fetched: *mut u32) -> Hresult {
    store(_this).log("RetrieveRequestedAttrs");
    if !fetched.is_null() {
        unsafe { *fetched = 0 };
    }
    E_NOTIMPL
}

unsafe extern "system" fn get_end_acp(this: *mut c_void, out: *mut i32) -> Hresult {
    let st = store(this);
    let g = st.lock();
    let n = g.text.encode_utf16().count() as i32;
    st.log(&format!("GetEndACP -> {n}"));
    if !out.is_null() {
        unsafe { *out = n };
    }
    S_OK
}

unsafe extern "system" fn get_active_view(this: *mut c_void, view: *mut u32) -> Hresult {
    store(this).log("GetActiveView");
    if !view.is_null() {
        unsafe { *view = VIEW };
    }
    S_OK
}

unsafe extern "system" fn get_acp_from_point(_this: *mut c_void, _v: u32, _pt: *const Point, _f: u32, acp: *mut i32) -> Hresult {
    store(_this).log("GetACPFromPoint");
    if !acp.is_null() {
        unsafe { *acp = 0 };
    }
    E_NOTIMPL
}

unsafe extern "system" fn get_text_ext(this: *mut c_void, _v: u32, _s: i32, _e: i32, rc: *mut Rect, clipped: *mut i32) -> Hresult {
    store(this).log("GetTextExt");
    if !rc.is_null() {
        unsafe { *rc = Rect { left: 0, top: 0, right: 100, bottom: 20 } };
    }
    if !clipped.is_null() {
        unsafe { *clipped = 0 };
    }
    S_OK
}

unsafe extern "system" fn get_screen_ext(this: *mut c_void, _v: u32, rc: *mut Rect) -> Hresult {
    store(this).log("GetScreenExt");
    if !rc.is_null() {
        unsafe { *rc = Rect { left: 0, top: 0, right: 100, bottom: 20 } };
    }
    S_OK
}

unsafe extern "system" fn get_wnd(this: *mut c_void, _v: u32, hwnd: *mut isize) -> Hresult {
    store(this).log("GetWnd");
    if !hwnd.is_null() {
        unsafe { *hwnd = crate::WINDOW.with(|w| w.get()) };
    }
    S_OK
}

// ---------------------------------------------------------------- 组合态通知(应用侧)

#[repr(C)]
struct UnknownVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> Hresult,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

const IID_ITF_CONTEXT_OWNER_COMPOSITION_SINK: Guid =
    Guid::from_parts(0x5F20_AA40, 0xB57A, 0x4F34, [0x96, 0xAB, 0x35, 0x76, 0xF3, 0x77, 0xCC, 0x79]);

#[repr(C)]
struct OwnerCompositionSinkVtbl {
    base: UnknownVtbl,
    on_start: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut i32) -> Hresult,
    on_update: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> Hresult,
    on_end: unsafe extern "system" fn(*mut c_void, *mut c_void) -> Hresult,
}

#[repr(C)]
struct OwnerCompositionSink {
    vtbl: *const OwnerCompositionSinkVtbl,
    refs: AtomicU32,
    log_path: PathBuf,
}

fn sink_log(store: &OwnerCompositionSink, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&store.log_path) {
        let _ = writeln!(f, "store {line}");
        let _ = f.flush();
    }
}

unsafe extern "system" fn os_qi(this: *mut c_void, riid: *const Guid, ppv: *mut *mut c_void) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_INVALIDARG;
    }
    let want = unsafe { *riid };
    if want == IID_IUNKNOWN || want == IID_ITF_CONTEXT_OWNER_COMPOSITION_SINK {
        unsafe { *ppv = this };
        unsafe { os_add_ref(this) };
        S_OK
    } else {
        unsafe { *ppv = std::ptr::null_mut() };
        0x8000_4002u32 as i32
    }
}

unsafe extern "system" fn os_add_ref(this: *mut c_void) -> u32 {
    let s = unsafe { &*(this as *const OwnerCompositionSink) };
    s.refs.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn os_release(this: *mut c_void) -> u32 {
    let s = unsafe { &*(this as *const OwnerCompositionSink) };
    s.refs.fetch_sub(1, Ordering::SeqCst) - 1
}

unsafe extern "system" fn os_on_start(this: *mut c_void, comp: *mut c_void, ok: *mut i32) -> Hresult {
    let s = unsafe { &*(this as *const OwnerCompositionSink) };
    sink_log(s, &format!("OnStartComposition view={comp:p} -> 接受"));
    if !ok.is_null() {
        unsafe { *ok = 1 }; // 接受这个组合态
    }
    S_OK
}

unsafe extern "system" fn os_on_update(this: *mut c_void, comp: *mut c_void, _range: *mut c_void) -> Hresult {
    let s = unsafe { &*(this as *const OwnerCompositionSink) };
    sink_log(s, &format!("OnUpdateComposition view={comp:p}"));
    S_OK
}

unsafe extern "system" fn os_on_end(this: *mut c_void, comp: *mut c_void) -> Hresult {
    let s = unsafe { &*(this as *const OwnerCompositionSink) };
    sink_log(s, &format!("OnEndComposition view={comp:p} —— 定稿落进文档"));
    S_OK
}

static OWNER_SINK_VTBL: OwnerCompositionSinkVtbl = OwnerCompositionSinkVtbl {
    base: UnknownVtbl {
        query_interface: os_qi,
        add_ref: os_add_ref,
        release: os_release,
    },
    on_start: os_on_start,
    on_update: os_on_update,
    on_end: os_on_end,
};

static OWNER_SINK: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
    let s = Box::new(OwnerCompositionSink {
        vtbl: &OWNER_SINK_VTBL,
        refs: AtomicU32::new(1),
        // 日志路径在第一次 QI 前由 create_context 填不进去,这里用同样的默认路径规则。
        log_path: std::env::var("TEMP").map(std::path::PathBuf::from).unwrap_or_default(),
    });
    Box::into_raw(s) as usize
});

static STORE_VTBL: StoreVtbl = StoreVtbl {
    query_interface: qi,
    add_ref,
    release,
    advise_sink,
    unadvise_sink,
    request_lock,
    get_status,
    query_insert,
    get_selection,
    set_selection,
    get_text,
    set_text,
    get_formatted_text,
    get_embedded,
    query_insert_embedded,
    insert_embedded,
    insert_text_at_selection,
    insert_embedded_at_selection,
    request_supported_attrs,
    request_attrs_at_position,
    request_attrs_transitioning,
    find_next_attr_transition,
    retrieve_requested_attrs,
    get_end_acp,
    get_active_view,
    get_acp_from_point,
    get_text_ext,
    get_screen_ext,
    get_wnd,
};

#[repr(C)]
struct StoreVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> Hresult,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    advise_sink: unsafe extern "system" fn(*mut c_void, *const Guid, *mut c_void, u32) -> Hresult,
    unadvise_sink: unsafe extern "system" fn(*mut c_void, *mut c_void) -> Hresult,
    request_lock: unsafe extern "system" fn(*mut c_void, u32, *mut Hresult) -> Hresult,
    get_status: unsafe extern "system" fn(*mut c_void, *mut TsStatus) -> Hresult,
    query_insert: unsafe extern "system" fn(*mut c_void, i32, i32, u32, *mut i32, *mut i32) -> Hresult,
    get_selection: unsafe extern "system" fn(*mut c_void, u32, u32, *mut TsSelectionAcp, *mut u32) -> Hresult,
    set_selection: unsafe extern "system" fn(*mut c_void, u32, *const TsSelectionAcp) -> Hresult,
    get_text: unsafe extern "system" fn(
        *mut c_void, i32, i32, *mut u16, u32, *mut u32, *mut TsRunInfo, u32, *mut u32, *mut i32,
    ) -> Hresult,
    set_text: unsafe extern "system" fn(*mut c_void, u32, i32, i32, *const u16, u32) -> Hresult,
    get_formatted_text: unsafe extern "system" fn(*mut c_void, i32, i32, *mut *mut c_void) -> Hresult,
    get_embedded: unsafe extern "system" fn(*mut c_void, i32, *const Guid, *const Guid, *mut *mut c_void) -> Hresult,
    query_insert_embedded: unsafe extern "system" fn(*mut c_void, *const Guid, *const c_void, *mut i32) -> Hresult,
    insert_embedded: unsafe extern "system" fn(*mut c_void, u32, i32, i32, *mut c_void) -> Hresult,
    insert_text_at_selection: unsafe extern "system" fn(*mut c_void, u32, *const u16, u32, *mut i32, *mut i32, *mut TsTextChange) -> Hresult,
    insert_embedded_at_selection: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut i32, *mut i32, *mut TsTextChange) -> Hresult,
    request_supported_attrs: unsafe extern "system" fn(*mut c_void, u32, u32, *const Guid) -> Hresult,
    request_attrs_at_position: unsafe extern "system" fn(*mut c_void, i32, u32, *const Guid, u32) -> Hresult,
    request_attrs_transitioning: unsafe extern "system" fn(*mut c_void, i32, u32, *const Guid, u32) -> Hresult,
    find_next_attr_transition: unsafe extern "system" fn(*mut c_void, i32, i32, u32, *const Guid, u32, *mut i32, *mut i32, *mut i32) -> Hresult,
    retrieve_requested_attrs: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut u32) -> Hresult,
    get_end_acp: unsafe extern "system" fn(*mut c_void, *mut i32) -> Hresult,
    get_active_view: unsafe extern "system" fn(*mut c_void, *mut u32) -> Hresult,
    get_acp_from_point: unsafe extern "system" fn(*mut c_void, u32, *const Point, u32, *mut i32) -> Hresult,
    get_text_ext: unsafe extern "system" fn(*mut c_void, u32, i32, i32, *mut Rect, *mut i32) -> Hresult,
    get_screen_ext: unsafe extern "system" fn(*mut c_void, u32, *mut Rect) -> Hresult,
    get_wnd: unsafe extern "system" fn(*mut c_void, u32, *mut isize) -> Hresult,
}

// ---------------------------------------------------------------- 建上下文

/// 向 TSF 报一次**初始状态**(文本变了 + 选区变了)。
///
/// `ITextStoreACPSink::OnTextChange(dwFlags=0, pChange=NULL)` = "内容全变了";
/// `OnSelectionChange()` = "选区变了"。这两条是**应用**告诉 TSF "我这里有什么"的
/// 唯一途径,不报的话 msctf 眼里这个上下文就是空的。
/// 从上下文 QI `ITextStoreACPSink`(应用那条"告诉 TSF 我变了"的通道)。
const IID_ITEXTSTORE_ACP_SINK: Guid =
    Guid::from_parts(0x22D4_4C94, 0xA419, 0x4542, [0xA2, 0x72, 0xAE, 0x26, 0x09, 0x3E, 0xCE, 0xCF]);

unsafe fn fetch_acp_sink(ctx: *mut c_void, log: &Log) -> *mut c_void {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let cvt = unsafe { *(ctx as *const *const UnknownVtbl) };
    let mut out: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { ((*cvt).query_interface)(ctx, &IID_ITEXTSTORE_ACP_SINK, &mut out) };
    log.line(&format!(
        "store 自取 ITextStoreACPSink(从上下文) hr={hr:#x} sink={out:p}"
    ));
    if hr == 0 { out } else { std::ptr::null_mut() }
}

unsafe fn notify_initial(store_ptr: *mut c_void, ctx: *mut c_void, log: &Log) {
    // 只用到前两个方法,单独声明一张**带类型**的 vtable(前缀里的那两个字段是 usize)。
    #[repr(C)]
    struct CallVtbl {
        query_interface: usize,
        add_ref: usize,
        release: usize,
        on_text_change: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> Hresult,
        on_selection_change: unsafe extern "system" fn(*mut c_void) -> Hresult,
    }
    let st = store(store_ptr);
    let mut sink = st.lock().sink;
    if sink.is_null() {
        // msctf 没有主动把 sink 递给我们(实测:整轮都不调 `AdviseSink`)。
        // 那就照 Chromium 的做法**自己从上下文要**:`ITfContext::QueryInterface(
        // IID_ITextStoreACPSink)` —— 这是应用拿到自己那条通知通道的正路。
        sink = fetch_acp_sink(ctx, log);
        if sink.is_null() {
            log.line("store 报初始状态:拿不到 ITextStoreACPSink,跳过");
            return;
        }
        st.lock().sink = sink;
    }
    let vt = unsafe { *(sink as *const *const CallVtbl) };
    let hr_t = unsafe { ((*vt).on_text_change)(sink, 0, std::ptr::null_mut()) };
    let hr_s = unsafe { ((*vt).on_selection_change)(sink) };
    log.line(&format!(
        "store 报初始状态 OnTextChange hr={hr_t:#x} OnSelectionChange hr={hr_s:#x}"
    ));
}


/// 只把本进程的 TSF 线程管理器**拉起来**(`TF_CreateThreadMgr` + `Activate`),
/// 不建任何存储/文档。返回线程管理器。
///
/// 为什么需要它:真控件(RichEdit/写字板那一族)是在**拿到焦点那一刻**向 TSF
/// 注册自己的文本存储的。要是进程里 TSF 还没 `Activate` 就去抢焦点,那一刻就
/// 白白过去,`GetFocus` 会一直空 —— 实测(2026-10-31)就是这么空的。
pub unsafe fn init_thread_mgr(log: &Log) -> Option<*mut c_void> {
    let Some(tf_create) = (unsafe { crate::tip::resolve_msctf_create() }) else {
        log.line("tfinit 拿不到 TF_CreateThreadMgr");
        return None;
    };
    let mut tm: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { tf_create(&mut tm) };
    if hr != 0 || tm.is_null() {
        log.line(&format!("tfinit TF_CreateThreadMgr hr={hr:#x}"));
        return None;
    }
    let tvt = unsafe { *(tm as *const *const ThreadMgrVtblPrefix) };
    let mut tid: u32 = 0;
    let hr = unsafe { ((*tvt).activate)(tm, &mut tid) };
    log.line(&format!("tfinit ITfThreadMgr::Activate hr={hr:#x} tid={tid}"));
    Some(tm)
}

/// 拿"当前焦点上下文"来探(不用假存储那条路:真实会话里标准控件的 TSF 兼容存储
/// 本来就会出现,那才是生产形态)。
pub unsafe fn probe_focus_context(tm: *mut c_void, log: &Log) -> bool {
    if tm.is_null() {
        log.line("probe 焦点上下文:没有线程管理器");
        return false;
    }
    let mut dim: *mut c_void = std::ptr::null_mut();
    let tvt2 = unsafe { *(tm as *const *const crate::tip::ThreadMgrVtblPrefix) };
    let hr = unsafe { ((*tvt2).get_focus)(tm, &mut dim) };
    log.line(&format!("probe GetFocus hr={hr:#x} dim={dim:p}"));
    if hr != 0 || dim.is_null() {
        return false;
    }
    let mut ctx: *mut c_void = std::ptr::null_mut();
    let dvt = unsafe { *(dim as *const *const DocMgrVtbl) };
    let hr = unsafe { ((*dvt).get_top)(dim, &mut ctx) };
    log.line(&format!("probe GetTop hr={hr:#x} ctx={ctx:p}"));
    if hr != 0 || ctx.is_null() {
        return false;
    }
    unsafe { probe_context_interfaces(ctx, log) };
    true
}

/// 逐个 QI 我们关心的几个 IID,把结果记下来。
///
/// 为什么值得单独做一遍:TSF 拒绝写事务时只说一句 `TF_E_EMPTYCONTEXT`,
/// 而"这个上下文支不支持组合态"是它的**能力**问题 —— 直接问比猜快。
pub unsafe fn probe_context_interfaces(ctx: *mut c_void, log: &Log) {
    const PROBES: [(u32, u16, u16, [u8; 8], &str); 5] = [
        (0xD40C_8AAE, 0xAC92, 0x4FC7, [0x9A, 0x11, 0x0E, 0xE0, 0xE2, 0x3A, 0xA3, 0x9B], "ITfContextComposition"),
        (0x8646_2810, 0x593B, 0x4916, [0x97, 0x64, 0x19, 0xC0, 0x8E, 0x9C, 0xE1, 0x10], "ITfContextOwnerCompositionServices"),
        (0x7DCF_57AC, 0x18AD, 0x438B, [0x82, 0x4D, 0x97, 0x9B, 0xFF, 0xB7, 0x4B, 0x7C], "ITfCompartmentMgr"),
        (0xAA80_E7F4, 0x2021, 0x11D2, [0x93, 0xE0, 0x00, 0x60, 0xB0, 0x67, 0xB8, 0x6E], "ITfRange(乱试)"),
        (0xAA80_E801, 0x2021, 0x11D2, [0x93, 0xE0, 0x00, 0x60, 0xB0, 0x67, 0xB8, 0x6E], "ITfThreadMgr(乱试)"),
    ];
    let cvt = unsafe { *(ctx as *const *const UnknownVtbl) };
    for (d1, d2, d3, d4, name) in PROBES {
        let iid = Guid::from_parts(d1, d2, d3, d4);
        let mut out: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { ((*cvt).query_interface)(ctx, &iid, &mut out) };
        log.line(&format!("store ctx QI {name} -> {hr:#x} out={out:p}"));
    }
}

#[repr(C)]
struct DocMgrVtbl {
    query_interface: usize,
    add_ref: usize,
    release: usize,
    create_context: unsafe extern "system" fn(*mut c_void, u32, u32, *mut c_void, *mut *mut c_void, *mut u32) -> Hresult,
    duplicate_context: usize,
    pop: usize,
    push: unsafe extern "system" fn(*mut c_void, *mut c_void) -> Hresult,
    get_top: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> Hresult,
}

type TfCreateThreadMgrFn = unsafe extern "system" fn(*mut *mut c_void) -> Hresult;

/// 建存储 + 文档 + 上下文,把焦点给它。返回 (存储对象, 文本快照句柄所需的一切)。
pub unsafe fn create_context(
    log: &Log,
    log_path: PathBuf,
    seed: &str,
) -> Option<(*mut c_void, *mut c_void)> {
    let Some(tf_create) = (unsafe { crate::tip::resolve_msctf_create() }) else {
        log.line("store 拿不到 TF_CreateThreadMgr");
        return None;
    };
    let mut tm: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { tf_create(&mut tm) };
    log.line(&format!("store TF_CreateThreadMgr hr={hr:#x}"));
    if hr != 0 || tm.is_null() {
        return None;
    }
    let tvt = unsafe { *(tm as *const *const ThreadMgrVtblPrefix) };
    let mut tid: u32 = 0;
    let hr = unsafe { ((*tvt).activate)(tm, &mut tid) };
    log.line(&format!("store ITfThreadMgr::Activate hr={hr:#x} tid={tid}"));

    let mut dim: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { ((*tvt).create_document_mgr)(tm, &mut dim) };
    log.line(&format!("store CreateDocumentMgr hr={hr:#x} dim={dim:p}"));
    if hr != 0 || dim.is_null() {
        return None;
    }

    let store = Store::new(log_path, seed);
    let store_ptr = Box::into_raw(store) as *mut c_void;
    let dvt = unsafe { *(dim as *const *const DocMgrVtbl) };
    let mut ctx: *mut c_void = std::ptr::null_mut();
    let mut ec: u32 = 0;
    let hr = unsafe {
        ((*dvt).create_context)(dim, tid, 0, store_ptr, &mut ctx, &mut ec)
    };
    log.line(&format!(
        "store CreateContext(tid={tid}, store) hr={hr:#x} ctx={ctx:p} ec={ec}"
    ));
    if hr != 0 || ctx.is_null() {
        return None;
    }
    let hr = unsafe { ((*dvt).push)(dim, ctx) };
    log.line(&format!("store Push(ctx) hr={hr:#x}"));
    let mut top: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { ((*dvt).get_top)(dim, &mut top) };
    log.line(&format!("store GetTop hr={hr:#x} top={top:p}"));
    let hr = unsafe { ((*tvt).set_focus)(tm, dim) };
    log.line(&format!("store SetFocus(dim) hr={hr:#x}"));

    // **把上下文标成"非空"**。
    //
    // 实测(2026-10-31):不标的话,薄壳请求写事务会被驳回 `TF_E_EMPTYCONTEXT`
    // (0x80040509)。文档里只说"context is empty",真正的原因是 TSF 有个隔间
    // `GUID_COMPARTMENT_EMPTYCONTEXT`(d7487dbf-804e-41c5-894d-ad96fd4eea13),
    // 应用不把它设成 FALSE,msctf 就当这个上下文没有内容 —— 真应用的文档里总有字,
    // 我们这个是假存储,得自己声明。
    // 用 `GetTop` 拿到的那个上下文指针(`CreateContext` 交回来的那个实测不是
    // 能直接 QI 的接口指针 —— 拿它当 vtable 解引用会访问违例)。
    // **不设** `GUID_COMPARTMENT_EMPTYCONTEXT`。
    //
    // 2026-10-31 更正:这里原先写着"不设 FALSE 就会被判定空上下文",那是错的。
    // 证据两条:(a) 本机 SDK 的 `msctf.idl` 里 `TF_E_EMPTYCONTEXT`(0x80040509) 只是
    // 一个返回码,与这个隔间没有文档上的因果关系;(b) Chromium(公认能跑的 TSF 宿主)
    // 对**正常**上下文从不碰这个隔间 —— 它是 Win11 才有的"空存储"支持,只在**没有
    // 文本存储**的 null store 上设成 `1`(那种上下文确实做不了组合态)。
    // 实测:设 0 / 不设,对 `ITfContextComposition` 的可得性**没有影响**(都是
    // `E_NOINTERFACE`),对写事务也没有 —— 所以这条弯路到此为止,不再设。
    //
    // 合成存储**真正**的上限见 HANDOFF §7 与 `PLAN-NEXT.md` 的 T1.4 行:
    // msctf 建上下文时会向我们 QI 两个不在公开 SDK 里的接口,之后既不发
    // `ITextStoreACPSink`、也不给组合态 —— 自搓的 `ITextStoreACP` 在它眼里不是
    // 一等公民。要在真应用里验,不能在假存储上磨。
    log.line("store 不设 EMPTYCONTEXT 隔间(对齐 Chromium;实测与组合态无关)");
    // 诊断:这个**上下文对象**到底支持哪些接口 —— 它决定 TSF 认不认这个上下文能接组合态。
    unsafe { probe_context_interfaces(top, log) };
    // 2026-10-31:我们**从没向 TSF 报过**任何文本/选区变化。真应用每改一次内容
    // 都会经 sink 报一次,msctf 的"这个上下文有没有内容"很可能就看这个 ——
    // 一回事不报,它就一直当这个上下文是空的(写事务回 `TF_E_EMPTYCONTEXT`)。
    notify_initial(store_ptr, top, log);
    log.line("store 上下文就绪(已向 TSF 报初始文本/选区)");
    Some((store_ptr, tm))
}