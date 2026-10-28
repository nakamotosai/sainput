//! 上屏：SendInput + 流式改写。
//!
//! 手写 Win32 绑定而不是用 windows crate —— 这几个结构体的 ABI 十几年没变，
//! 手工声明比跟 crate 版本较劲更可靠。

use anyhow::Result;

const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const KEYEVENTF_UNICODE: u32 = 0x0004;
const VK_BACK: u16 = 0x08;

#[repr(C)]
#[derive(Clone, Copy)]
struct KEYBDINPUT {
    w_vk: u16,
    w_scan: u16,
    dw_flags: u32,
    time: u32,
    dw_extra_info: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MOUSEINPUT {
    dx: i32,
    dy: i32,
    mouse_data: u32,
    dw_flags: u32,
    time: u32,
    dw_extra_info: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HARDWAREINPUT {
    u_msg: u32,
    w_param_l: u16,
    w_param_h: u16,
}

#[repr(C)]
union InputUnion {
    mi: MOUSEINPUT,
    ki: KEYBDINPUT,
    hi: HARDWAREINPUT,
}

#[repr(C)]
struct Input {
    type_: u32,
    u: InputUnion,
}

#[link(name = "user32")]
extern "system" {
    fn SendInput(c_inputs: u32, p_inputs: *const Input, cb_size: i32) -> u32;
}

fn unicode_event(unit: u16, keyup: bool) -> Input {
    Input {
        type_: INPUT_KEYBOARD,
        u: InputUnion {
            ki: KEYBDINPUT {
                w_vk: 0,
                w_scan: unit,
                dw_flags: KEYEVENTF_UNICODE | if keyup { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dw_extra_info: 0,
            },
        },
    }
}

fn vk_event(vk: u16, keyup: bool) -> Input {
    Input {
        type_: INPUT_KEYBOARD,
        u: InputUnion {
            ki: KEYBDINPUT {
                w_vk: vk,
                w_scan: 0,
                dw_flags: if keyup { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dw_extra_info: 0,
            },
        },
    }
}

fn send(inputs: &[Input]) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let n = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<Input>() as i32) };
    if n as usize != inputs.len() {
        anyhow::bail!("SendInput 只接受了 {n}/{} 个事件（目标窗口可能拒绝注入）", inputs.len());
    }
    Ok(())
}

/// 用 KEYEVENTF_UNICODE 打一段文本（支持中文）。
pub fn type_text(s: &str) -> Result<()> {
    let mut v: Vec<Input> = Vec::with_capacity(s.len() * 2);
    for u in s.encode_utf16() {
        v.push(unicode_event(u, false));
        v.push(unicode_event(u, true));
    }
    send(&v)
}

/// 退格 n 次。
pub fn backspace(n: usize) -> Result<()> {
    let mut v: Vec<Input> = Vec::with_capacity(n * 2);
    for _ in 0..n {
        v.push(vk_event(VK_BACK, false));
        v.push(vk_event(VK_BACK, true));
    }
    send(&v)
}

// ------------------------------------------------------------------ 上屏方式
//
// 为什么需要第二条路(2026-10-28 实测):目标窗口所在线程挂着中文/日文输入法时,
// **注入的字母会被输入法当成它自己的输入** —— 落进它的组字串,弹出候选面板,
// 程序一个字都收不到;中文汉字反倒不受影响(输入法不吃非字母)。
// 于是「说英文它就被吃掉 / 全删掉」。这不是模型的问题:同一段音频,识别原文是完整的。
//
// 输入法的状态读不到也改不了:跨进程 `ImmGetContext(目标窗口)` 返回 NULL;
// 用 `WM_INPUTLANGCHANGEREQUEST` 把窗口切到别的键盘布局、或者改 IMM32 的
// open/conversion 状态,输入法照样把字母收走。所以对这类窗口改走
// **PostMessage(WM_CHAR)**:消息不经过键盘那条链,输入法看不到,
// 实测在经典文本控件上「无焦点也能完整落字 + 退格照常」。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyMode {
    /// SendInput + KEYEVENTF_UNICODE(默认路径,绝大多数窗口都对)
    Keys,
    /// 走窗口消息、**照常流式**:字符用 PostMessage(WM_CHAR),退格用
    /// PostMessage(WM_KEYDOWN/UP, VK_BACK)。同一个消息队列,先后顺序天然一致。
    WmChar(isize),
    /// 走窗口消息、**整句一次投递**(不流式):给「连消息里的退格都不认」的程序兜底。
    /// 代价是松手才出字,但没有退格就不存在「记账 vs 屏幕」的偏差。
    WmCharFinal(isize),
    /// **Pebrel 控制面**:一个按键都不发。整句塞进它自己的组合态缓冲层
    /// (`pane.compose` 的 preedit,可无限次整体替换),定稿时才作为一次写入提交给
    /// PTY。见 `crate::pebrel`。
    Pebrel,
}

