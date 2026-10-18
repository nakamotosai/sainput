//! 全局低级键盘钩子：按住说话、松开收尾。
//!
//! 常见做法（`voice key hook installed on dedicated thread`）：
//! 专用线程装 WH_KEYBOARD_LL，目标键按下/抬起转成事件，
//! 并且**把这个键吃掉**，不让它漏给目标应用。

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};

const WH_KEYBOARD_LL: i32 = 13;
const WH_MOUSE_LL: i32 = 14;
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_SYSKEYDOWN: u32 = 0x0104;
const WM_SYSKEYUP: u32 = 0x0105;
// 鼠标低层钩子的消息
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_RBUTTONDOWN: u32 = 0x0204;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_MBUTTONDOWN: u32 = 0x0207;
const WM_MBUTTONUP: u32 = 0x0208;
const WM_XBUTTONDOWN: u32 = 0x020B;
const WM_XBUTTONUP: u32 = 0x020C;
const XBUTTON1: u32 = 0x0001;
const XBUTTON2: u32 = 0x0002;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    Down,
    Up,
}

/// 触发键：可以是键盘键，也可以是鼠标键。
///
/// 为什么要鼠标：用户平时用**鼠标侧键**（X1/X2）说话，键盘钩子根本收不到鼠标事件，
/// 必须再装一个 WH_MOUSE_LL。这不是"多一个选项"，是多一条输入链路。
///
/// ⚠️ 把鼠标键当热键意味着**吃掉这个键**：X1/X2 默认是浏览器的"后退/前进"，
/// 被吃掉之后网页里就不再生效了 —— 这是用户要的（侧键改成说话），但要知道这个代价。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Key(u32),
    /// X1 / X2（侧键）
    MouseX1,
    MouseX2,
    MouseMiddle,
}

impl Trigger {
    /// 能不能被"按住"—— 鼠标左右键不提供，因为按住左键说话会毁掉正常点击。
    pub fn is_holdable(self) -> bool {
        matches!(self, Trigger::Key(_) | Trigger::MouseX1 | Trigger::MouseX2)
    }

    /// 配置文件 / 命令行里的写法。
    pub fn as_conf(self) -> String {
        match self {
            Trigger::Key(vk) => vk_name(vk),
            Trigger::MouseX1 => "mousex1".into(),
            Trigger::MouseX2 => "mousex2".into(),
            Trigger::MouseMiddle => "mousemiddle".into(),
        }
    }

    /// 给人看的名字（托盘标题用）。
    pub fn label(self) -> String {
        match self {
            Trigger::Key(vk) => key_label(vk),
            Trigger::MouseX1 => "鼠标侧键X1".into(),
            Trigger::MouseX2 => "鼠标侧键X2".into(),
            Trigger::MouseMiddle => "鼠标中键".into(),
        }
    }

    /// 托盘菜单里那一行的文字。
    pub fn menu_label(self) -> String {
        format!("热键：{}", self.label())
    }
}

/// 把虚拟键码翻回配置写法（和 `parse_vk` 一一对应）。
pub fn vk_name(vk: u32) -> String {
    match vk {
        0xA3 => "rightctrl".into(),
        0xA2 => "leftctrl".into(),
        0xA5 => "rightalt".into(),
        0xA4 => "leftalt".into(),
        0xA1 => "rightshift".into(),
        0xA0 => "leftshift".into(),
        0x14 => "capslock".into(),
        0x77 => "f8".into(),
        0x78 => "f9".into(),
        0x79 => "f10".into(),
        0xC0 => "backquote".into(),
        other => format!("0x{other:X}"),
    }
}

/// 键盘键的人话名。
pub fn key_label(vk: u32) -> String {
    match vk {
        0xA3 => "右Ctrl".into(),
        0xA2 => "左Ctrl".into(),
        0xA5 => "右Alt".into(),
        0xA4 => "左Alt".into(),
        0xA1 => "右Shift".into(),
        0xA0 => "左Shift".into(),
        0x14 => "CapsLock".into(),
        0x77 => "F8".into(),
        0x78 => "F9".into(),
        0x79 => "F10".into(),
        0xC0 => "`".into(),
        other => format!("0x{other:X}"),
    }
}

