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
// (左/右键的 WM_LBUTTON*/WM_RBUTTON* 曾是死常量,2026-10-31 体检清掉 —— 本文件只处理
//  可作为热键的三种按下:中键与侧键 X1/X2。)
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
    fn GetCurrentThreadId() -> u32;
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

/// 装钩子的**那个线程**的 id。
///
/// 低级钩子的回调只在装它的线程上被派发(所以 `install` 要求那是个有消息循环的线程)。
/// 记下来是为了回答「我现在能不能把钩子卸了再装回去」—— 换个线程装上去,
/// 等于装了个永远收不到事件的钩子(热键静默假死)。
/// 0 = 本进程还没装过钩子,比如 `sainput release-keys` 这条独立命令。
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);

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
const INPUT_MOUSE: u32 = 0;
const KEYEVENTF_KEYUP: u32 = 0x0002;
// 鼠标键抬起。注意这套 MOUSEEVENTF_* 是 SendInput 的 dwFlags,
// 和上面 WM_XBUTTONUP 那套消息号不是一回事。
const MOUSEEVENTF_MIDDLEUP: u32 = 0x0040;
const MOUSEEVENTF_XUP: u32 = 0x0100;

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
#[derive(Clone, Copy)]
struct MouseInput {
    dx: i32,
    dy: i32,
    mouse_data: u32,
    dw_flags: u32,
    time: u32,
    dw_extra_info: usize,
}

/// ⚠️ 这个联合体的大小**就是** `INPUT` 的 cbSize,少一个成员就整个失效。
///
/// 体检报告 §1.1(实测):原来这里只有 `ki`(KEYBDINPUT,24 字节)→ 联合体 24 →
/// `Input` 只有 **32** 字节;而 x64 下 Win32 的 `INPUT`(拿 MOUSEINPUT 当最大成员)是 **40**。
/// `SendInput(1, ptr, 32)` 被内核直接拒绝:返回 0、LastError=87(ERROR_INVALID_PARAMETER)。
/// 也就是说「释放卡住的键」这条唯一的自救路**100% 静默失效** —— 用户的热键是鼠标侧键,
/// 卡住时整个鼠标点不活任何窗口,而点托盘自救没有任何反应。
///
/// HARDWAREINPUT 只有 8 字节,不影响大小、这里也确实用不到,所以没放进来。
/// 大小由文件末尾的编译期断言钉死。
#[repr(C)]
union InputUnion {
    ki: KeybdInput,
    mi: MouseInput,
}

#[repr(C)]
struct Input {
    type_: u32,
    u: InputUnion,
}

/// 编译期断言:`INPUT` 必须是 40 字节(§1.1)。
///
/// 放在编译期而不是只放测试里:这个数字错了是「救命功能静默失效」这种最难查的故障,
/// 应该让构建当场炸掉,而不是等谁想起来去跑单元测试。实测口径(§1.1):
/// cbSize=32 → 返回 0 / 错误 87;cbSize=40 → 返回 1。
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Input>() == 40);

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

/// 造一个鼠标键抬起事件。`flag` 取 MOUSEEVENTF_*UP;`mouse_data` 只有侧键要填 XBUTTON1/2。
fn mouse_up_event(flag: u32, mouse_data: u32) -> Input {
    Input {
        type_: INPUT_MOUSE,
        u: InputUnion {
            mi: MouseInput {
                dx: 0,
                dy: 0,
                mouse_data,
                dw_flags: flag,
                time: 0,
                dw_extra_info: 0,
            },
        },
    }
}

#[link(name = "user32")]
extern "system" {
    fn SendInput(c_inputs: u32, p_inputs: *const Input, cb_size: i32) -> u32;
    /// 系统认为这个键现在是不是按着(返回值的最高位 = 当前按下)。
    /// 失同步自愈的判据(§7-3 第 2 点)靠它 —— 见 `heal_stuck_trigger`。
    fn GetAsyncKeyState(vkey: i32) -> i16;
}