impl KeyMode {
    /// 走窗口消息(不管流不流式)
    pub fn is_wmchar(self) -> bool {
        matches!(self, KeyMode::WmChar(_) | KeyMode::WmCharFinal(_))
    }
    /// 只允许定稿时整句投递
    pub fn is_final_only(self) -> bool {
        matches!(self, KeyMode::WmCharFinal(_))
    }
    /// 走应用自己的组合态缓冲层(不产生按键)
    pub fn is_compose(self) -> bool {
        matches!(self, KeyMode::Pebrel)
    }
    fn hwnd(self) -> isize {
        match self {
            KeyMode::Keys => 0,
            KeyMode::Pebrel => 0,
            KeyMode::WmChar(h) | KeyMode::WmCharFinal(h) => h,
        }
    }
}

/// 日志用:当前方式打在哪个窗口上。
pub fn describe_target(mode: KeyMode) -> String {
    match mode {
        KeyMode::Keys => "键盘注入".to_string(),
        KeyMode::WmChar(h) => format!("消息通道(流式){}", target::describe(h)),
        KeyMode::WmCharFinal(h) => format!("消息通道(整句一次){}", target::describe(h)),
        KeyMode::Pebrel => "Pebrel 控制面(组合态缓冲层)".to_string(),
    }
}

/// 该给这个窗口用哪种上屏方式。纯函数,好测。
///
/// `policy` 是配置里的 `ime_wmchar`,四种写法:
///   `off` / `keys`            —— 永远走键盘注入(老行为)
///   `on`  / `wmchar`          —— 永远走 WM_CHAR(拿不到窗口句柄时退回键盘)
///   `on:pebrel.exe,zed.exe`   —— **只对这几个程序**走消息通道(流式),别的窗口照旧
///   `off:some.exe`            —— 这几个程序永远走键盘注入,别的按 auto
///   `nostream[:<exe>,...]`    —— 走消息通道但**整句一次投递**(连消息退格都不认的程序用)
///   `pebrel[:<exe>,...]`      —— 走 Pebrel 控制面的**组合态缓冲层**(一个按键都不发)。
///                               端点在本机只有一份,所以不写名单时只对 Pebrel 自己的
///                               程序名(pebrel.exe / nebula.exe)生效;名单外照旧走键盘。
///   `auto`(默认,等于没写)   —— 目标线程挂着输入法 **且** 焦点是经典文本控件时走
///                              WM_CHAR。只对「已实测字母会被吃掉、而 WM_CHAR 已验证
///                              能落字」的这一类生效,免得在别的窗口上把本来好用的
///                              注入方式换掉。
///
/// `exe` 是目标进程的可执行文件名(小写,含 `.exe`),用来匹配名单。
pub fn decide_key_mode(
    policy: &str,
    hwnd: isize,
    thread_has_ime: bool,
    classic_control: bool,
    exe: &str,
) -> KeyMode {
    let exe = exe.to_ascii_lowercase();
    let (head, list) = match policy.split_once(':') {
        Some((h, l)) => (h.trim(), l.trim()),
        None => (policy.trim(), ""),
    };
    let listed = |list: &str| -> bool {
        list.split(',')
            .map(|x| x.trim().to_ascii_lowercase())
            .any(|x| !x.is_empty() && x == exe)
    };
    let only_listed = !list.is_empty();
    let wmchar = |yes: bool| {
        if yes && hwnd != 0 {
            KeyMode::WmChar(hwnd)
        } else {
            KeyMode::Keys
        }
    };
    let wmchar_final = |yes: bool| {
        if yes && hwnd != 0 {
            KeyMode::WmCharFinal(hwnd)
        } else {
            KeyMode::Keys
        }
    };
    match head {
        "nostream" | "final" => wmchar_final(!only_listed || listed(list)),
        "pebrel" | "compose" => {
            // 组合态那条路走的是 Pebrel 的控制面(发现文件只有一份),所以「不写名单」
            // = 只对 Pebrel 自己的程序名生效,而不是「对所有窗口生效」。
            // 名单外仍旧走键盘:静默换成别的程序的控制面是最难查的错误。
            const PEBREL_EXES: [&str; 2] = ["pebrel.exe", "nebula.exe"];
            let want = if only_listed { listed(list) } else { PEBREL_EXES.contains(&exe.as_str()) };
            if want {
                KeyMode::Pebrel
            } else {
                KeyMode::Keys
            }
        }
        "off" | "keys" | "input" => {
            // `off:foo.exe`:名单里的程序永远走键盘;名单外的按 auto,免得把兜底关掉
            if only_listed && !listed(list) {
                wmchar(thread_has_ime && classic_control)
            } else {
                KeyMode::Keys
            }
        }
        "on" | "wmchar" => wmchar(!only_listed || listed(list)),
        _ => {
            // auto(以及任何写错的值):经典文本控件 + 输入法才换;带名单时只对名单生效
            let allow = !only_listed || listed(list);
            wmchar(allow && thread_has_ime && classic_control)
        }
    }
}

#[cfg(windows)]
mod target {
    use super::KeyMode;

    #[repr(C)]
    struct GuiThreadInfo {
        cb_size: u32,
        flags: u32,
        hwnd_active: isize,
        hwnd_focus: isize,
        hwnd_capture: isize,
        hwnd_menu_owner: isize,
        hwnd_move_size: isize,
        hwnd_caret: isize,
        rc_caret: [i32; 4],
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> isize;
        fn GetGUIThreadInfo(id_thread: u32, pgui: *mut GuiThreadInfo) -> i32;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
        fn GetClassNameW(hwnd: isize, buf: *mut u16, max: i32) -> i32;
    }