/// 解析热键写法 —— 键盘键或鼠标键都能认。
pub fn parse_trigger(s: &str) -> Result<Trigger> {
    let l = s.trim().to_ascii_lowercase();
    Ok(match l.as_str() {
        // 鼠标：侧键的常见叫法都收（用户可能写 mousex1 / mouse4 / xbutton1）
        "mousex1" | "mouse4" | "xbutton1" | "x1" | "侧键1" | "鼠标侧键1" => Trigger::MouseX1,
        "mousex2" | "mouse5" | "xbutton2" | "x2" | "侧键2" | "鼠标侧键2" => Trigger::MouseX2,
        "mousemiddle" | "middle" | "中键" | "鼠标中键" => Trigger::MouseMiddle,
        other => Trigger::Key(parse_vk(other)?),
    })
}

type Hhook = isize;

#[repr(C)]
struct Kbdllhookstruct {
    vk_code: u32,
    scan_code: u32,
    flags: u32,
    time: u32,
    dw_extra_info: usize,
}

type HookProc = unsafe extern "system" fn(i32, usize, isize) -> isize;

#[link(name = "user32")]
extern "system" {
    fn SetWindowsHookExW(id_hook: i32, lpfn: HookProc, hmod: isize, thread_id: u32) -> Hhook;
    fn CallNextHookEx(hhk: Hhook, n_code: i32, wparam: usize, lparam: isize) -> isize;
    fn UnhookWindowsHookEx(hhk: Hhook) -> i32;
    pub(crate) fn GetMessageW(lpmsg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
    pub(crate) fn TranslateMessage(lpmsg: *const Msg) -> i32;
    pub(crate) fn DispatchMessageW(lpmsg: *const Msg) -> isize;
}

/// Win32 MSG。overlay 也用它，所以是 crate 级公开。
#[repr(C)]
pub(crate) struct Msg {
    hwnd: isize,
    message: u32,
    w_param: usize,
    l_param: isize,
    time: u32,
    pt_x: i32,
    pt_y: i32,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
}

/// 当前生效的触发键。**用原子量而不是常量**：托盘菜单里能换键，
/// 换完要立刻生效，不能要求重启（重启会打断用户正在做的事）。
/// 编码方式：0 = 键盘键（值放 TRIGGER_VK），1/2/3 = 鼠标键（见 Trigger 编码）。
static TRIGGER_KIND: AtomicU32 = AtomicU32::new(0);
static TRIGGER_VK: AtomicU32 = AtomicU32::new(0xA3); // 默认右 Ctrl

const KIND_KEY: u32 = 0;
const KIND_MOUSE_X1: u32 = 1;
const KIND_MOUSE_X2: u32 = 2;
const KIND_MOUSE_MIDDLE: u32 = 3;

/// 目标键当前是否处于「按下」状态。
///
/// 必须有这个闩：按住不放时 Windows 会**自动重复**送 KEYDOWN（默认延迟 ~250ms 后
/// 约每秒 30 次）。不丢掉重复事件，上层就会把每一次重复都当成「新起一句」，
/// 于是不断重置流式上屏的跟踪状态，屏幕上会出现「同一句话一遍遍被打出来」的堆叠。
/// 这是真机上按住说话才暴露的问题 —— 合成单次按下/抬起测不出来。
///
/// 键盘钩子和鼠标钩子**共用这一个闩**：同一时刻只有一个热键生效，
/// 所以两边的按下/抬起不能各记各的，否则换键时状态会对不上。
static KEY_DOWN: AtomicBool = AtomicBool::new(false);
static HOOK: AtomicIsize = AtomicIsize::new(0);
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);
static TX: OnceLock<Mutex<Sender<HookEvent>>> = OnceLock::new();

/// 把触发键写进原子量（托盘换键时调用，立刻生效）。
pub fn set_trigger(t: Trigger) {
    match t {
        Trigger::Key(vk) => {
            TRIGGER_VK.store(vk, Ordering::Relaxed);
            TRIGGER_KIND.store(KIND_KEY, Ordering::Relaxed);
        }
        Trigger::MouseX1 => {
            TRIGGER_KIND.store(KIND_MOUSE_X1, Ordering::Relaxed);
        }
        Trigger::MouseX2 => {
            TRIGGER_KIND.store(KIND_MOUSE_X2, Ordering::Relaxed);
        }
        Trigger::MouseMiddle => {
            TRIGGER_KIND.store(KIND_MOUSE_MIDDLE, Ordering::Relaxed);
        }
    }
    // 换键时清闩：旧键可能正按着，不清就会让新键的第一次按下被当成"自动重复"丢掉。
    KEY_DOWN.store(false, Ordering::Relaxed);
}

