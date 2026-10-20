//! 系统托盘图标 —— 让 sainput 有「看得见的存在感」。
//!
//! 为什么需要它：sainput 本体是个控制台程序，用户双击 start.bat 之后只有一个黑窗口，
//! 关掉就没了、也看不出它到底在不在跑。托盘图标解决三件事：
//!   1) 一眼看到「它在运行」；
//!   2) 右键能退出（不用去黑窗口按 Ctrl+C）；
//!   3) 启动时弹一个气泡告诉用户怎么用。
//!
//! 图标是自己按像素画的（3x3 超采样成 32x32 带 alpha 的麦克风），不依赖任何外部资源。
//! 线程模型与 overlay 一致：独立线程持隐藏窗口 + 消息循环，主线程只读它的请求。

use std::ptr;
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use crate::hotkey::{DispatchMessageW, GetMessageW, Msg, TranslateMessage};

pub const TRAY_CLASS: &str = "SainputTrayWnd";

const WM_TRAY: u32 = 0x0400 + 100; // WM_APP + 100
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_CONTEXTMENU: u32 = 0x007B;
const WM_COMMAND: u32 = 0x0111;
const WM_NULL: u32 = 0x0000;

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;

const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;
const NIF_INFO: u32 = 0x10;

const MF_STRING: u32 = 0x0000;
const MF_GRAYED: u32 = 0x0001;
const MF_CHECKED: u32 = 0x0008;
const MF_SEPARATOR: u32 = 0x0800;
const MF_POPUP: u32 = 0x0010;
const TPM_RIGHTBUTTON: u32 = 0x0002;
const TPM_RETURNCMD: u32 = 0x0100;

const ID_STATUS: usize = 1;
const ID_EXIT: usize = 2;
const ID_PUNCT_SPACE: usize = 10;
const ID_MERGE_TERMS: usize = 11;
const ID_CTX_BIAS: usize = 12;
const ID_CTX_VERBOSE: usize = 13;
const ID_LEADING_SPACE: usize = 14;
const ID_FIX_CASE: usize = 15;
/// 静音断句子菜单的基址（5/10/15/30/不断句）
const ID_SILENCE_BASE: usize = 200;
/// 把隐藏的控制台再叫出来（双击启动时控制台是自动收起的）
const ID_SHOW_CONSOLE: usize = 17;
/// 开机自启开关（写 HKCU\...\Run）
const ID_AUTOSTART: usize = 18;
/// 悬浮显示方式子菜单：关 / 细条 / 悬浮球（三选一，互斥）
const ID_OVERLAY_BASE: usize = 300;
/// 边缘光颜色子菜单基址（white/rainbow/amber/cyan/aurora）
const ID_EDGE_TINT_BASE: usize = 320;
/// 释放卡住的修饰键（键盘失灵时自救）
const ID_RELEASE_KEYS: usize = 19;
/// 热键子菜单的基址：ID_HOTKEY_BASE + 序号。
/// 用连续 id 而不是给每个键写死常量 —— 以后加热键只改候选表，不用动命令行常量。
const ID_HOTKEY_BASE: usize = 100;

/// 托盘菜单里可以点的开关。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayToggle {
    /// 中文标点转空格（问号保留）
    PunctToSpace,
    /// 英文拆字合并（"R E A" -> "REA"）
    MergeTerms,
    /// 上下文偏置
    CtxBias,
    /// 偏置生效时在控制台打提示
    CtxVerbose,
    /// 光标前有字时在前面补一个空格
    LeadingSpace,
    /// 英文大小写修复（模型吐不出小写）
    FixCase,
    /// 静音多少秒算一句说完
    SetSilence(u32),
    /// 换热键（换成候选表里第 n 个）
    SetHotkey(usize),
    /// 把控制台窗口叫出来
    ShowConsole,
    /// 开机自启开关
    AutoStart,
    /// 悬浮显示方式：0=关 1=细条 2=悬浮球 3=边缘光
    SetOverlayMode(usize),
    /// 边缘光颜色：0=白 1=彩虹 2=暖橙 3=青蓝 4=极光
    SetEdgeTint(usize),
    /// 释放卡住的修饰键（键盘失灵时自救）
    ReleaseKeys,
}