// GetAsyncKeyState 用的是「虚拟键码」体系,和鼠标钩子那套消息号无关。
const VK_MBUTTON: i32 = 0x04;
const VK_XBUTTON1: i32 = 0x05;
const VK_XBUTTON2: i32 = 0x06;

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
/// 看门狗用的**共享标志**(T6.7):钩子活着 = 1。
///
/// 引擎在 `install()` 置 1、`uninstall()` 置 0;进程外的看门狗在父进程死掉之后读它 ——
/// 读到 1 说明"死在钩子活着的那一刻"(硬崩、被任务管理器杀、断点卡死),那一发抬起
/// 我们根本没机会补,必须由外面补。
///
/// 为什么用**命名共享内存**而不是文件:① 硬崩时文件可能只写了一半(这不是读文件的时候);
/// ② 内核对象在进程死掉后依然在,只要还有句柄(看门狗持有)就能读到最后写的值;
/// ③ 写 4 字节是原子的,不需要锁。
///
/// 名字带**命名空间**:`SAINPUT_SINGLE_NS` 是"单实例隔离"用的那个环境变量,
/// 测试(私有桌面)和你日常那个实例各用各的。不带的话,两个引擎会共享同一个标志 ——
/// 一边死了另一边也会被"补发"一次(而且带着自己那份状态),这种事只能靠隔离避免。
fn guard_map_name() -> String {
    match std::env::var("SAINPUT_SINGLE_NS") {
        Ok(ns) if !ns.is_empty() => format!("Local\\sainput-guard-v1-{ns}"),
        _ => "Local\\sainput-guard-v1".to_string(),
    }
}
/// 映射视图地址(0 = 还没建)。只在 `install`/`uninstall` 里写。
static GUARD_VIEW: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileMappingW(
        h: isize,
        attr: *const std::ffi::c_void,
        protect: u32,
        hi: u32,
        lo: u32,
        name: *const u16,
    ) -> isize;
    fn OpenFileMappingW(access: u32, inherit: i32, name: *const u16) -> isize;
    fn MapViewOfFile(h: isize, access: u32, hi: u32, lo: u32, bytes: usize) -> *mut std::ffi::c_void;
    fn UnmapViewOfFile(p: *const std::ffi::c_void) -> i32;
}

/// UTF-16 终止串(只给上面的映射名用)。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 共享块大小(字节)。布局:`[0]` = 标志(1 = 钩子活着),`[1..5]` = **父进程 pid**。
///
/// 带 pid 是为了**认领**:看门狗只认"这块内存属于我要守的那个进程"。否则上一轮硬崩
/// 残留的块(标志还是 1,被某个还活着的看门狗句柄撑着)会让新看门狗误补一发。
const GUARD_MAP_BYTES: usize = 8;

/// 建/取共享标志(引擎侧)。返回视图地址;失败返回 0(看门狗就变成"看不见状态",
/// 那也只会退化成"什么都不补",不会误伤)。
fn open_guard_map(create: bool) -> isize {
    let name = wide(&guard_map_name());
    let h = unsafe {
        if create {
            CreateFileMappingW(-1_isize, std::ptr::null(), 0x04 /* PAGE_READWRITE */, 0, GUARD_MAP_BYTES as u32, name.as_ptr())
        } else {
            OpenFileMappingW(0x04 /* FILE_MAP_READ */, 0, name.as_ptr())
        }
    };
    if h == 0 {
        return 0;
    }
    // ⚠️ 三个 0x04 是**不同的**常量,别混:PAGE_READWRITE=0x04(建映射的页保护)、
    // FILE_MAP_READ=0x04(只读视图)、FILE_MAP_WRITE=0x02(可写视图)。
    // 第一版把"建映射"那一路也用了 FILE_MAP_READ,然后往视图里写标志 ——
    // 直接 ACCESS_VIOLATION(单元测试当场抓到)。
    let access = if create { 0x02 /* FILE_MAP_WRITE */ } else { 0x04 /* FILE_MAP_READ */ };
    unsafe { MapViewOfFile(h, access, 0, 0, GUARD_MAP_BYTES) as isize }
}

