//! 读「焦点输入框里光标左边是什么」—— 只为判断**要不要在前面补一个空格**。
//!
//! 为什么不用 UI Automation：本机 UIA 不可用 ——
//! `comtypes` 的预生成模块与 typelib 版本不符，而它的生成目录 `site-packages\comtypes\gen\`
//! 连当前用户都写不进去（实测 Win32 拒绝访问），所以任何基于 comtypes 的 UIA 都会在
//! `Can not load UIAutomationCore.dll` 上失败。Python 侧的 `uiautomation`、Rust 侧的
//! `CoCreateInstance(CUIAutomation)` 都以 `CLSID 未注册` 告终。
//!
//! 于是改走**标准窗口消息**这条路（旧 ainput 项目也是这么兜底的）：
//!   · 拿到焦点窗口 → 找它下面的文本控件（RichEdit / Edit 类）
//!   · `EM_GETSEL` 问光标位置，`WM_GETTEXT` 读全文，取光标前一个字
//! 实测在 Win11 记事本（文本控件是 `RichEditD2DPT`）上工作正常：
//! 塞入「你好世界ABCDEF」、光标放第 6 位 → 读出前一个字是 `B`。
//!
//! **失败就当"不知道"，并且按"有字"处理**（补空格）—— 宁可在行首多加一个空格，
//! 也好过该分隔时没分隔（旧项目 `TargetLeftContext::Unavailable => true` 同此取舍）。

/// 光标左边是什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftContext {
    /// 明确没有字（行首，或左边只有空白）→ 不补空格
    AtStart,
    /// 明确有字 → 补空格
    HasText,
    /// 读不出来（不是标准控件 / 消息失败）→ 按"有字"处理，补空格
    Unknown,
}