    #[link(name = "imm32")]
    extern "system" {
        fn ImmIsIME(hkl: isize) -> i32;
    }

    /// 「经典文本控件」= Edit / RichEdit 这一族。
    ///
    /// 判据是类名里带 `Edit`(大小写不敏感):这一类窗口把自己收到的按键交给输入法
    /// 处理(IMM32 那条链),注入的字母才会被吃掉;而 WM_CHAR 正好绕开它。
    fn is_classic_text_control(hwnd: isize) -> bool {
        if hwnd == 0 {
            return false;
        }
        let mut buf = [0u16; 128];
        let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        if n <= 0 {
            return false;
        }
        let name = String::from_utf16_lossy(&buf[..n as usize]).to_ascii_lowercase();
        name.contains("edit")
    }

    /// 目标窗口所在线程挂的是不是输入法(IME)。
    fn thread_uses_ime(hwnd: isize) -> bool {
        if hwnd == 0 {
            return false;
        }
        let tid = unsafe { GetWindowThreadProcessId(hwnd, std::ptr::null_mut()) };
        if tid == 0 {
            return false;
        }
        let hkl = unsafe { GetKeyboardLayout(tid as u32) };
        hkl != 0 && unsafe { ImmIsIME(hkl) } != 0
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetKeyboardLayout(id_thread: u32) -> isize;
    }

    /// 目标窗口所属进程的可执行文件名(小写,含 `.exe`);拿不到就空串。
    fn exe_name(hwnd: isize) -> String {
        #[link(name = "kernel32")]
        extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
            fn QueryFullProcessImageNameW(h: isize, flags: u32, buf: *mut u16, size: *mut u32) -> i32;
            fn CloseHandle(h: isize) -> i32;
        }
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        if hwnd == 0 {
            return String::new();
        }
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid == 0 {
            return String::new();
        }
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h == 0 {
            return String::new();
        }
        let mut buf = [0u16; 512];
        let mut size = buf.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size) };
        unsafe { CloseHandle(h) };
        if ok == 0 {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..size as usize]);
        path.rsplit(['\\', '/'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    }

    /// 前台窗口所属进程 pid(拿不到就 0)。
    pub fn focused_pid() -> u32 {
        let hwnd = focused_hwnd();
        if hwnd == 0 {
            return 0;
        }
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        pid
    }

    /// 焦点控件(拿不到就退回前台窗口)。
    pub fn focused_hwnd() -> isize {
        let mut info: GuiThreadInfo = unsafe { std::mem::zeroed() };
        info.cb_size = std::mem::size_of::<GuiThreadInfo>() as u32;
        if unsafe { GetGUIThreadInfo(0, &mut info) } != 0 && info.hwnd_focus != 0 {
            return info.hwnd_focus;
        }
        if info.hwnd_active != 0 {
            return info.hwnd_active;
        }
        unsafe { GetForegroundWindow() }
    }

    pub fn key_mode(policy: &str) -> KeyMode {
        let hwnd = focused_hwnd();
        super::decide_key_mode(
            policy,
            hwnd,
            thread_uses_ime(hwnd),
            is_classic_text_control(hwnd),
            &exe_name(hwnd),
        )
    }

    /// 只给日志用:一眼看出为什么走了 WM_CHAR。
    pub fn describe(hwnd: isize) -> String {
        if hwnd == 0 {
            return "无焦点窗口".into();
        }
        let mut buf = [0u16; 128];
        let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        let cls = if n > 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { "?".into() };
        format!("{cls} 上有输入法,注入的字母会被它吃掉")
    }
}

#[cfg(not(windows))]
mod target {
    use super::KeyMode;
    pub fn focused_hwnd() -> isize {
        0
    }

    pub fn focused_pid() -> u32 {
        0
    }
    pub fn key_mode(_policy: &str) -> KeyMode {
        KeyMode::Keys
    }
    pub fn describe(_hwnd: isize) -> String {
        String::new()
    }
}

pub use target::key_mode as target_key_mode;

/// 前台窗口所属进程 pid(拿不到就 0)。
pub fn focused_pid() -> u32 {
    target::focused_pid()
}

/// 按当前方式打一段文本。
///
/// WM_CHAR 模式下**整段都走消息**:不能一半走消息、一半走键盘。
/// 这两条通道在系统里的处理时机不一样(SendInput 要过输入子系统,PostMessage 直接进
/// 消息队列),混着发**先后顺序不保证** —— 真机上表现为字母插到汉字中间
/// (`说到TOKEN` 变成 `说TO到 说TOKE到N`)。所以选定一条路就一条路走到底。
fn type_text_with(mode: KeyMode, s: &str) -> Result<()> {
    match mode {
        KeyMode::Keys => type_text(s),
        // 消息通道:整段都走消息(两条通道抢顺序会把字母插到汉字中间,所以不混)
        KeyMode::WmChar(h) | KeyMode::WmCharFinal(h) => post_chars(h, s),
        // 组合态不发按键:文本走 `pane.compose`。走到这里说明模式/传输层接线错了,
        // 宁可明确报错,也不要静默退化成键盘注入(那正是这条模式要避开的失败)。
        KeyMode::Pebrel => anyhow::bail!("Pebrel 组合态不走按键通道"),
    }
}