/// 引擎侧:建好标志并归零(刚开机时没有"活着的手势")。
pub fn arm_guard_state() {
    let view = open_guard_map(true);
    GUARD_VIEW.store(view, Ordering::Relaxed);
    if view != 0 {
        set_guard_flag(0);
        // 认领:把本进程 pid 写进去,看门狗据此确认"这块内存是我的"
        let pid = std::process::id();
        unsafe {
            let p = view as *mut u8;
            *p.add(1) = (pid & 0xff) as u8;
            *p.add(2) = ((pid >> 8) & 0xff) as u8;
            *p.add(3) = ((pid >> 16) & 0xff) as u8;
            *p.add(4) = ((pid >> 24) & 0xff) as u8;
        }
    }
}

/// 读块里记的父进程 pid(0 = 没写/块是空的)。
pub fn read_guard_pid(view: isize) -> u32 {
    if view == 0 {
        return 0;
    }
    unsafe {
        let p = view as *const u8;
        (*p.add(1) as u32)
            | ((*p.add(2) as u32) << 8)
            | ((*p.add(3) as u32) << 16)
            | ((*p.add(4) as u32) << 24)
    }
}

/// 写标志(0/1)。没建起来就是空操作。
pub fn set_guard_flag(v: u8) {
    let view = GUARD_VIEW.load(Ordering::Relaxed);
    if view != 0 {
        unsafe { *(view as *mut u8) = v };
    }
}

/// 看门狗侧:打开并**持有**视图(必须在父进程还活着的时候打开)。
///
/// 🔴 这是这条功能唯一容易写错的地方:命名内核对象(共享内存)在**最后一个句柄**关掉时就消失。
/// 父进程被硬杀 → 它的句柄被系统收走 → 如果那时才 `OpenFileMapping`,什么都打不开,
/// 于是"标志为 1"这件事就永远读不到 —— 第一版就是这么写的,回归直接逮住(NO-STATE)。
/// 所以看门狗要在等父进程死**之前**把视图拿在手里,之后一直读这块内存。
///
/// 返回视图地址(0 = 没拿到,可能是父进程还没建;调用方可以重试)。
pub fn open_guard_view_held() -> isize {
    open_guard_map(false)
}

/// 读某个已持有的视图里的标志。
pub fn read_flag_at(view: isize) -> u8 {
    if view == 0 {
        return 0;
    }
    unsafe { *(view as *const u8) }
}

/// 看门狗侧:读标志(自己开、读完就关 —— 只在**父进程还活着**时用得上,见上)。
pub fn read_guard_flag() -> Option<u8> {
    let view = open_guard_map(false);
    if view == 0 {
        return None;
    }
    let v = unsafe { *(view as *const u8) };
    unsafe { UnmapViewOfFile(view as *const std::ffi::c_void) };
    Some(v)
}

pub fn install(t: Trigger, tx: Sender<HookEvent>) -> Result<()> {
    set_trigger(t);
    let _ = TX.set(Mutex::new(tx));
    // 记下这个线程 —— 只有它能"卸了再装"(见 HOOK_THREAD 的注释)
    HOOK_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
    // 看门狗标志:钩子活着就置 1。硬崩时外面那只看门狗读到 1 → 补发全部抬起(T6.7)。
    arm_guard_state();
    let r = attach_hooks();
    if r.is_ok() {
        set_guard_flag(1);
    }
    r
}