/// 读当前触发键（写回配置用）。
pub fn current_trigger() -> Trigger {
    match TRIGGER_KIND.load(Ordering::Relaxed) {
        KIND_MOUSE_X1 => Trigger::MouseX1,
        KIND_MOUSE_X2 => Trigger::MouseX2,
        KIND_MOUSE_MIDDLE => Trigger::MouseMiddle,
        _ => Trigger::Key(TRIGGER_VK.load(Ordering::Relaxed)),
    }
}

/// 把一次按下/抬起交给上层（含闩逻辑）。
///
/// `is_down` = 这次是按下还是抬起。返回 true 表示"这个事件被我们吃掉了"。
fn feed(is_down: bool) -> bool {
    let ev = if is_down {
        // 已经按下过 = 这是自动重复，丢掉（但仍然吃掉这个键）
        if KEY_DOWN.swap(true, Ordering::Relaxed) {
            return true;
        }
        HookEvent::Down
    } else {
        if !KEY_DOWN.swap(false, Ordering::Relaxed) {
            return true; // 没按下过的抬起，一样丢掉
        }
        HookEvent::Up
    };
    if let Some(tx) = TX.get() {
        if let Ok(g) = tx.lock() {
            let _ = g.send(ev);
        }
    }
    true
}

// ---------------------------------------------------------------- SendInput
//
// 只为了「补发 keyup」这一件事 —— 所以刻意最小化，不复用 inject.rs 的那套
// （inject 面向"打文字"，这里面向"松开一个可能卡住的键"，关注点不同）。

const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_KEYUP: u32 = 0x0002;

#[repr(C)]
#[derive(Clone, Copy)]
struct KeybdInput {
    w_vk: u16,
    w_scan: u16,
    dw_flags: u32,
    time: u32,
    dw_extra_info: usize,
}

#[repr(C)]
union InputUnion {
    ki: KeybdInput,
}

#[repr(C)]
struct Input {
    type_: u32,
    u: InputUnion,
}

/// 造一个 keyup 事件。
fn keyup_event(vk: u16) -> Input {
    Input {
        type_: INPUT_KEYBOARD,
        u: InputUnion {
            ki: KeybdInput {
                w_vk: vk,
                w_scan: 0,
                dw_flags: KEYEVENTF_KEYUP,
                time: 0,
                dw_extra_info: 0,
            },
        },
    }
}

#[link(name = "user32")]
extern "system" {
    fn SendInput(c_inputs: u32, p_inputs: *const Input, cb_size: i32) -> u32;
}

#[repr(C)]
struct Msllhookstruct {
    pt_x: i32,
    pt_y: i32,
    mouse_data: u32,
    flags: u32,
    time: u32,
    dw_extra_info: usize,
}

unsafe extern "system" fn hook_proc(code: i32, wparam: usize, lparam: isize) -> isize {
    if code >= 0 && lparam != 0 && TRIGGER_KIND.load(Ordering::Relaxed) == KIND_KEY {
        let kb = lparam as *const Kbdllhookstruct;
        if (*kb).vk_code == TRIGGER_VK.load(Ordering::Relaxed) {
            let eaten = match wparam as u32 {
                WM_KEYDOWN | WM_SYSKEYDOWN => feed(true),
                WM_KEYUP | WM_SYSKEYUP => feed(false),
                _ => false,
            };
            if eaten {
                return 1; // 吃掉这个键，不让目标应用看见
            }
        }
    }
    CallNextHookEx(0, code, wparam, lparam)
}

unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: usize, lparam: isize) -> isize {
    if code >= 0 && lparam != 0 {
        let ms = lparam as *const Msllhookstruct;
        let kind = TRIGGER_KIND.load(Ordering::Relaxed);
        let m = wparam as u32;

        // ⚠️ WM_XBUTTON* 的 XBUTTON 序号在 **HIWORD**，不是低 16 位。
        // 实测（2026-10-17）：合成 XBUTTON1 时 mouseData = 0x00010000，
        // 直接跟 0x0001 比较永远不成立 —— 钩子收到了事件却认不出是谁，侧键热键**静默失效**。
        // 这是 Win32 文档里的既定行为，但很容易写错，且错了没有任何报错。
        let xbtn = ((*ms).mouse_data >> 16) & 0xFFFF;
        let is_down = match kind {
            KIND_MOUSE_X1 => match m {
                WM_XBUTTONDOWN if xbtn == XBUTTON1 => Some(true),
                WM_XBUTTONUP if xbtn == XBUTTON1 => Some(false),
                _ => None,
            },
            KIND_MOUSE_X2 => match m {
                WM_XBUTTONDOWN if xbtn == XBUTTON2 => Some(true),
                WM_XBUTTONUP if xbtn == XBUTTON2 => Some(false),
                _ => None,
            },
            KIND_MOUSE_MIDDLE => match m {
                WM_MBUTTONDOWN => Some(true),
                WM_MBUTTONUP => Some(false),
                _ => None,
            },
            _ => None,
        };

        if let Some(d) = is_down {
            feed(d);
            return 1; // 吃掉，不让它去触发浏览器前进/后退
        }
    }
    CallNextHookEx(0, code, wparam, lparam)
}