/// 按当前方式退格 n 次。
///
/// 消息通道下退格用 **PostMessage(WM_KEYDOWN/UP, VK_BACK)**,不是 WM_CHAR(0x08):
/// 退格在 GUI 程序里走的是按键那条路(终端收到按键才给 pty 发 DEL),
/// `WM_CHAR(0x08)` 只有经典控件会当删除用。两种消息进的是同一个队列,
/// 所以「退格 + 补打」的先后顺序仍然有保证。
fn backspace_with(mode: KeyMode, n: usize) -> Result<()> {
    match mode {
        KeyMode::Keys => backspace(n),
        KeyMode::WmChar(h) | KeyMode::WmCharFinal(h) => post_backspace(h, n),
        KeyMode::Pebrel => anyhow::bail!("Pebrel 组合态不做退格:整句是整体替换的"),
    }
}

#[cfg(windows)]
fn post_backspace(hwnd: isize, n: usize) -> Result<()> {
    const WM_KEYDOWN: u32 = 0x0100;
    const WM_KEYUP: u32 = 0x0101;
    const VK_BACK: usize = 0x08;
    if hwnd == 0 {
        return Ok(());
    }
    // lParam:重复次数 | 扫描码(0x0E=Backspace)<<16 | 抬起标志
    let scan: isize = 0x0E;
    let down = 1 | (scan << 16);
    let up = 1 | (scan << 16) | (1 << 30) | (1 << 31);
    for _ in 0..n {
        if unsafe { PostMessageW(hwnd, WM_KEYDOWN, VK_BACK, down) } == 0 {
            anyhow::bail!("PostMessage(WM_KEYDOWN VK_BACK) 失败:窗口句柄 {hwnd:#x}");
        }
        if unsafe { PostMessageW(hwnd, WM_KEYUP, VK_BACK, up) } == 0 {
            anyhow::bail!("PostMessage(WM_KEYUP VK_BACK) 失败:窗口句柄 {hwnd:#x}");
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn post_backspace(_hwnd: isize, _n: usize) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn post_chars(hwnd: isize, s: &str) -> Result<()> {
    const WM_CHAR: u32 = 0x0102;
    if hwnd == 0 {
        return Ok(());
    }
    for unit in s.encode_utf16() {
        let ok = unsafe { PostMessageW(hwnd, WM_CHAR, unit as usize, 0) };
        if ok == 0 {
            anyhow::bail!("PostMessage(WM_CHAR) 失败:窗口句柄 {hwnd:#x}");
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn post_chars(_hwnd: isize, _s: &str) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
}

fn common_prefix_chars(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}


#[cfg(test)]
mod tests {
    use super::*;

    /// 前缀（前置空格）必须进入和正文同一条算术。
    /// 这里不真的敲键盘（type_text 会 SendInput），只检查「该显示什么」的推导。
    #[test]
    fn want_includes_prefix() {
        let mut w = StreamWriter::new(8, true);
        assert_eq!(w.want("你好"), "你好");
        w.set_prefix(" ");
        assert_eq!(w.want("你好"), " 你好");
        w.set_prefix("");
        assert_eq!(w.want("你好"), "你好");
    }

    /// reset 要同时清掉前缀 —— 前缀是"这一句"的决策，换句要重新判断。
    #[test]
    fn reset_clears_prefix() {
        let mut w = StreamWriter::new(8, true);
        w.set_prefix(" ");
        w.reset().unwrap();
        assert_eq!(w.want("下一句"), "下一句");
    }

    /// 前缀变化不能影响"公共前缀"的正确性:
    /// 从" 你好"到" 你好啊" → 公共前缀 3 个字(空格+你好),只需补"啊"。
    #[test]
    fn prefix_participates_in_common_prefix() {
        let mut w = StreamWriter::new(8, true);
        w.set_prefix(" ");
        let a = w.want("你好");
        let b = w.want("你好啊");
        assert_eq!(common_prefix_chars(&a, &b), 3);
        assert_eq!(a.chars().count() - common_prefix_chars(&a, &b), 0); // 不用退格
    }

    /// 上屏方式的选择逻辑(纯函数,不需要真窗口)。
    ///
    /// 这里钉的是**只在该换的时候换**:`auto` 只在「目标线程挂着输入法 + 焦点是
    /// 经典文本控件」时用 WM_CHAR —— 别的窗口照着老路走,不拿没验证过的路去换掉
    /// 本来就没问题的注入方式。
    #[test]
    fn key_mode_policy() {
        const E: &str = "pebrel.exe";
        const OTHER: &str = "notepad.exe";
        // off:永远是键盘注入
        assert_eq!(decide_key_mode("off", 0x1234, true, true, E), KeyMode::Keys);
        // on:强制 WM_CHAR
        assert_eq!(decide_key_mode("on", 0x1234, false, false, E), KeyMode::WmChar(0x1234));
        // on 但拿不到窗口句柄 → 退回键盘(有句柄才谈得上投递消息)
        assert_eq!(decide_key_mode("on", 0, true, true, E), KeyMode::Keys);
        // auto:输入法 + 经典文本控件才换
        assert_eq!(decide_key_mode("auto", 0x1234, true, true, E), KeyMode::WmChar(0x1234));
        assert_eq!(decide_key_mode("auto", 0x1234, true, false, E), KeyMode::Keys);
        assert_eq!(decide_key_mode("auto", 0x1234, false, true, E), KeyMode::Keys);
        assert_eq!(decide_key_mode("auto", 0, true, true, E), KeyMode::Keys);
        // 没写这个键(或写错)按 auto 走
        assert_eq!(decide_key_mode("", 0x1234, true, true, E), KeyMode::WmChar(0x1234));
        assert_eq!(decide_key_mode("whatever", 0x1234, true, true, E), KeyMode::WmChar(0x1234));
        // 名单形式:只对该程序生效(自绘终端就靠它试,不牵动别的窗口)
        assert_eq!(decide_key_mode("on:pebrel.exe", 0x1234, false, false, E), KeyMode::WmChar(0x1234));
        assert_eq!(decide_key_mode("on:pebrel.exe", 0x1234, false, false, OTHER), KeyMode::Keys);
        assert_eq!(decide_key_mode("on: Pebrel.EXE , zed.exe", 0x1234, false, false, E), KeyMode::WmChar(0x1234));
        assert_eq!(decide_key_mode("on:pebrel.exe", 0x1234, true, true, OTHER), KeyMode::Keys);
        // off 带名单:名单里的永远键盘,名单外仍享受 auto 兜底
        assert_eq!(decide_key_mode("off:pebrel.exe", 0x1234, true, true, E), KeyMode::Keys);
        assert_eq!(decide_key_mode("off:pebrel.exe", 0x1234, true, true, OTHER), KeyMode::WmChar(0x1234));
        // auto 带名单:只有名单里的程序吃 auto 规则
        assert_eq!(decide_key_mode("auto:pebrel.exe", 0x1234, true, true, OTHER), KeyMode::Keys);
        assert_eq!(decide_key_mode("auto:pebrel.exe", 0x1234, true, true, E), KeyMode::WmChar(0x1234));
        // nostream:只对名单生效,走「整句一次投递」
        assert_eq!(decide_key_mode("nostream:pebrel.exe", 0x1234, false, false, E), KeyMode::WmCharFinal(0x1234));
        assert_eq!(decide_key_mode("nostream:pebrel.exe", 0x1234, false, false, OTHER), KeyMode::Keys);
        assert_eq!(decide_key_mode("nostream", 0x1234, false, false, E), KeyMode::WmCharFinal(0x1234));
        // pebrel:组合态缓冲层。**不写名单 = 只对 Pebrel 自己的程序名生效** ——
        // 控制面在这台机上只有一份,把别的程序的文本塞进它的缓冲层是最难查的错误。
        assert_eq!(decide_key_mode("pebrel", 0x1234, false, false, E), KeyMode::Pebrel);
        assert_eq!(
            decide_key_mode("pebrel", 0x1234, false, false, "nebula.exe"),
            KeyMode::Pebrel,
            "旧程序名 nebula.exe 也认"
        );
        assert_eq!(decide_key_mode("pebrel", 0x1234, false, false, OTHER), KeyMode::Keys);
        assert_eq!(
            decide_key_mode("pebrel:psql.exe", 0x1234, false, false, "psql.exe"),
            KeyMode::Pebrel
        );
        assert_eq!(
            decide_key_mode("pebrel:psql.exe", 0x1234, false, false, OTHER),
            KeyMode::Keys,
            "名单外不许静默换通道"
        );
    }

    /// 消息通道默认**照常流式**(退格走 PostMessage(WM_KEYDOWN/UP, VK_BACK))。
    /// 用 `WmChar(0)` 做这个测试:句柄 0 时消息函数直接返回,不会真的发消息。
    #[test]
    fn wmchar_mode_streams_on_the_message_channel() {
        let mut w = StreamWriter::new(8, true);
        w.set_mode(KeyMode::WmChar(0));
        w.update("我说到 TOKE").unwrap();
        assert_eq!(w.injected, "我说到 TOKE", "消息通道要照常逐帧上屏");
        let typed = w.typed_chars;
        w.update("我说到 TOKEN 的时候").unwrap();
        assert_eq!(w.injected, "我说到 TOKEN 的时候");
        assert!(w.typed_chars > typed, "新增部分要接着打出去");
        assert_eq!(w.backspaces, 0, "只往后加,不用退格");
        // 回退(ASR 改口)时才退格:这里退 1 个字再补一个字
        w.update("我说到 TOKEN 的时").unwrap();
        assert_eq!(w.backspaces, 1, "改口要退格一次");
        assert_eq!(w.injected, "我说到 TOKEN 的时");
    }

    /// `nostream:` 策略:一个字都不流式,定稿时整句一次投递(连消息退格都不认的程序用)。
    #[test]
    fn nostream_mode_types_only_the_whole_final() {
        let mut w = StreamWriter::new(8, true);
        w.set_mode(KeyMode::WmCharFinal(0));
        w.update("我").unwrap();
        w.update("我说到 TOKEN 的时候").unwrap();
        assert_eq!(w.injected, "", "整句一次模式不许流式上屏");
        w.finalize("我说到 TOKEN 的时候", 32).unwrap();
        assert_eq!(w.injected, "我说到 TOKEN 的时候", "定稿时整句一次投递");
        assert_eq!(w.typed_chars, "我说到 TOKEN 的时候".chars().count());
        assert_eq!(w.backspaces, 0, "整句投递不需要退格");
        let typed = w.typed_chars;
        w.finalize("我说到 TOKEN 的时候", 32).unwrap();
        assert_eq!(w.typed_chars, typed, "重复 final 不该重复打一遍");
    }

    /// 安全阀放弃修正时,**记账必须留在屏幕上那一版**。
    ///
    /// 两条路径都不许把 `injected` 推到屏幕前面 —— 一旦对不上,后面每次退格都是
    /// 照「屏幕上有这些字」算的,算出来的退格数会删掉用户真正的文字。
    ///
    /// 这两次调用的差值都**故意超过**安全阀(final 32 / 流式 `max_backspace` 8),
    /// 所以都走「放弃」分支 —— 不会真的 SendInput,测试里可以放心跑。
    #[test]
    fn giveup_keeps_bookkeeping_on_screen() {
        let mut w = StreamWriter::new(8, true);
        // 屏幕上已经有一句(测试里直接摆好状态,不真的敲键盘)。
        // 必须比 final 的 32 字上限长,否则就会走进「真的退格重打」那条路。
        w.injected =
            "已经上屏的这一句 hello world 后面还有一大截没说完还在继续说下去".to_string();
        assert!(w.injected.chars().count() > 32);
        let screen = w.injected.clone();

        // final 超限(> 32):整笔放弃,记账不动
        w.finalize("收口", 32).unwrap();
        assert_eq!(w.injected, screen, "final 放弃后 injected 必须还是屏幕上那一版");

        // 流式超限(> max_backspace 8):同样是放弃且记账不动
        w.update("收口").unwrap();
        assert_eq!(w.injected, screen, "流式放弃后 injected 必须还是屏幕上那一版");
    }

    /// 组合态模式的替身:只记录被要求设置/提交了什么,不碰任何窗口。
    #[derive(Clone, Default)]
    struct Recorder(std::sync::Arc<std::sync::Mutex<Vec<(String, bool)>>>);

    impl Recorder {
        fn calls(&self) -> Vec<String> {
            self.0.lock().unwrap().iter().filter(|(_, commit)| !commit).map(|(t, _)| t.clone()).collect()
        }
        fn commits(&self) -> Vec<String> {
            self.0.lock().unwrap().iter().filter(|(_, commit)| *commit).map(|(t, _)| t.clone()).collect()
        }
    }

    impl crate::pebrel::Composer for Recorder {
        fn set(&mut self, text: &str) -> Result<()> {
            self.0.lock().unwrap().push((text.to_string(), false));
            Ok(())
        }
        fn commit(&mut self, text: &str) -> Result<()> {
            self.0.lock().unwrap().push((text.to_string(), true));
            Ok(())
        }
    }

    /// 组合态:**整体替换**缓冲层,不产生一个按键、不产生一次退格。
    /// (真机语料里约 5% 的帧会回头改前面的词;那些帧在按键模式下就是退格补打。)
    #[test]
    fn compose_mode_replaces_the_whole_buffer_and_never_backspaces() {
        let mut w = StreamWriter::new(8, true);
        let rec = Recorder::default();
        w.set_mode(KeyMode::Pebrel);
        w.set_composer(Some(Box::new(rec.clone())));
        w.update("我说到 TOKE").unwrap();
        w.update("我说到 TOKEN 的时候").unwrap();
        w.update("我说到 TOKEN 的时").unwrap(); // ← 改口那一帧
        assert_eq!(w.injected, "", "没提交就等于没上屏");
        assert_eq!(w.backspaces, 0, "组合态不做退格");
        assert_eq!(w.typed_chars, 0, "组合态不按键,所以不打字");
        assert_eq!(
            rec.calls(),
            vec!["我说到 TOKE", "我说到 TOKEN 的时候", "我说到 TOKEN 的时"],
            "每一帧都是整体替换"
        );
        assert!(rec.commits().is_empty(), "没定稿不许提交");
        w.update("我说到 TOKEN 的时").unwrap();
        assert_eq!(rec.calls().len(), 3, "同一段文本不重复发");
    }

    /// 定稿只提交一次,提交之后记账才等于屏幕。
    #[test]
    fn compose_mode_commits_once_on_finalize() {
        let mut w = StreamWriter::new(8, true);
        let rec = Recorder::default();
        w.set_mode(KeyMode::Pebrel);
        w.set_composer(Some(Box::new(rec.clone())));
        w.set_prefix(" ");
        w.update("我说到 TOKE").unwrap();
        w.finalize("我说到 TOKEN 的时候", 32).unwrap();
        assert_eq!(rec.commits(), vec![" 我说到 TOKEN 的时候"], "提交要带上前置空格");
        assert_eq!(w.injected, " 我说到 TOKEN 的时候");
        assert_eq!(w.typed_chars, " 我说到 TOKEN 的时候".chars().count());
        w.finalize("我说到 TOKEN 的时候", 32).unwrap();
        assert_eq!(rec.commits().len(), 1, "重复 final 不许重复提交(等于说了一遍进去两遍)");
    }

    /// 换句必须把上一版组合串从应用里清掉:它挂着的时候,应用是把按键让给它的。
    #[test]
    fn compose_mode_cancels_the_buffer_on_reset() {
        let mut w = StreamWriter::new(8, true);
        let rec = Recorder::default();
        w.set_mode(KeyMode::Pebrel);
        w.set_composer(Some(Box::new(rec.clone())));
        w.update("还没说完的一句").unwrap();
        w.reset().unwrap();
        assert_eq!(rec.calls(), vec!["还没说完的一句", ""], "换句 = 清掉缓冲层");
        w.reset().unwrap();
        assert_eq!(rec.calls().len(), 2, "没有挂着的组合串时不多发一条");
    }

}

/// 流式改写器：把「当前应显示的文本」增量地落到目标窗口。
///
/// 策略：算公共前缀 → 退格掉差异尾巴 → 补打新增部分。
/// `max_backspace` 是安全阀：改动超过这个字数就放弃本次修正，
/// 宁可等下一帧，也不冒删掉用户自己文字的风险。
pub struct StreamWriter {
    injected: String,
    max_backspace: usize,
    enabled: bool,
    /// 这一句要不要在前面补一个空格（用户要的"光标前有字就插空格"）。
    ///
    /// 为什么放在 writer 里而不是在 typing 前拼一下：
    /// 流式改写靠「公共前缀 → 退格差异 → 补打新增」这套算术，**屏幕上的内容必须和
    /// `injected` 完全一致**。如果只是打的时候多敲一个空格、而 `injected` 里没有，
    /// 下一帧算前缀就会错位，退格数算错 → 删到用户自己的字。
    /// 所以空格作为 `prefix` 进入同一条算术：`injected` 始终等于 prefix + 逻辑文本。
    prefix: String,
    /// 这一句用哪种方式上屏(每句开始时按目标窗口决定,见 `decide_key_mode`)。
    /// 默认是键盘注入;只有「输入法会吃字母」的窗口才切成 WM_CHAR。
    mode: KeyMode,
    /// 组合态模式(`KeyMode::Pebrel`)的传输层:设置/提交应用自己的缓冲层。
    /// 只在这个模式下才需要,别的模式是 `None`。
    composer: Option<Box<dyn crate::pebrel::Composer>>,
    /// 当前**已经放进组合串**的文本 —— 屏幕上还没提交的那一版。
    /// 组合态下 `injected` 始终是空的:没提交就是没上屏。
    composed: String,
    pub typed_chars: usize,
    pub backspaces: usize,
}

impl StreamWriter {
    pub fn new(max_backspace: usize, enabled: bool) -> Self {
        Self {
            injected: String::new(),
            max_backspace,
            enabled,
            prefix: String::new(),
            mode: KeyMode::Keys,
            composer: None,
            composed: String::new(),
            typed_chars: 0,
            backspaces: 0,
        }
    }

    /// 设定这一句的前缀(`" "` 或 `""`)。**每句开始时**由调用方按光标上下文决定一次。
    pub fn set_prefix(&mut self, p: &str) {
        self.prefix = p.to_string();
    }

    /// 设定这一句的上屏方式。**每句开始时**决定一次:目标窗口可能已经换了。
    pub fn set_mode(&mut self, m: KeyMode) {
        self.mode = m;
    }

    pub fn mode(&self) -> KeyMode {
        self.mode
    }

    /// 装上/卸下组合态传输层。**每句开始时**由调用方按模式决定:
    /// 组合态需要一个真客户端(每句重解析目标窗口与活动 pane)。
    pub fn set_composer(&mut self, composer: Option<Box<dyn crate::pebrel::Composer>>) {
        self.composer = composer;
    }


    pub fn reset(&mut self) -> Result<()> {
        self.injected.clear();
        // 前缀属于"这一句"的决策，换句要重新决定（光标位置可能已经变了）
        self.prefix.clear();
        if self.composed.is_empty() {
            return Ok(());
        }
        // 组合串挂在应用自己的输入缓冲里,而这期间应用的按键处理是让位给它的;
        // 留着等于替用户按住了一个看不见的输入框。
        self.composed.clear();
        if let Some(composer) = self.composer.as_mut() {
            composer.set("")?;
        }
        Ok(())
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn injected(&self) -> &str {
        &self.injected
    }

    /// 这一句最终应该显示成什么（含前缀）。
    fn want(&self, target: &str) -> String {
        if self.prefix.is_empty() {
            target.to_string()
        } else {
            format!("{}{}", self.prefix, target)
        }
    }

    /// 用 partial 结果更新屏幕。失败不致命,交给下一帧。
    ///
    /// ⚠️ WM_CHAR 模式**不流式**:一个字都不打,等定稿(`finalize`)整句一次投递。
    /// 为什么:这条路靠 PostMessage,而「消息里的退格」(WM_CHAR(0x08))各家程序语义
    /// 不一样 —— 真机上退格被忽略、插入照常,屏幕就成了同一句反复叠加
    /// (`说到TOKEN` 变成 `说TO到 说TOKE到N`)。整句一次投递既不需要退格,也不存在
    /// 「消息通道 vs 键盘通道抢先后」的问题。代价:这类窗口里没有逐字上屏。
    pub fn update(&mut self, target: &str) -> Result<()> {
        if self.mode.is_compose() {
            // 组合态:**整体替换**缓冲层,没有退格算术 —— 因为一个字节都还没上屏。
            // 这正是「边说边改」在终端里能做到的形态:应用自己那层 preedit 可以随便
            // 改写,屏幕(和 shell 历史)不会经过任何中间态。
            return self.compose_set(target);
        }
        if self.mode.is_final_only() {
            return Ok(());
        }
        if !self.enabled || target.is_empty() {
            // 空结果一律当成「这次没听清」,绝不把已经上屏的字擦掉。
            // final 走 finalize(),那条路径才允许改写/清空。
            return Ok(());
        }
        let want = self.want(target);
        if want == self.injected {
            return Ok(());
        }
        let common = common_prefix_chars(&self.injected, &want);
        let bs = self.injected.chars().count().saturating_sub(common);
        if bs > self.max_backspace {
            return Ok(()); // 改动太大，等 final
        }
        if bs > 0 {
            backspace_with(self.mode, bs)?;
            self.backspaces += bs;
        }
        let tail: String = want.chars().skip(common).collect();
        if !tail.is_empty() {
            type_text_with(self.mode, &tail)?;
            self.typed_chars += tail.chars().count();
        }
        self.injected = want;
        Ok(())
    }

    /// 组合态:把这一句(含前缀)放进应用的缓冲层。同样的文本不重复发。
    fn compose_set(&mut self, target: &str) -> Result<()> {
        if !self.enabled || target.is_empty() {
            // 空结果 = 这次没听清:不要把已经放进缓冲层的那一版擦掉,
            // 定稿或换句时才收尾。
            return Ok(());
        }
        let want = self.want(target);
        if want == self.composed {
            return Ok(());
        }
        let Some(composer) = self.composer.as_mut() else {
            anyhow::bail!("组合态模式没有装传输层(内部接线错误)");
        };
        composer.set(&want)?;
        self.composed = want;
        Ok(())
    }


    /// final 结果：允许更大的回退（final 通常只是把尾巴改得更准）。
    ///
    /// 走和 `update` **完全相同**的前缀算术（含 `prefix`），否则屏幕与 `injected` 会错位。
    pub fn finalize(&mut self, target: &str, max_bs: usize) -> Result<()> {
        let want = self.want(target);
        if !self.enabled {
            self.injected = want;
            return Ok(());
        }
        if self.mode.is_compose() {
            // 提交:清掉缓冲层,把整段作为**一次**写入交给应用(与输入法提交同一条路)。
            // `max_bs` 在这里没有意义 —— 缓冲层是整体替换的,不存在退格。
            if want == self.injected {
                return Ok(());
            }
            let Some(composer) = self.composer.as_mut() else {
                anyhow::bail!("组合态模式没有装传输层(内部接线错误)");
            };
            composer.commit(&want)?;
            self.typed_chars += want.chars().count();
            self.injected = want;
            self.composed.clear();
            return Ok(());
        }
        if self.mode.is_final_only() {
            // 不流式:整句(含前置空格)一次投递,没有退格,也就没有「记账 vs 屏幕」的偏差。
            if want == self.injected {
                return Ok(());
            }
            type_text_with(self.mode, &want)?;
            self.typed_chars += want.chars().count();
            self.injected = want;
            return Ok(());
        }
        if want == self.injected {
            return Ok(());
        }
        let common = common_prefix_chars(&self.injected, &want);
        let bs = self.injected.chars().count().saturating_sub(common);
        if bs > max_bs {
            // 超限:整笔放弃,**屏幕和记账都不动**。
            //
            // 这里原来会 `self.injected = want` —— 于是记账跑到了屏幕前面
            // (记账说屏幕上是新文本,屏幕上其实还是旧文本)。之后每一次退格都是
            // 照着「屏幕上有这些字」算的,算出来的退格数就会删掉用户真正的文字;
            // 真机上表现为「说英文的那一句整段不见了」。
            // 宁可这一笔不生效(屏幕留着上一版假设,反正下一句会 reset),
            // 也不能让记账和屏幕对不上。
            return Ok(());
        }
        if bs > 0 {
            backspace_with(self.mode, bs)?;
            self.backspaces += bs;
        }
        let tail: String = want.chars().skip(common).collect();
        if !tail.is_empty() {
            type_text_with(self.mode, &tail)?;
            self.typed_chars += tail.chars().count();
        }
        self.injected = want;
        Ok(())
    }
}