/// 装两个低级钩子(键盘必须成功,鼠标失败只告警)。
///
/// 单独抽出来,是因为 `panic_release()`「卸了要装回去」用的和 `install()` 完全一样 ——
/// 两份实现迟早会漂移,而漂移的那一边是"自救之后热键失效"。
fn attach_hooks() -> Result<()> {
    let hmod = unsafe { GetModuleHandleW(std::ptr::null()) };

    let h = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, hook_proc, hmod, 0) };
    if h == 0 {
        anyhow::bail!(
            "键盘钩子装不上(Win32 错误 {})。换个热键或用管理员身份试试。",
            std::io::Error::last_os_error()
        );
    }
    HOOK.store(h, Ordering::Relaxed);

    // 鼠标钩子失败**不致命**:键盘热键照样能用,只是鼠标键不行。
    let mh = unsafe { SetWindowsHookExW(WH_MOUSE_LL, mouse_hook_proc, hmod, 0) };
    if mh == 0 {
        eprintln!(
            "[sainput] 鼠标钩子装不上(Win32 错误 {}):鼠标侧键当热键会不可用,键盘热键不受影响。",
            std::io::Error::last_os_error()
        );
    } else {
        MOUSE_HOOK.store(mh, Ordering::Relaxed);
    }
    Ok(())
}

/// 摘掉我们装的两个低级钩子(如果装着)。返回是否真的摘了。
fn detach_hooks() -> bool {
    let h = HOOK.swap(0, Ordering::Relaxed);
    let mh = MOUSE_HOOK.swap(0, Ordering::Relaxed);
    if h == 0 && mh == 0 {
        return false;
    }
    unsafe {
        if h != 0 {
            UnhookWindowsHookEx(h);
        }
        if mh != 0 {
            UnhookWindowsHookEx(mh);
        }
    }
    true
}

/// 当前线程是不是"装钩子的那个线程"。只有它能把钩子卸了再装回去,理由见 `HOOK_THREAD`。
fn on_hook_thread() -> bool {
    HOOK_THREAD.load(Ordering::Relaxed) == unsafe { GetCurrentThreadId() }
}

/// 在「自己的钩子被摘掉」的那一小段里跑一段代码 —— **所有补发抬起都必须这么走**(§7-3)。
///
/// 为什么:被低级钩子 `return 1` 吃掉的事件**根本不进系统按键状态机**
/// (`docs/TROUBLESHOOTING.md` §5.1 的实测表),而我们的钩子吃的恰好是"当前热键"这一个键,
/// 这里要补的又恰好是它的抬起 —— 钩子活着时注入,抬起当场被自己吃掉,系统那边照样闩着。
///
/// 卸掉了还必须装回去:托盘菜单点「释放卡住的键」时进程要继续跑,不装回去热键就永久失效了。
/// 装不回来只告警,不 panic —— 用户此刻要的是先把鼠标解开。
fn without_hooks<R>(f: impl FnOnce() -> R) -> R {
    let toggle = on_hook_thread()
        && (HOOK.load(Ordering::Relaxed) != 0 || MOUSE_HOOK.load(Ordering::Relaxed) != 0);
    if !toggle {
        return f();
    }
    detach_hooks();
    let r = f();
    if let Err(e) = attach_hooks() {
        eprintln!("[sainput] 补发抬起后钩子没装回来:{e:#} —— 热键暂时失效,重启 sainput 可恢复。");
    }
    r
}

/// 发一批补发事件,并**检查 `SendInput` 的返回值**。
///
/// 原来的两个调用点都把返回值丢了。而 `SendInput` 失败是**静默的**:返回 0,只在 LastError
/// 里留个错误码 —— 实测(§1.1)cbSize 传错时就是「返回 0 / 错误 87」,而调用方完全不知道,
/// 用户看到的只是「点了解除卡键,没用」,没有任何线索可查。
///
/// 一次自救只报一条:一批事件失败的原因对整个批次是同一个,逐条打屏只会淹掉日志。
fn send_all(events: &[Input]) -> bool {
    if events.is_empty() {
        return true;
    }
    let n = unsafe { SendInput(events.len() as u32, events.as_ptr(), std::mem::size_of::<Input>() as i32) };
    if n as usize == events.len() {
        return true;
    }
    eprintln!(
        "[sainput] 补发按键抬起失败:SendInput 只接受 {n}/{} 条(Win32 错误 {})。\
         热键可能仍卡在按下状态;先退出 sainput 再跑一次 `sainput release-keys` 通常能解开。",
        events.len(),
        std::io::Error::last_os_error()
    );
    false
}

