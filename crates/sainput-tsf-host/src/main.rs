//! sainput-tsf-host —— TSF 上屏回归用的**目标应用**壳。
//!
//! 为什么需要它:组合态上屏(preedit)要有一个"应用侧"的接收方才能断言 —— 它得有自己的
//! 文本存储、自己的焦点、一个能读回内容的控件。拿 Word / 飞书 这类真实应用天天跑自动化
//! 回归既不稳也不该动它们,所以这里做一个最小的真实 Windows 应用:窗口 + 经典 `EDIT`
//! 控件 + 消息泵。它跟真实应用走的是同一条 IMM32/TSF 兼容路径。
//!
//! 它**故意**跑在私有桌面上(见 `tools/hdesk.py`):窗口对用户不可见、抢不到焦点。
//!
//! ```text
//! sainput-tsf-host --life 5000 --log <path> [--selftest "文本"] [--dump-every 250]
//! ```
//!
//! `--selftest` 在自己的 EDIT 上跑一遍 `WM_CHAR` 往返(一字不差才算 PASS),用来证明
//! 「壳本身是活的」—— 这一步不依赖输入法与 TSF。TSF 场景由外部驱动:
//!
//! ```text
//! python tools/hdesk.py readtext --pid <host pid>     # 跨进程读 EDIT 内容(WM_GETTEXT)
//! ```
//!
//! 运行时输出一律 ASCII(本机控制台 CP936,中文会串码);日志文件是 UTF-8。

mod profile;
mod store;
mod tip;

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

// ---------------- Win32 声明(与仓库既有风格一致:手写 extern,不引 windows crate)----------------

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

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Msg {
    hwnd: isize,
    message: u32,
    w_param: usize,
    l_param: isize,
    time: u32,
    pt_x: i32,
    pt_y: i32,
}