/// 热键候选表 —— 托盘「换热键」子菜单就照这个列。
///
/// 为什么是"选"而不是"录入"：让用户录一次按键要抓键、要处理冲突、要弹窗，
/// 是一整个小功能；先给一份**覆盖绝大多数人**的候选，用起来最直接。
pub const HOTKEY_CANDIDATES: &[(&str, &str)] = &[
    ("mousex1", "鼠标侧键 X1（后侧）"),
    ("mousex2", "鼠标侧键 X2（前侧）"),
    ("rightctrl", "右 Ctrl"),
    ("rightalt", "右 Alt"),
    ("f8", "F8"),
    ("f9", "F9"),
    ("f10", "F10"),
];

/// 主循环每拍读一次：托盘点了哪个开关。
static PENDING: std::sync::Mutex<Option<TrayToggle>> = std::sync::Mutex::new(None);
/// 托盘图标的数据块 —— 必须留着：Explorer 重启后要拿同一份数据重新 NIM_ADD。
static TRAY_NID: std::sync::Mutex<Option<NotifyIconDataW>> = std::sync::Mutex::new(None);
/// Explorer 重启时广播的消息 id（RegisterWindowMessageW 的返回值；0 = 没注册成功）。
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
/// 当前开关状态（由主循环推给托盘，用来画勾）。
static FLAGS: std::sync::Mutex<TrayFlags> = std::sync::Mutex::new(TrayFlags {
    punct_to_space: false,
    merge_terms: true,
    ctx_bias: true,
    ctx_verbose: false,
    leading_space: true,
    fix_case: true,
    silence_sec: 15,
    overlay_mode: 0,
    edge_tint: 0,
    hotkey: "rightctrl",
    autostart: false,
});

/// 托盘要显示的开关状态。
#[derive(Clone, Copy)]
pub struct TrayFlags {
    pub punct_to_space: bool,
    pub merge_terms: bool,
    pub ctx_bias: bool,
    pub ctx_verbose: bool,
    /// 光标前有字时补一个前置空格。
    pub leading_space: bool,
    /// 英文大小写修复。
    pub fix_case: bool,
    /// 当前静音断句门槛（秒；0 = 不断句）。
    pub silence_sec: u32,
    /// 悬浮显示方式：0=关 1=细条 2=悬浮球 3=边缘光。**互斥** —— 一次只显示一个。
    pub overlay_mode: usize,
    /// 边缘光颜色：0=白 1=彩虹 2=暖橙 3=青蓝 4=极光。
    pub edge_tint: usize,
    /// 当前热键的配置写法（用来在子菜单里打勾），例如 "mousex1"。
    pub hotkey: &'static str,
    /// 是否已设置开机自启。
    pub autostart: bool,
}

/// 主循环把当前生效状态推给托盘（决定菜单里哪几项打勾）。
pub fn set_flags(f: TrayFlags) {
    if let Ok(mut g) = FLAGS.lock() {
        *g = f;
    }
}

/// 菜单里那行标题（带版本号）。
static TITLE: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// 设置托盘的悬停提示与菜单标题（带版本号 —— 用户要看得出跑的是哪版）。
pub fn set_title(t: &str) {
    if let Ok(mut g) = TITLE.lock() {
        *g = t.to_string();
    }
}

fn title_now() -> String {
    TITLE
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// 把热键配置写法翻成人话（托盘菜单标题用）。
/// 只认托盘候选表里的那几种 + 常见键；认不出就原样显示，不假装认识。
pub fn key_label_of(conf: &str) -> &'static str {
    match conf {
        "mousex1" => "鼠标侧键 X1",
        "mousex2" => "鼠标侧键 X2",
        "mousemiddle" => "鼠标中键",
        "rightctrl" => "右 Ctrl",
        "leftctrl" => "左 Ctrl",
        "rightalt" => "右 Alt",
        "leftalt" => "左 Alt",
        "rightshift" => "右 Shift",
        "leftshift" => "左 Shift",
        "capslock" => "CapsLock",
        "f8" => "F8",
        "f9" => "F9",
        "f10" => "F10",
        "backquote" => "`",
        _ => "?",
    }
}

/// 取走一次托盘开关点击（主循环用；没有就返回 None）。
pub fn take_toggle() -> Option<TrayToggle> {
    PENDING.lock().ok().and_then(|mut g| g.take())
}

/// 托盘窗口回调里记录「用户点了退出」。
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);
/// 需要在退出时唤醒的主线程（热键消息循环所在线程）。
static MAIN_THREAD: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
#[derive(Clone, Copy)]
struct NotifyIconDataW {
    cb_size: u32,
    hwnd: isize,
    u_id: u32,
    u_flags: u32,
    u_callback_message: u32,
    h_icon: isize,
    sz_tip: [u16; 128],
    dw_state: u32,
    dw_state_mask: u32,
    sz_info: [u16; 256],
    u_timeout_or_version: u32,
    sz_info_title: [u16; 64],
    dw_info_flags: u32,
    guid_item: [u8; 16],
    h_balloon_icon: isize,
}