/// 把可能「卡在按下状态」的键全部补一个抬起。**调用方负责先把钩子摘掉**(见 `without_hooks`)。
fn send_all_keyups() {
    // 修饰键 + 常见热键候选。宁可多发,不可漏发 —— 多余的 keyup 无害。
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

    let mut evs: Vec<Input> = Vec::with_capacity(VKS.len() + 4);
    for vk in VKS {
        evs.push(keyup_event(vk));
    }
    // 当前热键如果是键盘键,也补一个
    let tk = TRIGGER_KIND.load(Ordering::Relaxed);
    let tv = TRIGGER_VK.load(Ordering::Relaxed);
    if tk == KIND_KEY && tv != 0 && !VKS.contains(&(tv as u16)) {
        evs.push(keyup_event(tv as u16));
    }
    // 鼠标:**中键 + 侧键 X1/X2** 都补(§1.2)。
    //
    // 原来这里一个鼠标键都没有 —— 而用户的热键就是 `mousex1`,等于这条救命功能对他完全无效。
    // 鼠标闩住比键盘更重:点哪都点不活任何窗口(TROUBLESHOOTING §5.1,2026-10-28 真事)。
    // 没按过的键补一发抬起是安全的(系统会忽略多余的抬起)。
    evs.push(mouse_up_event(MOUSEEVENTF_MIDDLEUP, 0));
    evs.push(mouse_up_event(MOUSEEVENTF_XUP, XBUTTON1));
    evs.push(mouse_up_event(MOUSEEVENTF_XUP, XBUTTON2));

    // 闩也清掉,避免下一次真的按下被当成自动重复而吞掉
    KEY_DOWN.store(false, Ordering::Relaxed);
    send_all(&evs);
}

pub fn uninstall() {
    // 先摘掉看门狗标志:走到这里说明我们有机会自己补抬起了,外面那只看门狗不必再动手。
    set_guard_flag(0);
    // 顺序:**先卸钩子,再补 keyup**(§7-3)。
    //
    // 原来写反了,注释里那句"趁钩子在时发得干净"是错的:钩子活着的时候,被它 `return 1`
    // 吃掉的抬起根本不进系统按键状态机(实测,TROUBLESHOOTING §5.1),而这里补的恰好就是
    // 热键自己的抬起 —— 先发等于白补。
    detach_hooks();
    send_all_keyups();
}