/// 把按键名或十六进制 VK 解析成虚拟键码。
pub fn parse_vk(s: &str) -> Result<u32> {
    let l = s.trim().to_ascii_lowercase();
    let v = match l.as_str() {
        "rightctrl" | "rctrl" => 0xA3,
        "leftctrl" | "lctrl" => 0xA2,
        "rightalt" | "ralt" => 0xA5,
        "leftalt" | "lalt" => 0xA4,
        "rightshift" | "rshift" => 0xA1,
        "leftshift" | "lshift" => 0xA0,
        "capslock" => 0x14,
        "f8" => 0x77,
        "f9" => 0x78,
        "f10" => 0x79,
        "`" | "backquote" => 0xC0,
        other => {
            let t = other.trim_start_matches("0x");
            u32::from_str_radix(t, 16).with_context(|| format!("无法解析热键: {s}"))?
        }
    };
    Ok(v)
}

/// 安装钩子。必须在将要跑消息循环的那个线程里调用。
///
/// 两个钩子都装：
///   · 键盘钩子 —— 键触发用；
///   · **鼠标钩子** —— 侧键/中键触发用。键盘钩子收不到鼠标事件，所以鼠标热键必须靠它。
/// 不按当前触发类型挑着装（只装一个）：因为托盘里能**随时换键**，
/// 换一次重装一次会有一瞬间热键失效；两个都常驻就永远不用重装。
/// 代价可以忽略：两个低层钩子都是被动回调，没有键被按时不产生开销。
pub fn install(t: Trigger, tx: Sender<HookEvent>) -> Result<()> {
    set_trigger(t);
    let _ = TX.set(Mutex::new(tx));
    let hmod = unsafe { GetModuleHandleW(std::ptr::null()) };

    let h = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, hook_proc, hmod, 0) };
    if h == 0 {
        anyhow::bail!(
            "键盘钩子装不上（Win32 错误 {}）。换个热键或用管理员身份试试。",
            std::io::Error::last_os_error()
        );
    }
    HOOK.store(h, Ordering::Relaxed);

    // 鼠标钩子失败**不致命**：键盘热键照样能用，只是鼠标键不行。
    let mh = unsafe { SetWindowsHookExW(WH_MOUSE_LL, mouse_hook_proc, hmod, 0) };
    if mh == 0 {
        eprintln!(
            "[sainput] 鼠标钩子装不上（Win32 错误 {}）：鼠标侧键当热键会不可用，键盘热键不受影响。",
            std::io::Error::last_os_error()
        );
    } else {
        MOUSE_HOOK.store(mh, Ordering::Relaxed);
    }
    Ok(())
}

pub fn uninstall() {
    // 先补发 keyup，再卸钩子。
    //
    // 顺序不能反：钩子还活着的时候，系统才把我们的 keyup 当"真实输入"处理；
    // 卸掉之后再发，虽然也有效，但不如趁钩子在时发得干净。
    panic_release();

    let h = HOOK.swap(0, Ordering::Relaxed);
    if h != 0 {
        unsafe {
            UnhookWindowsHookEx(h);
        }
    }
    let mh = MOUSE_HOOK.swap(0, Ordering::Relaxed);
    if mh != 0 {
        unsafe {
            UnhookWindowsHookEx(mh);
        }
    }
}