#[link(name = "ole32")]
extern "system" {
    /// TSF 要求跑在 **COM 初始化过的**线程上(Chromium 那条 UI 线程就是)。
    /// 这条我们一直缺:没它就靠 msctf 内部兜底,某些上下文服务(组合态就是其一)
    /// 会直接以 `E_NOINTERFACE` 收场。
    fn CoInitializeEx(pv: *mut std::ffi::c_void, dw: u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> isize;
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(c: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex: u32,
        cls: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: isize,
        menu: isize,
        inst: isize,
        param: isize,
    ) -> isize;
    fn DestroyWindow(h: isize) -> i32;
    fn DefWindowProcW(h: isize, m: u32, w: usize, l: isize) -> isize;
    fn ShowWindow(h: isize, cmd: i32) -> i32;
    fn SetFocus(h: isize) -> isize;

    /// 在私有桌面上把自己顶到前台:引擎按"它的桌面上的前台窗口"决定往哪儿打字,
    /// 这张桌面上只有我们,所以这里失败也没关系,记进日志就行。
    fn SetForegroundWindow(h: isize) -> i32;
    fn GetFocus() -> isize;
    fn GetModuleHandleW(name: *const u16) -> isize;
    fn PeekMessageW(msg: *mut Msg, h: isize, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(m: *const Msg) -> i32;
    fn DispatchMessageW(m: *const Msg) -> isize;
    fn PostMessageW(h: isize, msg: u32, wp: usize, lp: isize) -> i32;
    fn SendMessageW(h: isize, msg: u32, wp: usize, lp: isize) -> isize;
    fn PostQuitMessage(code: i32);
}

// ---------------- 常量 ----------------

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const ES_MULTILINE: u32 = 0x0004;
const ES_AUTOVSCROLL: u32 = 0x0040;
const SW_SHOW: i32 = 5;
const PM_REMOVE: u32 = 1;

const WM_DESTROY: u32 = 0x0002;
const WM_CHAR: u32 = 0x0102;
const WM_GETTEXT: u32 = 0x000D;
const WM_GETTEXTLENGTH: u32 = 0x000E;
const WM_SETFOCUS: u32 = 0x0007;

const CLASS_NAME: &str = "SainputTsfHost";

static DESTROYED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// 主窗口句柄:文本存储的 `GetWnd` 要用它(没有窗口时是 0)。
    pub(crate) static WINDOW: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

/// 主窗口在 `WM_SETFOCUS` 里要把焦点交给 EDIT。
///
/// 原先用 `SetWindowLongPtrW(GWLP_USERDATA)` 存句柄,结果**编 32 位时链不上** ——
/// `SetWindowLongPtrW` 在 32 位 Windows 上不是导出符号(那只是 64 位的名字,
/// 32 位要靠 `SetWindowLongW` 宏)。这个壳只有一个窗口,用个静态变量最省事,
/// 也顺手把这个位数差异绕过去了。
static EDIT_HANDLE: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn wnd_proc(h: isize, m: u32, w: usize, l: isize) -> isize {
    match m {
        WM_DESTROY => {
            DESTROYED.store(true, Ordering::SeqCst);
            PostQuitMessage(0);
            0
        }
        WM_SETFOCUS => {
            // 焦点给到 EDIT:TSF 的文本存储是跟着焦点窗口建立的。
            let edit = EDIT_HANDLE.load(Ordering::SeqCst);
            if edit != 0 {
                unsafe { SetFocus(edit) };
            }
            0
        }
        _ => unsafe { DefWindowProcW(h, m, w, l) },
    }
}


// ---------------- 工具 ----------------

/// `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX` -> `Guid`(诊断用,不追求宽容)。
fn parse_guid(s: &str) -> Option<sainput_tsf::Guid> {
    let t = s.trim().trim_matches(|c| c == '{' || c == '}');
    let parts: Vec<&str> = t.split('-').collect();
    if parts.len() != 5 || parts[0].len() != 8 || parts[3].len() != 4 {
        return None;
    }
    let d1 = u32::from_str_radix(parts[0], 16).ok()?;
    let d2 = u16::from_str_radix(parts[1], 16).ok()?;
    let d3 = u16::from_str_radix(parts[2], 16).ok()?;
    let mut d4 = [0u8; 8];
    let bytes = format!("{}{}", parts[3], parts[4]);
    if bytes.len() != 16 {
        return None;
    }
    for i in 0..8 {
        d4[i] = u8::from_str_radix(&bytes[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(sainput_tsf::Guid::from_parts(d1, d2, d3, d4))
}

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 读回 EDIT 内容(WM_GETTEXT 让系统做跨进程/跨控件族的字符串搬运)。
fn read_edit(edit: isize) -> String {
    unsafe {
        let len = SendMessageW(edit, WM_GETTEXTLENGTH, 0, 0) as usize;
        let mut buf = vec![0u16; len + 1];
        let got = SendMessageW(edit, WM_GETTEXT, buf.len(), buf.as_mut_ptr() as isize) as usize;
        buf.truncate(got);
        String::from_utf16_lossy(&buf)
    }
}

pub(crate) fn pump(trace: Option<&Log>, translate: bool) {
    const WM_KEYDOWN: u32 = 0x0100;
    const WM_KEYUP: u32 = 0x0101;
    let mut msg = Msg::default();
    unsafe {
        while PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) != 0 {
            if let Some(lg) = trace {
                match msg.message {
                    WM_CHAR | WM_KEYDOWN | WM_KEYUP => lg.line(&format!(
                        "  msg 0x{:04x} wparam=0x{:x} lparam=0x{:x} -> edit 0x{:x}",
                        msg.message, msg.w_param, msg.l_param, msg.l_param
                    )),
                    _ => {}
                }
            }
            if translate {
                TranslateMessage(&msg);
            }
            DispatchMessageW(&msg);
        }
    }
}

pub(crate) struct Log {
    /// 逐条消息落盘开了吗(`--trace`)。`tip::run` 里的保持期也要能记。
    pub(crate) tracing: bool,
    path: PathBuf,
    start: Instant,
}

impl Log {
    pub(crate) fn line(&self, s: &str) {
        let ms = self.start.elapsed().as_millis();
        let mut f = match OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(f) => f,
            Err(_) => return,
        };
        let _ = writeln!(f, "[{ms:>6}ms] {s}");
        let _ = f.flush();
    }
}

struct Args {
    life_ms: u64,
    log: PathBuf,
    selftest: Option<String>,
    selftest_file: Option<PathBuf>,
    dump_every: u64,
    title: String,
    control: String,
    /// 装一遍 TSF 薄壳并走完整条 COM 激活链(`--load-tip <dll>`)。
    load_tip: Option<String>,
    /// 薄壳激活后保持多久(毫秒):管道服务端只在这段时间里可连(T1.5 用)。
    tip_hold: u64,
    /// 文本控件的类名(`--control edit|rich50`)。T4.5 要在**不同控件类**上量
    /// "退格吃哪种消息",所以靶子得能换。
    /// `--report-profile`:只当"旁观者",把默认输入法与当前输入法打出来(T1.1 用)。
    report_profile: bool,
    /// `--hide-window`:建窗口但**不显示**(真实会话里跑 T1.1 用:一个像素都不露)。
    hide_window: bool,
    /// `--no-window`:只做非 GUI 的活(枚举/旁观/装薄壳),一个窗口都不建。
    /// 排障时要在**真实会话**里跑又不想打扰用户,就靠它。
    no_window: bool,
    /// `--activate-legacy`:`ActivateLanguageProfile`(老 API)那条路的 T1.1 复测。
    activate_legacy: bool,
    /// `--set-default <CLSID> <PROFILE>`:把 zh-CN 的默认输入法设回去(撤销我们自己的激活)。
    set_default: Option<(String, String)>,
    /// `--tfstore`:自建文本存储 + 文档 + 上下文(T1.4 的组合态前提,零副作用)。
    tfstore: bool,
    /// 假存储里的初始文本(空上下文会被 TSF 判定 `TF_E_EMPTYCONTEXT` 驳回写事务)。
    store_seed: String,
    /// `--probe-ctx`:探一探**当前焦点上下文**支持哪些 TSF 接口(不用假存储)。
    probe_ctx: bool,
    /// `--tfinit`:建窗口**之前**就把本进程的 TSF 拉起来(`Activate`)。
    /// 真控件是在拿焦点那一刻注册文本存储的,顺序错了它就不注册(实测)。
    tfinit: bool,
    /// `--enum-profiles`:把 msctf 眼里的输入法列出来(T1.1 排障用)。
    enum_profiles: bool,
    /// `--activate-hold <ms>`:激活后停这么久再做别的(T1.4 实验窗口)。
    activate_hold: u64,
    /// `--activate-profile`:在本进程里按 `TF_IPPMF_FORPROCESS` 激活我们的 TIP(T1.1 用)。
    activate_profile: bool,
    /// 不调 `TranslateMessage`(`--no-translate`):用来把「posted 的 WM_KEYDOWN 到底
    /// 有没有被控件处理」和「TranslateMessage 生成的那条 WM_CHAR(0x08)」分开。
    no_translate: bool,
    /// 逐条消息落盘(`--trace`):哪个字符/按键什么时候真的到了窗口 —— 排查
    /// 「上屏顺序/丢字」这类问题时,只有这条记录是现场。
    trace: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        life_ms: 5000,
        log: PathBuf::from("tsf-host.log"),
        selftest: None,
        selftest_file: None,
        dump_every: 250,
        title: "sainput tsf host".into(),
        load_tip: None,
        tip_hold: 0,
        control: "edit".into(),
        trace: false,
        no_translate: false,
        report_profile: false,
        enum_profiles: false,
        tfstore: false,
        probe_ctx: false,
        tfinit: false,
        store_seed: "这是窗口里原有的文本。".into(),
        set_default: None,
        activate_legacy: false,
        no_window: false,
        hide_window: false,
        activate_profile: false,
        activate_hold: 0,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        match k.as_str() {
            "--life" => a.life_ms = it.next().and_then(|v| v.parse().ok()).unwrap_or(5000),
            "--log" => a.log = PathBuf::from(it.next().unwrap_or_default()),
            "--selftest" => a.selftest = it.next(),
            // 非 ASCII 的测试文本走文件:命令行要过好几层编码,文件是 UTF-8 就稳。
            "--selftest-file" => a.selftest_file = it.next().map(PathBuf::from),
            "--dump-every" => a.dump_every = it.next().and_then(|v| v.parse().ok()).unwrap_or(250),
            "--title" => a.title = it.next().unwrap_or(a.title),
            "--load-tip" => a.load_tip = it.next(),
            "--tip-hold" => a.tip_hold = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--control" => a.control = it.next().unwrap_or(a.control),
            "--trace" => a.trace = true,
            "--no-translate" => a.no_translate = true,
            "--report-profile" => a.report_profile = true,
            "--enum-profiles" => a.enum_profiles = true,
            "--tfstore" => a.tfstore = true,
            "--probe-ctx" => a.probe_ctx = true,
            "--tfinit" => a.tfinit = true,
            "--store-seed" => a.store_seed = it.next().unwrap_or(a.store_seed),
            "--activate-legacy" => a.activate_legacy = true,
            "--set-default" => {
                let c = it.next().unwrap_or_default();
                let p = it.next().unwrap_or_default();
                a.set_default = Some((c, p));
            }
            "--no-window" => a.no_window = true,
            "--hide-window" => a.hide_window = true,
            "--activate-profile" => a.activate_profile = true,
            "--activate-hold" => a.activate_hold = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            other => eprintln!("[host] unknown arg: {other}"),
        }
    }
    a
}

fn main() {
    // COM 先初始化:TSF 的一切(上下文服务、组合态、sink 编组)都建立在它之上。
    // `RPC_E_CHANGED_MODE`(0x80010106)说明这条线程早就初始化过,不算错。
    let _co_hr = unsafe { CoInitializeEx(std::ptr::null_mut(), 2 /* COINIT_APARTMENTTHREADED */) };
    let mut a = parse_args();
    let start = Instant::now();
    let log = Log { path: a.log.clone(), start, tracing: a.trace };
    log.line(&format!("host start pid={} log={}", std::process::id(), a.log.display()));

    if a.no_window {
        // 无窗口模式:不建类、不建窗口 —— 一个像素都不出现在桌面上。
        log.line("no-window 模式:跳过窗口创建");
        unsafe {
            if let Some((c, p)) = a.set_default.clone() {
                let (c, p) = (parse_guid(&c), parse_guid(&p));
                match (c, p) {
                    (Some(c), Some(p)) => {
                        profile::set_default(&log, &c, &p);
                    }
                    _ => log.line("set-default 参数不是合法 GUID"),
                }
            }
            if a.enum_profiles {
                profile::enum_profiles(&log);
            }
            if a.report_profile {
                profile::snapshot(&log, "旁观");
            }
            if let Some(dll) = a.load_tip.clone() {
                let ok = tip::run_mode(&dll, a.tip_hold, a.activate_profile, a.activate_legacy, a.activate_hold, a.probe_ctx, &log);
                log.line(&format!("tip RESULT {}", if ok { "PASS" } else { "FAIL" }));
            }
        }
        let deadline = start + Duration::from_millis(a.life_ms);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        log.line("no-window 模式结束");
        return;
    }

    unsafe {
        let inst = GetModuleHandleW(std::ptr::null());
        let cls = wide(CLASS_NAME);
        let wc = WndClassW {
            style: 0,
            lpfn_wnd_proc: Some(wnd_proc),
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: inst,
            h_icon: 0,
            h_cursor: 0,
            hbr_background: 0,
            menu_name: std::ptr::null(),
            class_name: cls.as_ptr(),
        };
        let atom = RegisterClassW(&wc);
        log.line(&format!("RegisterClass atom={atom}"));
        let trace_ref = if a.trace { Some(&log) } else { None };

        // 顺序要紧:TSF 先 Activate,再建窗口/给焦点。
        if a.tfinit {
            store::init_thread_mgr(&log);
        }

        let title = wide(&a.title);
        let hwnd = CreateWindowExW(
            0,
            cls.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            20,
            20,
            760,
            520,
            0,
            0,
            inst,
            0,
        );
        if hwnd == 0 {
            log.line("FATAL CreateWindowExW failed");
            eprintln!("[host] CreateWindowExW failed");
            std::process::exit(1);
        }

        // 经典 EDIT:走的是 IMM32/TSF 兼容路径,和记事本那类应用同一条。
        // `--control rich50` 换成 RichEdit(msftedit.dll 的 RICHEDIT50W,写字板那类),
        // 用来确认「经典控件那一族」的退格判据是不是同一条。
        let edit_cls = match a.control.as_str() {
            "rich50" | "richedit" => {
                let m = LoadLibraryW(wide("msftedit.dll").as_ptr());
                log.line(&format!("LoadLibrary msftedit.dll module=0x{m:x}"));
                wide("RICHEDIT50W")
            }
            _ => wide("EDIT"),
        };
        let edit = CreateWindowExW(
            0,
            edit_cls.as_ptr(),
            wide("").as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_BORDER | WS_VSCROLL | ES_MULTILINE | ES_AUTOVSCROLL,
            8,
            8,
            720,
            440,
            hwnd,
            0,
            inst,
            0,
        );
        if edit == 0 {
            log.line("FATAL EDIT creation failed");
            std::process::exit(1);
        }
        EDIT_HANDLE.store(edit, Ordering::SeqCst);
        WINDOW.with(|w| w.set(hwnd));
        let fg = if a.hide_window {
            0 // 不显示、也不抢前台:只要控件存在于本进程,焦点在**自己的线程内**就够了
        } else {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd)
        };
        SetFocus(edit);
        pump(trace_ref, !a.no_translate);

        log.line(&format!(
            "window=0x{hwnd:x} edit=0x{edit:x} focused={} foreground={fg}",
            GetFocus() == edit
        ));

        if a.enum_profiles {
            profile::enum_profiles(&log);
            println!("[host] enum-profiles 完成");
        }
        if a.report_profile {
            profile::snapshot(&log, "旁观");
            println!("[host] report-profile 完成");
        }
        if a.tfstore {
            // 先把上下文建好(组合态要用),再装薄壳 —— 薄壳 `Activate` 时拿到的线程
            // 管理器与本进程是同一个,所以它 `GetFocus` 就能看到我们刚推上去的上下文。
            store::create_context(&log, a.log.clone(), &a.store_seed);
        }
        if let Some(dll) = a.load_tip.clone() {
            let ok = tip::run_mode(
                &dll,
                a.tip_hold,
                a.activate_profile,
                a.activate_legacy,
                a.activate_hold,
                a.probe_ctx,
                &log,
            );
            log.line(&format!("tip RESULT {}", if ok { "PASS" } else { "FAIL" }));
            println!("[host] tip {}", if ok { "PASS" } else { "FAIL" });
        }

        // WM_CHAR 往返自检:证明壳真的能收字符、能读回。
        if let Some(path) = a.selftest_file.clone() {
            match std::fs::read_to_string(&path) {
                Ok(t) => a.selftest = Some(t.trim_end_matches(['\r', '\n']).to_string()),
                Err(e) => {
                    log.line(&format!("FATAL cannot read selftest file {path:?}: {e}"));
                    eprintln!("[host] cannot read selftest file: {e}");
                    std::process::exit(1);
                }
            }
        }
        if let Some(text) = a.selftest.clone() {
            let before = read_edit(edit);
            for unit in text.encode_utf16() {
                PostMessageW(edit, WM_CHAR, unit as usize, 0);
            }
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_millis(400) {
                pump(trace_ref, !a.no_translate);
                std::thread::sleep(Duration::from_millis(5));
            }
            let after = read_edit(edit);
            let expected = format!("{before}{text}");
            let ok = after == expected;
            log.line(&format!(
                "selftest {} expected={expected:?} actual={after:?}",
                if ok { "PASS" } else { "FAIL" }
            ));
            println!("[host] selftest {} actual={after:?}", if ok { "PASS" } else { "FAIL" });
        }

        // 主环:泵消息 + 定时把 EDIT 内容的变化写进日志(外部断言看日志或用 hdesk readtext)。
        let mut last = read_edit(edit);
        log.line(&format!("initial text={last:?}"));
        let deadline = start + Duration::from_millis(a.life_ms);
        let mut last_dump = Instant::now();
        while !DESTROYED.load(Ordering::SeqCst) && Instant::now() < deadline {
            pump(trace_ref, !a.no_translate);
            std::thread::sleep(Duration::from_millis(5));
            if last_dump.elapsed() >= Duration::from_millis(a.dump_every.max(20)) {
                last_dump = Instant::now();
                let cur = read_edit(edit);
                if cur != last {
                    log.line(&format!("text changed -> {cur:?}"));
                    last = cur;
                }
            }
        }

        let final_text = read_edit(edit);
        log.line(&format!("final text={final_text:?}"));
        println!("[host] final text={final_text:?}");
        DestroyWindow(hwnd);
        pump(None, false);
    }
}