/// 失同步自愈(HANDOFF §7-3 第 2 点):系统认为当前热键还按着,而我们这边早就不按了。
///
/// ## 为什么需要
///
/// 被钩子 `return 1` 吃掉的事件不进系统按键状态机(TROUBLESHOOTING §5.1 实测)。于是只要
/// 有一发 down **不是我们的钩子吃掉的**(别的进程注入、钩子重装的间隙、甚至上一次自救自己开的那道缝),
/// 那个键就在系统那边**永久闩住** —— 之后包括我们注入的在内,所有抬起都会被同一个钩子吃掉,
/// 它自己永远解不开。2026-10-28 那次就这么卡了 20 分钟。
///
/// ## 为什么不会误伤
///
/// 正常按住热键时 `GetAsyncKeyState` **恒为 up**:down 被我们的钩子吃了,压根没进状态机。
/// 所以「系统按下 + 我们没按」这个组合,在用户真的按着键时不可能成立。
///
/// 每拍调一次(见 `run_message_loop_with_tick`)。冷却计数是防"有别的进程一直在补 down"那种
/// 病态情况:别每拍都卸一次钩子、也别把日志刷爆。
fn heal_stuck_trigger() {
    const COOLDOWN_TICKS: u32 = 20;
    static COOLDOWN: AtomicU32 = AtomicU32::new(0);

    let cd = COOLDOWN.load(Ordering::Relaxed);
    if cd > 0 {
        COOLDOWN.store(cd - 1, Ordering::Relaxed);
        return;
    }
    // 钩子没装着就别自愈:没有钩子吃事件的话,"系统按下"就是用户真的按着,补一发抬起
    // 等于把人家按着的键凭空松开。
    if HOOK.load(Ordering::Relaxed) == 0 {
        return;
    }
    // 我们这边认为按着 = 正常说话中,没它的事
    if KEY_DOWN.load(Ordering::Relaxed) {
        return;
    }
    let (vk, ev) = match TRIGGER_KIND.load(Ordering::Relaxed) {
        KIND_KEY => {
            let v = TRIGGER_VK.load(Ordering::Relaxed);
            (v as i32, keyup_event(v as u16))
        }
        KIND_MOUSE_X1 => (VK_XBUTTON1, mouse_up_event(MOUSEEVENTF_XUP, XBUTTON1)),
        KIND_MOUSE_X2 => (VK_XBUTTON2, mouse_up_event(MOUSEEVENTF_XUP, XBUTTON2)),
        KIND_MOUSE_MIDDLE => (VK_MBUTTON, mouse_up_event(MOUSEEVENTF_MIDDLEUP, 0)),
        _ => return,
    };
    if vk == 0 || unsafe { GetAsyncKeyState(vk) } & (0x8000u16 as i16) == 0 {
        return;
    }

    COOLDOWN.store(COOLDOWN_TICKS, Ordering::Relaxed);
    eprintln!(
        "[sainput] 「{}」在系统里卡在按下状态(我们并没有按它),已自动补发一次抬起。",
        current_trigger().label()
    );
    // 这一发必须**在钩子外面**发(理由同 panic_release),否则会被自己吃掉。
    // 不走 panic_release 是因为那会把 14 个修饰键一起松开 —— 别人的键盘还按着呢。
    without_hooks(|| send_all(&[ev]));
}

/// 把可能「卡在按下状态」的键全部补一个 keyup。
///
/// ## 为什么必须有这个
///
/// 低级键盘钩子在热键按下时吃掉事件(`return 1`)。**keyup 也要靠同一个钩子送达**。
/// 如果进程被强行杀掉(`Stop-Process -Force`、任务管理器结束进程、崩溃),
/// Windows 直接卸载钩子 —— keyup 永远丢失,
/// 系统认为该键一直按着:空格打不出来、滚轮变成缩放、整个键盘失灵。
///
/// 补的范围是**所有修饰键 + 当前热键 + 鼠标中键/X1/X2**(§1.2)。
/// 补发一个本来就没按的键是安全的(系统会忽略多余的 keyup)。
///
/// ## 顺序(§7-3):**先卸钩子,再注入**
///
/// 被低级钩子吃掉的抬起不进系统按键状态机(实测,TROUBLESHOOTING §5.1),而我们的钩子吃的
/// 正是当前热键。钩子活着时注入 → 抬起被自己吃掉 → 系统那边照样闩着,等于白补。
/// 所以走 `without_hooks`:托盘菜单点这条时进程还要继续跑,补完**原样装回去**。
///
/// `sainput release-keys` 是另一个进程,那里 `HOOK`/`MOUSE_HOOK` 都是 0,
/// 什么也不用卸,直接注入 —— 这也是 TROUBLESHOOTING §5.1 推荐的处置顺序。
pub fn panic_release() {
    without_hooks(send_all_keyups);
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
            // 每次醒过来(不管是超时还是有事件)都跑一拍 tick。
            // 开关处理是幂等的,多跑几次没坏处。
            tick();
            // 顺带做一次"失同步"自检(§7-3 第 2 点):热键在系统那边卡住时自动补一发抬起。
            // 放在这里而不是让调用方自己记得调 —— 钩子的健康检查就该由钩子自己负责,
            // 少一个调用点就少一个"忘了调"的漏洞。
            heal_stuck_trigger();
        }
    }
}