impl LeftContext {
    /// 该不该在前面补空格。
    ///
    /// `AtStart` 不补；其余都补。这是**保守**方向：少补一个空格在视觉上无伤，
    /// 而该分隔时没分隔会把两句话粘在一起（"…看完了接下来"）。
    pub fn wants_leading_space(self) -> bool {
        !matches!(self, LeftContext::AtStart)
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    use super::LeftContext;

    const WM_GETTEXT: u32 = 0x000D;
    const WM_GETTEXTLENGTH: u32 = 0x000E;
    const EM_GETSEL: u32 = 0x00B0;

    /// 只读这么多个字符 —— 光标前一个字就够了，别把整篇文档拖进来。
    const MAX_READ_CHARS: usize = 4096;

    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> isize;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
        fn GetGUIThreadInfo(tid: u32, info: *mut GuiThreadInfo) -> i32;
        fn GetClassNameW(hwnd: isize, buf: *mut u16, max: i32) -> i32;
        fn SendMessageW(hwnd: isize, msg: u32, wp: usize, lp: isize) -> isize;
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct GuiThreadInfo {
        cb_size: u32,
        flags: u32,
        hwnd_active: isize,
        hwnd_focus: isize,
        hwnd_capture: isize,
        hwnd_menu_owner: isize,
        hwnd_move_size: isize,
        hwnd_caret: isize,
        rc_caret: Rect,
    }

    fn class_of(hwnd: isize) -> String {
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), 256) };
        if n <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..n as usize])
    }

    /// 这些类的控件支持 `EM_GETSEL` + `WM_GETTEXT`。
    /// `RichEditD2DPT` 是 Win11 记事本用的（实测有效）；`RichEdit20W` 是老版记事本；
    /// `Edit` 是经典单行/多行编辑框。
    fn is_readable_text_control(cls: &str) -> bool {
        matches!(cls, "Edit" | "RichEdit" | "RichEdit20A" | "RichEdit20W" | "RichEdit50W" | "RichEditD2DPT")
            || cls.starts_with("RichEdit")
    }

    /// 遍历子窗口找一个可读的文本控件（只挖两层，够用且不至于慢）。
    fn find_text_control(root: isize) -> Option<isize> {
        extern "system" {
            fn EnumChildWindows(hwnd: isize, cb: EnumProc, lp: isize) -> i32;
        }
        type EnumProc = unsafe extern "system" fn(isize, isize) -> i32;

        // 用静态槽收集：EnumChildWindows 的回调不能捕获环境。
        static mut FOUND: isize = 0;
        unsafe extern "system" fn cb(hwnd: isize, _lp: isize) -> i32 {
            let c = class_of(hwnd);
            if is_readable_text_control(&c) {
                unsafe { FOUND = hwnd };
                return 0; // 找到就停
            }
            1
        }
        unsafe {
            FOUND = 0;
            EnumChildWindows(root, cb, 0);
            if FOUND != 0 {
                return Some(FOUND);
            }
        }
        None
    }

    /// 问控件的「光标位置」和「光标前一个字」。
    fn read_left_char(hwnd: isize) -> Option<LeftContext> {
        // EM_GETSEL 返回值：低 16 位是选区起点（= 光标位置）。
        // ⚠️ 注意：超过 65535 字符的长文档这里会被截断 —— 但我们只需要判断
        // "左边有没有字"，而且真到那种长度，返回值也不会正好是 0。
        let sel = unsafe { SendMessageW(hwnd, EM_GETSEL, 0, 0) } as u32;
        let caret = (sel & 0xFFFF) as usize;
        if caret == 0 {
            return Some(LeftContext::AtStart);
        }

        let len = unsafe { SendMessageW(hwnd, WM_GETTEXTLENGTH, 0, 0) } as usize;
        if len == 0 {
            // 有光标位但读不到长度：说不清，保守补空格
            return Some(LeftContext::Unknown);
        }
        let want = len.min(MAX_READ_CHARS);
        let mut buf = vec![0u16; want + 2];
        let got = unsafe {
            SendMessageW(
                hwnd,
                WM_GETTEXT,
                buf.len(),
                buf.as_mut_ptr() as *mut c_void as isize,
            )
        } as usize;
        if got == 0 {
            return Some(LeftContext::Unknown);
        }
        let text = String::from_utf16_lossy(&buf[..got.min(want)]);

        // 取光标前一个字（按字符而不是字节，中文才对得上）
        let chars: Vec<char> = text.chars().collect();
        let idx = caret.min(chars.len());
        if idx == 0 {
            return Some(LeftContext::AtStart);
        }
        let prev = chars[idx - 1];
        if prev.is_whitespace() {
            Some(LeftContext::AtStart) // 左边已经是空白了，不用补
        } else {
            Some(LeftContext::HasText)
        }
    }

    /// 读当前焦点处光标左边是什么。
    pub fn left_context() -> LeftContext {
        unsafe {
            let fg = GetForegroundWindow();
            if fg == 0 {
                return LeftContext::Unknown;
            }
            let tid = GetWindowThreadProcessId(fg, std::ptr::null_mut());
            if tid == 0 {
                return LeftContext::Unknown;
            }
            let mut gi = GuiThreadInfo {
                cb_size: std::mem::size_of::<GuiThreadInfo>() as u32,
                ..Default::default()
            };
            if GetGUIThreadInfo(tid, &mut gi) == 0 {
                return LeftContext::Unknown;
            }
            // 焦点控件（没拿到就用前台窗口本身）
            let focus = if gi.hwnd_focus != 0 { gi.hwnd_focus } else { fg };

            // 先看焦点控件自己是不是可读文本控件
            if is_readable_text_control(&class_of(focus)) {
                if let Some(c) = read_left_char(focus) {
                    return c;
                }
            }
            // 不然往它下面找一层（很多应用是 容器 → 文本控件 两层结构）
            if let Some(child) = find_text_control(focus) {
                if let Some(c) = read_left_char(child) {
                    return c;
                }
            }
            LeftContext::Unknown
        }
    }
}

#[cfg(windows)]
pub fn left_context() -> LeftContext {
    win::left_context()
}

#[cfg(not(windows))]
pub fn left_context() -> LeftContext {
    LeftContext::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_start_means_no_space() {
        assert!(!LeftContext::AtStart.wants_leading_space());
    }

    #[test]
    fn has_text_and_unknown_both_want_space() {
        // 保守方向：读不出来也要补，宁多勿缺
        assert!(LeftContext::HasText.wants_leading_space());
        assert!(LeftContext::Unknown.wants_leading_space());
    }
}