/// 把可能「卡在按下状态」的键全部补一个 keyup。
///
/// ## 为什么必须有这个
///
/// 低级键盘钩子在热键按下时吃掉事件（`return 1`）。**keyup 也要靠同一个钩子送达**。
/// 如果进程被强行杀掉（`Stop-Process -Force`、任务管理器结束进程、崩溃），
/// Windows 直接卸载钩子 —— keyup 永远丢失，
/// 系统认为该键一直按着：空格打不出来、滚轮变成缩放、整个键盘失灵。
///
/// 这个函数把**所有修饰键 + 当前热键**都补一遍 keyup。
/// 补发一个本来就没按的键是安全的（系统会忽略多余的 keyup）。
///
/// 也可以单独调用：`sainput release-keys`，用于进程已经被强杀后的补救。
pub fn panic_release() {
    // 修饰键 + 常见热键候选。宁可多发，不可漏发 —— 多余的 keyup 无害。
    const VKS: [u16; 14] = [
        0x11, // VK_CONTROL
        0xA2, // VK_LCONTROL
        0xA3, // VK_RCONTROL
        0x12, // VK_MENU
        0xA4, // VK_LMENU
        0xA5, // VK_RMENU
        0x10, // VK_SHIFT
        0xA0, // VK_LSHIFT
        0xA1, // VK_RSHIFT
        0x5B, // VK_LWIN
        0x5C, // VK_RWIN
        0x14, // VK_CAPITAL
        0x08, // VK_BACK
        0x20, // VK_SPACE
    ];
    unsafe {
        // 先把"闩"清掉，避免下一次按下被当成自动重复而吞掉
        KEY_DOWN.store(false, Ordering::Relaxed);
        for vk in VKS {
            let ev = [keyup_event(vk)];
            SendInput(1, ev.as_ptr(), std::mem::size_of::<Input>() as i32);
        }
        // 当前热键如果是键盘键，也补一个
        let tk = TRIGGER_KIND.load(Ordering::Relaxed);
        let tv = TRIGGER_VK.load(Ordering::Relaxed);
        if tk == KIND_KEY && tv != 0 && !VKS.contains(&(tv as u16)) {
            let ev = [keyup_event(tv as u16)];
            SendInput(1, ev.as_ptr(), std::mem::size_of::<Input>() as i32);
        }
    }
}

/// 阻塞跑消息循环（低级钩子必须靠它才能收到回调）。
pub fn run_message_loop() {
    unsafe {
        let mut msg = std::mem::zeroed::<Msg>();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[link(name = "user32")]
extern "system" {
    fn PeekMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32, remove: u32) -> i32;
}

const PM_REMOVE: u32 = 0x0001;

/// 带「每拍回调」的消息循环。
///
/// 为什么需要：`GetMessageW` 会**阻塞**，所以主线程没消息时干不了别的 ——
/// 但托盘菜单点出来的开关要由主线程落盘 + 通知 sidecar，不能等有键盘消息才处理。
/// 这里用 PeekMessage 轮询：没消息就跑一次 `tick`（用户点了开关就这一拍生效）。
///
/// 🔴 **绝对不能用 sleep 等消息**（踩过，代价是用户的鼠标卡成幻灯片）：
///   低级钩子（尤其 WH_MOUSE_LL）是**同步**的 —— Windows 把**每一个**鼠标事件都交给
///   钩子过程，等它返回才继续往下传。而钩子回调是**在这条消息循环里**被派发的。
///   所以这里的任何睡眠，都会变成「每一个鼠标移动事件的额外延迟」。
///   实测 `sleep(40ms)` 把 1000Hz 的鼠标拖到 ~25Hz，手感立刻变成拖影卡顿。
///   正确做法是 `MsgWaitForMultipleObjectsEx` 带超时地等 —— 它**有消息就立刻醒**，
///   没有才睡满超时，鼠标事件永远不用等。
pub fn run_message_loop_with_tick(tick_ms: u32, mut tick: impl FnMut()) {
    const QS_ALLINPUT: u32 = 0x04FF;
    const MWMO_INPUTAVAILABLE: u32 = 0x0004;
    unsafe {
        let mut msg = std::mem::zeroed::<Msg>();
        loop {
            // 先把已排队的消息全部处理掉（有鼠标事件就立刻走这条，不经过等待）
            while PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) > 0 {
                if msg.message == 0x0012 {
                    // WM_QUIT
                    return;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            // 没消息了：有超时地等 —— 新事件一到立即返回，绝不拖住鼠标
            MsgWaitForMultipleObjectsEx(0, std::ptr::null(), tick_ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            // 每次醒过来（不管是超时还是有事件）都跑一拍 tick。
            // 开关处理是幂等的，多跑几次没坏处。
            tick();
        }
    }
}

#[link(name = "user32")]
extern "system" {
    /// 带超时地等消息；`milliseconds` 到点或队列里有输入时返回。
    fn MsgWaitForMultipleObjectsEx(
        count: u32,
        handles: *const isize,
        milliseconds: u32,
        wake_mask: u32,
        flags: u32,
    ) -> u32;
}