#[repr(C)]
struct IconInfo {
    f_icon: i32,
    x_hotspot: u32,
    y_hotspot: u32,
    hbm_mask: isize,
    hbm_color: isize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BitmapInfoHeader {
    bi_size: u32,
    bi_width: i32,
    bi_height: i32,
    bi_planes: u16,
    bi_bit_count: u16,
    bi_compression: u32,
    bi_size_image: u32,
    bi_x_pels: i32,
    bi_y_pels: i32,
    bi_clr_used: u32,
    bi_clr_important: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BitmapInfo {
    header: BitmapInfoHeader,
    colors: [u32; 3],
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

#[link(name = "shell32")]
extern "system" {
    fn Shell_NotifyIconW(message: u32, data: *mut NotifyIconDataW) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(lpwc: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex: u32,
        class: *const u16,
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
    fn DestroyWindow(hwnd: isize) -> i32;
    fn DefWindowProcW(hwnd: isize, msg: u32, wp: usize, lp: isize) -> isize;
    fn CreatePopupMenu() -> isize;
    fn DestroyMenu(menu: isize) -> i32;
    fn AppendMenuW(menu: isize, flags: u32, id: usize, item: *const u16) -> i32;
    fn TrackPopupMenu(menu: isize, flags: u32, x: i32, y: i32, reserved: i32, hwnd: isize, rect: isize) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
    fn GetCursorPos(pt: *mut Point) -> i32;
    fn PostMessageW(hwnd: isize, msg: u32, wp: usize, lp: isize) -> i32;
    fn PostQuitMessage(code: i32);
    fn PostThreadMessageW(thread: u32, msg: u32, wp: usize, lp: isize) -> i32;
    fn GetCurrentThreadId() -> u32;
    fn GetModuleHandleW(name: *const u16) -> isize;
    fn RegisterWindowMessageW(name: *const u16) -> u32;
    fn EnumWindows(cb: Option<unsafe extern "system" fn(isize, isize) -> i32>, lp: isize) -> i32;
    fn GetClassNameW(hwnd: isize, buf: *mut u16, max: i32) -> i32;
    fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
}

#[link(name = "user32")]
extern "system" {
    fn SetWindowTextW(hwnd: isize, text: *const u16) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn FreeConsole() -> i32;
    fn SetStdHandle(which: u32, handle: isize) -> i32;
    fn CreateFileW(
        name: *const u16, access: u32, share: u32, sec: *const u8,
        disposition: u32, flags: u32, template: isize,
    ) -> isize;
    fn GetConsoleWindow() -> isize;
    fn SetConsoleTitleW(title: *const u16) -> i32;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateDIBSection(
        hdc: isize,
        bmi: *const BitmapInfo,
        usage: u32,
        bits: *mut *mut u8,
        section: isize,
        offset: u32,
    ) -> isize;
    fn CreateBitmap(width: i32, height: i32, planes: u32, bits_per_pel: u32, bits: *const u8) -> isize;
    fn CreateIconIndirect(info: *const IconInfo) -> isize;
    fn DeleteObject(obj: isize) -> i32;
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

fn set_fixed<const N: usize>(dst: &mut [u16; N], s: &str) {
    let w = wide(s);
    let n = w.len().min(N);
    dst[..n].copy_from_slice(&w[..n]);
    if n < N {
        dst[n] = 0;
    }
}

/// 托盘句柄。起不来时是 None（调用方按「没有也不影响录音」处理）。
#[derive(Clone)]
pub struct Tray {
    enabled: bool,
}

impl Tray {
    /// 记录主线程 id，退出时用它唤醒热键消息循环。
    pub fn note_main_thread() {
        MAIN_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
    }

    pub fn start(tooltip: &str) -> Option<Tray> {
        let tip = tooltip.to_string();
        let (tx, rx) = mpsc::channel::<bool>();
        std::thread::Builder::new()
            .name("sainput-tray".into())
            .spawn(move || unsafe {
                // Explorer 重启会广播这条消息。名字注册一次，id 全进程有效。
                TASKBAR_CREATED.store(
                    RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
                    Ordering::Relaxed,
                );
                let hwnd = create_window();
                if hwnd == 0 {
                    let _ = tx.send(false);
                    return;
                }
                let icon = make_icon();
                let mut nid = new_nid(hwnd);
                nid.u_flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
                nid.u_callback_message = WM_TRAY;
                nid.h_icon = icon;
                set_fixed(&mut nid.sz_tip, &tip);
                if Shell_NotifyIconW(NIM_ADD, &mut nid) == 0 {
                    let _ = tx.send(false);
                    return;
                }
                if let Ok(mut g) = TRAY_NID.lock() {
                    *g = Some(nid);
                }
                let _ = tx.send(true);
                let mut msg: Msg = std::mem::zeroed();
                while GetMessageW(&mut msg, 0, 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                Shell_NotifyIconW(NIM_DELETE, &mut nid);
                if icon != 0 {
                    DeleteObject(icon);
                }
            })
            .ok()?;
        match rx.recv_timeout(Duration::from_millis(2500)) {
            Ok(true) => Some(Tray { enabled: true }),
            _ => None,
        }
    }

    /// 弹一个气泡通知（启动提示、状态变化）。
    pub fn balloon(&self, title: &str, text: &str) {
        if !self.enabled {
            return;
        }
        let hwnd = match HWND.get() {
            Some(h) => *h,
            None => return,
        };
        unsafe {
            balloon_now(hwnd, title, text);
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

/// 给控制台窗口起个能认出来的标题（默认是那串 exe 路径，用户看不出是什么）。
pub fn set_console_title(title: &str) {
    unsafe {
        let c = GetConsoleWindow();
        if c == 0 {
            return;
        }
        let t = wide(title);
        SetWindowTextW(c, t.as_ptr());
    }
}

/// 隐藏控制台。
///
/// **不是** `ShowWindow(SW_HIDE)` —— 那个在 Windows Terminal 承载下无效
/// （可见窗口属于 WindowsTerminal.exe，不属于本进程，`GetConsoleWindow()`
/// 拿到的是另一个隐藏句柄）。
///
/// 真正有效的是 `FreeConsole()`：把进程从控制台分离。
/// 分离后标准输出失效，所以先把 stdout/stderr/stdin 指向 NUL，
/// 避免后续 println! 触发写错误。
pub fn set_console_visible(v: bool) {
    unsafe {
        if v {
            // 「显示」在 FreeConsole 之后没法真的恢复控制台（不可逆）。
            // 用 SW_SHOW 尽力而为 —— 如果进程还在控制台上就有用。
            let c = GetConsoleWindow();
            if c != 0 {
                ShowWindow(c, 5); // SW_SHOW
            }
            return;
        }

        // 把三个标准句柄指向 NUL，避免 FreeConsole 之后写管道出错。
        let nul = wide("NUL");
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const GENERIC_READ: u32 = 0x8000_0000;
        const OPEN_EXISTING: u32 = 3;
        // STD_INPUT=-10 / STD_OUTPUT=-11 / STD_ERROR=-12
        let h_out = CreateFileW(
            nul.as_ptr(), GENERIC_WRITE, 0, std::ptr::null(), OPEN_EXISTING, 0, 0,
        );
        if h_out != -1isize {
            SetStdHandle(0xFFFF_FFF5, h_out); // -11 as u32
            SetStdHandle(0xFFFF_FFF4, h_out); // -12 as u32
        }
        let h_in = CreateFileW(
            nul.as_ptr(), GENERIC_READ, 0, std::ptr::null(), OPEN_EXISTING, 0, 0,
        );
        if h_in != -1isize {
            SetStdHandle(0xFFFF_FFF6, h_in); // -10 as u32
        }

        FreeConsole();
    }
}

/// 让指定进程走「托盘退出」路径：它的主循环收尾、sidecar 由 Drop 结束。
///
/// 找不到它的托盘窗口（旧版没起托盘 / 托盘线程已死）时返回 false，
/// 交给调用方硬杀。跨进程 PostMessage 不需要命名管道 —— 类名固定，找得到就能发。
pub fn request_exit_of(pid: u32) -> bool {
    struct Ctx {
        pid: u32,
        sent: bool,
    }

    unsafe extern "system" fn find_and_exit(hwnd: isize, lp: isize) -> i32 {
        let ctx = &mut *(lp as *mut Ctx);
        let mut wpid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut wpid);
        if wpid != ctx.pid {
            return 1;
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, buf.as_mut_ptr(), 64);
        if n <= 0 || String::from_utf16_lossy(&buf[..n as usize]) != TRAY_CLASS {
            return 1;
        }
        PostMessageW(hwnd, WM_COMMAND, ID_EXIT, 0);
        ctx.sent = true;
        0
    }

    let mut ctx = Ctx { pid, sent: false };
    unsafe {
        EnumWindows(Some(find_and_exit), &mut ctx as *mut Ctx as isize);
    }
    ctx.sent
}

/// 用户是否通过托盘点了退出。
pub fn exit_requested() -> bool {
    EXIT_REQUESTED.load(Ordering::Relaxed)
}

static HWND: std::sync::OnceLock<isize> = std::sync::OnceLock::new();

fn new_nid(hwnd: isize) -> NotifyIconDataW {
    NotifyIconDataW {
        cb_size: std::mem::size_of::<NotifyIconDataW>() as u32,
        hwnd,
        u_id: 1,
        u_flags: 0,
        u_callback_message: 0,
        h_icon: 0,
        sz_tip: [0; 128],
        dw_state: 0,
        dw_state_mask: 0,
        sz_info: [0; 256],
        u_timeout_or_version: 0,
        sz_info_title: [0; 64],
        dw_info_flags: 0,
        guid_item: [0; 16],
        h_balloon_icon: 0,
    }
}

unsafe fn create_window() -> isize {
    let inst = GetModuleHandleW(ptr::null());
    let cls = wide(TRAY_CLASS);
    let wc = WndClassW {
        style: 0,
        lpfn_wnd_proc: Some(wnd_proc),
        cb_cls_extra: 0,
        cb_wnd_extra: 0,
        h_instance: inst,
        h_icon: 0,
        h_cursor: 0,
        hbr_background: 0,
        menu_name: ptr::null(),
        class_name: cls.as_ptr(),
    };
    if RegisterClassW(&wc) == 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(1410) {
            return 0;
        }
    }
    let title = wide("sainput");
    let hwnd = CreateWindowExW(0, cls.as_ptr(), title.as_ptr(), 0, 0, 0, 0, 0, 0, 0, inst, 0);
    if hwnd != 0 {
        let _ = HWND.set(hwnd);
    }
    hwnd
}

/// Explorer 重启会把通知区域整个重建，托盘图标随之消失。
/// 拿启动时存下的 nid 重新 ADD 一次 —— 不重新 ADD，进程还在但用户永远看不到图标。
/// 图标本来就在时 ADD 会失败（同一 hwnd+uID 重复注册），这不是错误，走 MODIFY 更新即可。
unsafe fn restore_icon() {
    if let Ok(mut g) = TRAY_NID.lock() {
        if let Some(nid) = g.as_mut() {
            nid.u_flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            if Shell_NotifyIconW(NIM_ADD, nid) == 0 {
                Shell_NotifyIconW(NIM_MODIFY, nid);
            }
        }
    }
}

unsafe fn balloon_now(hwnd: isize, title: &str, text: &str) {
    let mut nid = new_nid(hwnd);
    nid.u_flags = NIF_INFO;
    set_fixed(&mut nid.sz_info_title, title);
    set_fixed(&mut nid.sz_info, text);
    nid.dw_info_flags = 1; // NIIF_INFO
    Shell_NotifyIconW(NIM_MODIFY, &mut nid);
}

unsafe extern "system" fn wnd_proc(hwnd: isize, msg: u32, wp: usize, lp: isize) -> isize {
    // Explorer 崩溃/更新/重启后广播 TaskbarCreated。收到就重新挂图标，
    // 否则图标永久消失：sainput 照常工作，用户却以为它没开。
    let tc = TASKBAR_CREATED.load(Ordering::Relaxed);
    if tc != 0 && msg == tc {
        restore_icon();
        return 0;
    }
    match msg {
        WM_TRAY => {
            // 传统版本的托盘回调：lParam 就是鼠标消息
            let m = lp as u32 & 0xFFFF;
            if m == WM_RBUTTONUP || m == WM_CONTEXTMENU || m == WM_LBUTTONUP {
                show_menu(hwnd);
            }
            0
        }
        WM_COMMAND => {
            // 菜单项两条来路都要认：
            //   1) TrackPopupMenu 带 TPM_RETURNCMD 时，命令是**返回值**（见 show_menu 末尾）；
            //   2) 不带 RETURNCMD 时（以及外部/脚本发 WM_COMMAND 时），命令走 WM_COMMAND。
            // 只认其中一条会让开关在某些路径下静默失效。
            let id = wp & 0xFFFF;
            if id == ID_EXIT {
                request_exit();
            } else {
                queue_toggle(id);
            }
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn show_menu(hwnd: isize) {
    let menu = CreatePopupMenu();
    if menu == 0 {
        return;
    }
    let f = FLAGS.lock().map(|g| *g).unwrap_or(TrayFlags {
        punct_to_space: false,
        merge_terms: true,
        ctx_bias: true,
        ctx_verbose: false,
        leading_space: true,
        fix_case: true,
        silence_sec: 15,
        overlay_mode: 0,
        edge_tint: 0,
        hotkey: "rightctrl",
        autostart: false,
    });
    let tick = |on: bool| if on { MF_CHECKED } else { 0 };

    // 第一行是标题（带版本号），灰色不可点 —— 用户一眼看出跑的是哪版。
    let t = title_now();
    let title = wide(if t.is_empty() { "sainput" } else { &t });
    let sep = wide("");
    let punct = wide("标点转空格（保留问号）");
    let merge = wide("英文连写（R E A → REA）");
    let ctx = wide("上下文偏置（记住刚打的词）");
    let ctxv = wide("显示偏置提示");
    let lead = wide("前面自动加空格");
    // 悬浮显示方式：三选一互斥（用户要求"一次最多只能显示一个或者完全不显示"）
    let MODES: [&str; 4] = [
        "不显示",
        "细条（只显示文字）",
        "悬浮球（录音波形）",
        "屏幕边缘光（录音时微亮）",
    ];
    let ovl_title = wide(&format!(
        "悬浮显示（当前：{}）",
        MODES.get(f.overlay_mode).copied().unwrap_or("不显示")
    ));
    let ovl_menu = CreatePopupMenu();
    let fixc = wide("英文小写修复");
    let showcon = wide("显示日志窗口");
    let autost = wide("开机自启");
    let sil_title = wide(&format!(
        "静音断句（当前：{}）",
        if f.silence_sec == 0 { "不断句".to_string() } else { format!("{} 秒", f.silence_sec) }
    ));
    let sil_menu = CreatePopupMenu();
    const SILENCE_CHOICES: [u32; 5] = [5, 10, 15, 30, 0];
    let exit = wide("退出 sainput");
    // 热键子菜单：标题写当前键，展开是候选表
    let hk_title = wide(&format!("换热键（当前：{}）", key_label_of(f.hotkey)));
    let hk_menu = CreatePopupMenu();

    AppendMenuW(menu, MF_STRING | MF_GRAYED, ID_STATUS, title.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, sep.as_ptr());
    // 热键放最上面 —— 这是用户最常改的一项
    if hk_menu != 0 {
        for (i, (conf, label)) in HOTKEY_CANDIDATES.iter().enumerate() {
            let l = wide(label);
            let checked = if *conf == f.hotkey { MF_CHECKED } else { 0 };
            AppendMenuW(hk_menu, MF_STRING | checked, ID_HOTKEY_BASE + i, l.as_ptr());
        }
        // MF_POPUP = 0x0010：这一项挂一个子菜单，不是命令 id
        AppendMenuW(menu, MF_STRING | MF_POPUP, hk_menu as usize, hk_title.as_ptr());
    }
    AppendMenuW(menu, MF_SEPARATOR, 0, sep.as_ptr());
    AppendMenuW(menu, MF_STRING | tick(f.punct_to_space), ID_PUNCT_SPACE, punct.as_ptr());
    AppendMenuW(menu, MF_STRING | tick(f.merge_terms), ID_MERGE_TERMS, merge.as_ptr());
    AppendMenuW(menu, MF_STRING | tick(f.ctx_bias), ID_CTX_BIAS, ctx.as_ptr());
    AppendMenuW(menu, MF_STRING | tick(f.ctx_verbose), ID_CTX_VERBOSE, ctxv.as_ptr());
    // 前置空格放最后一组 —— 它是"上屏形态"类开关，和上面三个"文本内容"类分开
    AppendMenuW(menu, MF_STRING | tick(f.leading_space), ID_LEADING_SPACE, lead.as_ptr());
    if ovl_menu != 0 {
        for (i, name) in MODES.iter().enumerate() {
            let l = wide(name);
            let checked = if i == f.overlay_mode { MF_CHECKED } else { 0 };
            AppendMenuW(ovl_menu, MF_STRING | checked, ID_OVERLAY_BASE + i, l.as_ptr());
        }
        // 边缘光颜色：只在选了边缘光时才有意义，但一直显示（切过去就能立刻改）
        let TINTS: [&str; 5] = ["白光", "彩虹光", "暖橙光", "青蓝光", "极光（青紫）"];
        let tint_title = wide(&format!(
            "边缘光颜色（当前：{}）",
            TINTS.get(f.edge_tint).copied().unwrap_or("白光")
        ));
        let tint_menu = CreatePopupMenu();
        if tint_menu != 0 {
            for (i, name) in TINTS.iter().enumerate() {
                let l = wide(name);
                let checked = if i == f.edge_tint { MF_CHECKED } else { 0 };
                AppendMenuW(tint_menu, MF_STRING | checked, ID_EDGE_TINT_BASE + i, l.as_ptr());
            }
            AppendMenuW(ovl_menu, MF_SEPARATOR, 0, sep.as_ptr());
            AppendMenuW(ovl_menu, MF_STRING | MF_POPUP, tint_menu as usize, tint_title.as_ptr());
        }
        AppendMenuW(menu, MF_STRING | MF_POPUP, ovl_menu as usize, ovl_title.as_ptr());
    }
    AppendMenuW(menu, MF_STRING | tick(f.fix_case), ID_FIX_CASE, fixc.as_ptr());
    AppendMenuW(menu, MF_STRING | tick(f.autostart), ID_AUTOSTART, autost.as_ptr());
    if sil_menu != 0 {
        for (i, sec) in SILENCE_CHOICES.iter().enumerate() {
            let text = if *sec == 0 { "不断句（只认松手）".to_string() } else { format!("{sec} 秒") };
            let l = wide(&text);
            let checked = if *sec == f.silence_sec { MF_CHECKED } else { 0 };
            AppendMenuW(sil_menu, MF_STRING | checked, ID_SILENCE_BASE + i, l.as_ptr());
        }
        AppendMenuW(menu, MF_STRING | MF_POPUP, sil_menu as usize, sil_title.as_ptr());
    }
    AppendMenuW(menu, MF_SEPARATOR, 0, sep.as_ptr());
    AppendMenuW(menu, MF_STRING, ID_RELEASE_KEYS, wide("释放卡住的键（键盘失灵时用）").as_ptr());
    AppendMenuW(menu, MF_STRING, ID_SHOW_CONSOLE, showcon.as_ptr());
    AppendMenuW(menu, MF_STRING, ID_EXIT, exit.as_ptr());

    let mut pt = Point { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    // 这两步是托盘菜单的标准姿势：不先 SetForegroundWindow 菜单不会自己消失
    SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD, pt.x, pt.y, 0, hwnd, 0);
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
    let id = cmd as usize;
    if id == ID_EXIT {
        request_exit();
        return;
    }
    // 开关点击：记下来，由主循环取走并落盘。托盘线程不自己改状态（免得多线程打架）。
    queue_toggle(id);
}

/// 把菜单 id 翻成开关并排队（主循环取走）。
fn queue_toggle(id: usize) {
    // 热键候选是连续 id：ID_HOTKEY_BASE + 序号
    const SILENCE_CHOICES: [u32; 5] = [5, 10, 15, 30, 0];
    if id >= ID_SILENCE_BASE && id < ID_SILENCE_BASE + SILENCE_CHOICES.len() {
        if let Ok(mut g) = PENDING.lock() {
            *g = Some(TrayToggle::SetSilence(SILENCE_CHOICES[id - ID_SILENCE_BASE]));
        }
        return;
    }
    if id >= ID_HOTKEY_BASE && id < ID_HOTKEY_BASE + HOTKEY_CANDIDATES.len() {
        if let Ok(mut g) = PENDING.lock() {
            *g = Some(TrayToggle::SetHotkey(id - ID_HOTKEY_BASE));
        }
        return;
    }
    let t = match id {
        ID_PUNCT_SPACE => Some(TrayToggle::PunctToSpace),
        ID_MERGE_TERMS => Some(TrayToggle::MergeTerms),
        ID_CTX_BIAS => Some(TrayToggle::CtxBias),
        ID_CTX_VERBOSE => Some(TrayToggle::CtxVerbose),
        ID_LEADING_SPACE => Some(TrayToggle::LeadingSpace),
        ID_FIX_CASE => Some(TrayToggle::FixCase),
        ID_SHOW_CONSOLE => Some(TrayToggle::ShowConsole),
        ID_AUTOSTART => Some(TrayToggle::AutoStart),
        ID_RELEASE_KEYS => Some(TrayToggle::ReleaseKeys),
        _ if id >= ID_OVERLAY_BASE && id < ID_OVERLAY_BASE + 4 => {
            Some(TrayToggle::SetOverlayMode(id - ID_OVERLAY_BASE))
        }
        _ if id >= ID_EDGE_TINT_BASE && id < ID_EDGE_TINT_BASE + 5 => {
            Some(TrayToggle::SetEdgeTint(id - ID_EDGE_TINT_BASE))
        }
        _ => None,
    };
    if let Some(t) = t {
        if let Ok(mut g) = PENDING.lock() {
            *g = Some(t);
        }
    }
}

/// 退出：先把控制台还原（用户可能还要看日志），再让主线程的消息循环结束，
/// 这样 Sidecar 的 Drop 才会跑、子进程不会变成孤儿。
unsafe fn request_exit() {
    if EXIT_REQUESTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let c = GetConsoleWindow();
    if c != 0 {
        ShowWindow(c, 5); // SW_SHOW
    }
    let tid = MAIN_THREAD.load(Ordering::Relaxed);
    if tid != 0 {
        PostThreadMessageW(tid, 0x0012, 0, 0); // WM_QUIT
    }
}

/// 自己画一个 32x32 的麦克风图标（3x3 超采样抗锯齿）。
unsafe fn make_icon() -> isize {
    const S: i32 = 32;
    let mut bits_ptr: *mut u8 = ptr::null_mut();
    let bmi = BitmapInfo {
        header: BitmapInfoHeader {
            bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
            bi_width: S,
            bi_height: -S,
            bi_planes: 1,
            bi_bit_count: 32,
            bi_compression: 0,
            bi_size_image: (S * S * 4) as u32,
            ..Default::default()
        },
        colors: [0; 3],
    };
    let dib = CreateDIBSection(0, &bmi, 0, &mut bits_ptr, 0, 0);
    if dib == 0 || bits_ptr.is_null() {
        return 0;
    }
    const SS: i32 = 3;
    for py in 0..S {
        for px in 0..S {
            let mut hit = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = px as f32 + (sx as f32 + 0.5) / SS as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / SS as f32;
                    if in_mic(x, y) {
                        hit += 1;
                    }
                }
            }
            let a = (hit * 255 / (SS * SS)) as u8;
            // BGRA：淡蓝，深色和浅色托盘上都看得见
            let o = ((py * S + px) * 4) as usize;
            *bits_ptr.add(o) = 255; // B
            *bits_ptr.add(o + 1) = 200; // G
            *bits_ptr.add(o + 2) = 130; // R
            *bits_ptr.add(o + 3) = a;
        }
    }
    let mask = CreateBitmap(S, S, 1, 1, ptr::null());
    let info = IconInfo {
        f_icon: 1,
        x_hotspot: 0,
        y_hotspot: 0,
        hbm_mask: mask,
        hbm_color: dib,
    };
    let icon = CreateIconIndirect(&info);
    if mask != 0 {
        DeleteObject(mask);
    }
    if dib != 0 {
        DeleteObject(dib);
    }
    icon
}

/// 麦克风形状：胶囊话筒 + 下面的 U 形支架 + 立杆 + 底座。
fn in_mic(x: f32, y: f32) -> bool {
    // 话筒主体：从 (16,9) 到 (16,14.5) 的胶囊，半径 4.6
    if seg_dist(x, y, 16.0, 9.0, 16.0, 14.5) <= 4.6 {
        return true;
    }
    // U 形支架：以 (16,15) 为心、半径 9.4 的圆环下半
    let d = ((x - 16.0).powi(2) + (y - 15.0).powi(2)).sqrt();
    if y >= 15.0 && (8.2..=10.6).contains(&d) {
        return true;
    }
    // 立杆
    if (15.2..=16.8).contains(&x) && (25.0..=27.5).contains(&y) {
        return true;
    }
    // 底座
    if seg_dist(x, y, 12.0, 28.2, 20.0, 28.2) <= 1.5 {
        return true;
    }
    false
}

fn seg_dist(px: f32, py: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let l2 = dx * dx + dy * dy;
    let t = if l2 == 0.0 {
        0.0
    } else {
        (((px - x1) * dx + (py - y1) * dy) / l2).clamp(0.0, 1.0)
    };
    let cx = x1 + t * dx;
    let cy = y1 + t * dy;
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}