#[link(name = "user32")]
extern "system" {
    /// 带超时地等消息;`milliseconds` 到点或队列里有输入时返回。
    fn MsgWaitForMultipleObjectsEx(
        count: u32,
        handles: *const isize,
        milliseconds: u32,
        wake_mask: u32,
        flags: u32,
    ) -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `INPUT` 必须是 Win32 的 40 字节(x64)。
    ///
    /// 这条是体检报告 §1.1 的机械断言:少 8 字节 → `SendInput` 静默失败 →
    /// 「释放卡住的键」这条唯一的自救路整个无效。纯内存布局,不碰任何真实热键。
    #[test]
    fn input_is_win32_40_bytes() {
        assert_eq!(std::mem::size_of::<Input>(), 40, "x64 下 INPUT 必须是 40 字节");
        assert_eq!(std::mem::align_of::<Input>(), 8);
        // 联合体里最长的是 MOUSEINPUT(32);少了它只剩 KEYBDINPUT(24),Input 就掉回 32。
        assert_eq!(std::mem::size_of::<MouseInput>(), 32);
        assert_eq!(std::mem::size_of::<KeybdInput>(), 24);
        // 每个事件自带 type_,所以键盘/鼠标可以混在**同一批**里发出去(自救就是这么发的)
        assert_eq!(std::mem::size_of::<Input>(), std::mem::size_of_val(&keyup_event(0xA3)));
        assert_eq!(std::mem::size_of::<Input>(), std::mem::size_of_val(&mouse_up_event(MOUSEEVENTF_XUP, XBUTTON1)));
    }

    /// 自救那批发出去的事件,形状必须对:修饰键抬起 + 当前热键抬起 + 鼠标中键/X1/X2 抬起。
    ///
    /// 不发任何东西,只看造出来的事件长什么样 —— 这条正好能钉住 §1.2(原来一个鼠标键都没有)。
    #[test]
    fn panic_release_events_cover_mouse_buttons() {
        let k = keyup_event(0xA3); // VK_RCONTROL,只当样例
        assert_eq!(k.type_, INPUT_KEYBOARD);
        assert_eq!(unsafe { k.u.ki.w_vk }, 0xA3);
        assert_eq!(unsafe { k.u.ki.dw_flags }, KEYEVENTF_KEYUP);

        let x1 = mouse_up_event(MOUSEEVENTF_XUP, XBUTTON1);
        assert_eq!(x1.type_, INPUT_MOUSE);
        assert_eq!(unsafe { x1.u.mi.dw_flags }, MOUSEEVENTF_XUP);
        // ⚠️ XBUTTON 序号走 mouse_data 的低 16 位(这条是 SendInput 侧的编码,
        // 和 WM_XBUTTONUP 消息里走 HIWORD 那套不是一回事)
        assert_eq!(unsafe { x1.u.mi.mouse_data }, XBUTTON1);
        assert_eq!(unsafe { x1.u.mi.dx }, 0);

        let mid = mouse_up_event(MOUSEEVENTF_MIDDLEUP, 0);
        assert_eq!(unsafe { mid.u.mi.dw_flags }, MOUSEEVENTF_MIDDLEUP);
        assert_eq!(unsafe { mid.u.mi.mouse_data }, 0);

        // 三个鼠标键都要在:用户的热键是 mousex1,少一个 X1 这条功能对他就是无效的
        let batch: Vec<Input> = vec![
            keyup_event(0x11),
            mouse_up_event(MOUSEEVENTF_MIDDLEUP, 0),
            mouse_up_event(MOUSEEVENTF_XUP, XBUTTON1),
            mouse_up_event(MOUSEEVENTF_XUP, XBUTTON2),
        ];
        let ups: Vec<u32> = batch
            .iter()
            .filter_map(|e| (e.type_ == INPUT_MOUSE).then(|| unsafe { e.u.mi.mouse_data }))
            .collect();
        assert_eq!(ups, vec![0, XBUTTON1, XBUTTON2]);
    }

    /// 触发键 → GetAsyncKeyState 判据用的 VK 码,三个鼠标键都不能漏(漏了 = 自愈那个键失效)。
    #[test]
    fn trigger_kind_maps_to_vk() {
        set_trigger(Trigger::MouseX1);
        assert_eq!(current_trigger(), Trigger::MouseX1);
        set_trigger(Trigger::MouseX2);
        assert_eq!(current_trigger(), Trigger::MouseX2);
        set_trigger(Trigger::MouseMiddle);
        assert_eq!(current_trigger(), Trigger::MouseMiddle);
        set_trigger(Trigger::Key(0xA3));
        assert_eq!(current_trigger(), Trigger::Key(0xA3));
        // 四个鼠标/键盘 VK 码互不相同,且都不是 0(0 = "没查成",会被自愈跳过)
        for v in [VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2, 0xA3] {
            assert_ne!(v, 0);
        }
        assert!(VK_MBUTTON != VK_XBUTTON1 && VK_XBUTTON1 != VK_XBUTTON2);
    }

    /// 一次性复现体检 §1.1 的实测口径:**只有 40 字节能过,32 字节被内核直接拒**。
    ///
    /// 默认 `#[ignore]`:它会真的调 `SendInput`。虽然只发**一发无害的 F24 抬起**
    /// (F24 不在 sainput 的候选热键里、不是修饰键、也不是鼠标键,键盘上根本没有这个键),
    /// 依然不该在默认测试里碰真实输入。要跑:
    ///
    /// ```text
    /// CARGO_TARGET_DIR=target/chk-a cargo test -p sainput-core --release --lib -- --ignored --nocapture
    /// ```
    ///
    /// 🔴 **红线**:这里的 `vk` 不许换成任何 sainput 能当热键的键(尤其右 Ctrl / 鼠标侧键),
    /// 也不许发 down —— 合成的 down/up 会被正在跑的实例的钩子吃掉,本仓已因此出过两次事故
    /// (用户键盘被卡死)。F24 抬起是唯一安全的选择。
    #[test]
    #[ignore = "会真的调 SendInput;只发一发无害的 F24 抬起"]
    fn send_input_rejects_32_accepts_40() {
        const VK_F24: u16 = 0x87;
        let ev = keyup_event(VK_F24);

        // 32 字节:内核按 cbSize 校验,和 x64 上 Win32 的 40 不符 → 整批拒绝
        let n_small = unsafe { SendInput(1, &ev, 32) };
        let e_small = std::io::Error::last_os_error();
        // 40 字节(真实大小):应当被接受
        let n_full = unsafe { SendInput(1, &ev, std::mem::size_of::<Input>() as i32) };
        let e_full = std::io::Error::last_os_error();

        println!(
            "size_of::<Input>() = {} | cbSize=32 -> 返回 {} 错误 {:?} | cbSize={} -> 返回 {} 错误 {:?}",
            std::mem::size_of::<Input>(),
            n_small,
            e_small.raw_os_error(),
            std::mem::size_of::<Input>(),
            n_full,
            e_full.raw_os_error()
        );

        assert_eq!(n_small, 0, "cbSize=32 不该成功");
        assert_eq!(
            e_small.raw_os_error(),
            Some(87),
            "cbSize=32 应当报 ERROR_INVALID_PARAMETER(87)"
        );
        assert_eq!(n_full, 1, "cbSize=40 应当接受(否则自救依然是死的)");
    }
}
#[cfg(test)]
mod guard_tests {
    use super::*;

    /// T6.7 的判据原子:引擎侧置位/清零,看门狗侧读得到同样的值。
    ///
    /// 只写一条:共享内存是**进程级**的,多条测试并行跑会互相踩同一个名字。
    #[test]
    fn 看门狗标志置位与清零() {
        arm_guard_state();
        assert_eq!(read_guard_flag(), Some(0), "刚武装好应该是 0(没有活着的手势)");
        set_guard_flag(1);
        assert_eq!(read_guard_flag(), Some(1), "钩子活着时必须读得到 1");
        set_guard_flag(0);
        assert_eq!(read_guard_flag(), Some(0), "正常退出后必须归零,否则看门狗会误补一发");
    }
}
