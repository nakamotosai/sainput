//! 光标旁的悬浮状态条 —— Direct2D 版（v2，六号方案）。
//!
//! ## 为什么重写
//!
//! 旧版走 GDI 的 Window DC。实测（`examples/gdiprobe.rs`）那条路上
//! `FillRect` / `RoundRect` / `Rectangle` / `TextOutW` **全都不落笔**，
//! 只有 `ExtTextOutW` 能画 —— 所以只能拿 `█` 字符铺底 + 1px 直角边框，
//! 做不出圆角、真阴影、半透明背景。用户评价「太丑」。
//!
//! Direct2D 走**独立的渲染目标**（DCRenderTarget / HWND RenderTarget），
//! 不受那条路径的限制：圆角、渐变、真阴影、抗锯齿文字全都能画。
//!
//! ## 设计规格（参考本地语音输入法的常见样式，配色自定）
//!
//! 常见悬浮条几何与质感参数实测：
//!   · 圆角 = 高度/2（胶囊），或小圆角矩形
//!   · 阴影 dy=2 / blur=3 / 黑 20%
//!   · 描边 0.5–1px，白 8%
//!   · 字体 Microsoft YaHei UI 13px，文字 85% 白
//!
//! 我们沿用几何与质感参数，配色用「暖墨」（暖棕黑 + 琥珀），见下方常量。
//!
//! ## 关键约束（少任何一条都会出事故）
//!   WS_EX_NOACTIVATE   不抢焦点。抢了焦点 => 注入打到别处、用户输入被打断。
//!   WS_EX_TOOLWINDOW   不进 alt-tab、不进任务栏。
//!   WS_EX_TOPMOST      永远浮在最上层。
//!   WS_EX_LAYERED      半透明（这里用 per-pixel alpha 走 UpdateLayeredWindow）
//!   WM_MOUSEACTIVATE   返回 MA_NOACTIVATE，用户点它也抢不走焦点。
//!
//! ## 线程模型
//! 独立线程持窗 + 40ms 定时器轮询共享状态（沿用旧版，已验证可靠）。

use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const WS_POPUP: u32 = 0x8000_0000;
const WS_EX_TOPMOST: u32 = 0x0000_0008;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_LAYERED: u32 = 0x0008_0000;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;

const SW_HIDE: i32 = 0;
const SW_SHOWNOACTIVATE: i32 = 4;
const HWND_TOPMOST: isize = -1;
const SWP_NOACTIVATE: u32 = 0x0010;

const WM_PAINT: u32 = 0x000F;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_TIMER: u32 = 0x0113;
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_MOUSEACTIVATE: u32 = 0x0021;
const MA_NOACTIVATE: isize = 3;

const OVERLAY_CLASS: &str = "SainputOverlayWnd";

/// 当前屏幕的 DPI 缩放系数（1.0 = 96 DPI / 100%）。
///
/// 为什么必须要有它：进程是 **per-monitor DPI aware v2** 的，
/// `GetSystemMetrics` 返回**物理像素**；而用户说的"太小"是**视觉大小**。
/// 本机实测屏幕 3320x3846 物理像素 / 225% 缩放 = 1476x1709 逻辑像素。
///
/// 早先所有尺寸都写成裸像素（MIN_W=520 等），在 225% 缩放下
/// 实际只占 231 逻辑像素宽 —— 视觉上当然小得看不清。
/// 现在统一：**设计稿按 100% 写，运行时乘 `scale()`**。
fn scale() -> f32 {
    unsafe {
        // GetDpiForWindow 需要 hwnd；这里用屏幕 DC 的 LOGPIXELSX 更简单，
        // 而且和 GetSystemMetrics 的坐标系一致（都是物理像素）。
        let dc = GetDC(0);
        if dc == 0 {
            return 1.0;
        }
        let dpi = GetDeviceCaps(dc, 88); // LOGPIXELSX
        ReleaseDC(0, dc);
        if dpi <= 0 {
            1.0
        } else {
            dpi as f32 / 96.0
        }
    }
}

/// 设计稿像素 -> 物理像素。
fn px(v: i32) -> i32 {
    (v as f32 * scale()).round() as i32
}

/// 边缘光的动画时钟：返回 (呼吸相位, 流动相位)，都在 0..1。
///
/// ⚠️ 必须用**相对时间**，不能用 `SystemTime::now()` 的绝对毫秒。
/// 绝对时间戳约 1.7e12，f32 在那个量级的量化步长约 25 ——
/// 把 0..1 的周长参数 `t` 加上去会被**完全吞掉**，
/// 结果是颜色沿边缘不变化（踩过这个坑）。
///
/// 相对时间最多几小时（~1e7 ms），f32 精度足够。
fn edge_clock() -> (f32, f32) {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = START.get_or_init(Instant::now);
    let ms = start.elapsed().as_millis() as f64;

    let breath = ((ms % EDGE_BREATH_MS as f64) / EDGE_BREATH_MS as f64) as f32;
    let flow = ((ms % EDGE_FLOW_MS as f64) / EDGE_FLOW_MS as f64) as f32;
    (breath, flow)
}

// ---- 设计规格 ----
const PAD_X: i32 = 24;          // 左右内边距（放大后按比例加宽）
const PAD_TOP: i32 = 18;
const PAD_BOTTOM: i32 = 20;
const RADIUS: i32 = 14;         // 六号方案：小圆角矩形（不是胶囊）
const SHADOW_DY: i32 = 3;
const SHADOW_BLUR: f32 = 6.0;
const SHADOW_ALPHA: f32 = 0.28;
/// 最小宽度按屏幕宽度的比例算（见 `anchor_pos`）—— 用户要的是"屏幕中下方的一条"，
/// 固定像素在不同分辨率上会时大时小。
const MIN_W: i32 = 520;
const MAX_W: i32 = 900;
/// 距屏幕底边（任务栏上方）留多少像素。
const BOTTOM_GAP: i32 = 56;

// ---- 细条（Bar）专用：只有一行正文 ----
const BAR_PAD_X: i32 = 18;
const BAR_PAD_Y: i32 = 10;
const BAR_LINE_H: i32 = 28;
const BAR_MIN_W: i32 = 320;
const BAR_MAX_W: i32 = 860;
const BAR_MIN_H: i32 = 44;
/// 估宽用：ASCII 字符宽 / 中日韩字符宽（设计像素）。
const BAR_CHAR_ASCII: i32 = 10;
const BAR_CHAR_CJK: i32 = 18;

// ---- 悬浮球（Orb）专用 ----
/// 球的直径（设计像素）。42 在 225% 缩放下约 95 物理像素，视觉上是个小圆。
const ORB_D: i32 = 46;
/// 波形柱数量与几何。
const ORB_BARS: usize = 9;
const ORB_BAR_W: i32 = 2;
const ORB_BAR_GAP: i32 = 1;
const ORB_BAR_MIN_H: i32 = 2;
const ORB_BAR_MAX_H: i32 = 18;

// ---- 细条外观 ----
const BAR_RADIUS: i32 = 10;
const BAR_FONT: f32 = 17.0;
const BAR_INK: u32 = 0xF2_F5F0E8;

// ---- 屏幕边缘光（Edge）----
/// 光带从屏幕边缘往内延伸多少设计像素。
/// 太大就挡内容，太小看不出来 —— 110 在 225% 缩放下约 248 物理像素，够"晕开"又不挡字。
const EDGE_SPREAD: i32 = 56;
/// 最亮处的白色不透明度（0..1）。
const EDGE_PEAK_ALPHA: f32 = 0.42;
/// 呼吸周期（毫秒）。录音时亮度在这个周期里来回起伏。
const EDGE_BREATH_MS: u64 = 2200;
/// 彩色沿周长流动一圈的时间（毫秒）。8 秒是"看得出在动但不晃眼"的节奏。
const EDGE_FLOW_MS: u64 = 8000;

// 暖墨配色
//
// ⚠️ 格式是 **0xAARRGGBB**（和 `argb()` 一致）。
// 早先抄常量时漏了 alpha 通道（写成 0x00RRGGBB），结果整个 HUD 全透明、
// 屏幕上只剩一层淡淡的阴影 —— 排查了半天才发现是 alpha=0。
const BG: u32 = 0xF2_241F1A;        // 暖棕黑 rgb(36,31,26) @ 95%
const BORDER: u32 = 0x1A_FFE0C0;    // 暖白描边 rgb(255,224,192) @ 10%
const INK: u32 = 0xF0_F5F0E8;       // 奶油白 rgb(245,240,232) @ 94%
const ACCENT: u32 = 0xFF_FF9A4D;    // 琥珀 rgb(255,154,77)
const ACCENT2: u32 = 0xFF_50D0FF;   // 青 rgb(80,208,255)

// 细条要**半透明** —— 用户明确要求「做成半透明的」，而且要能透出下面的字。
// 88/255 ≈ 35% 不透明：足够压住背景保证可读，又能看见底下的内容。
const BAR_BG: u32 = 0x88_1E1A16;
const BAR_BORDER: u32 = 0x22_FFE0C0;
// 球体用更实一点的底（它小，太透就看不见了）
const ORB_BG: u32 = 0xC8_1E1A16;
const ORB_RING: u32 = 0x40_FF9A4D;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
#[repr(C)]
struct PaintStruct {
    hdc: isize,
    erase: i32,
    rc: Rect,
    restore: i32,
    inc_update: i32,
    reserved: [u8; 32],
}
#[repr(C)]
struct Msg {
    hwnd: isize,
    message: u32,
    w_param: usize,
    l_param: isize,
    time: u32,
    pt_x: i32,
    pt_y: i32,
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
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Point {
    x: i32,
    y: i32,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Size {
    cx: i32,
    cy: i32,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct BlendFunction {
    blend_op: u8,
    blend_flags: u8,
    source_constant_alpha: u8,
    alpha_format: u8,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
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

#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(hdc: isize) -> isize;
    fn CreateDIBSection(
        hdc: isize,
        bmi: *const BitmapInfo,
        usage: u32,
        bits: *mut *mut u8,
        section: isize,
        offset: u32,
    ) -> isize;
    fn DeleteDC(hdc: isize) -> i32;
    fn SelectObject(hdc: isize, obj: isize) -> isize;
    fn DeleteObject(obj: isize) -> i32;
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
    fn IsWindowVisible(h: isize) -> i32;
    fn SetWindowPos(h: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
    fn SetTimer(h: isize, id: usize, ms: u32, proc: isize) -> usize;
    fn KillTimer(h: isize, id: usize) -> i32;
    fn PostQuitMessage(code: i32);
    fn GetClientRect(h: isize, rc: *mut Rect) -> i32;
    fn GetForegroundWindow() -> isize;
    fn GetWindowThreadProcessId(h: isize, pid: *mut u32) -> u32;
    fn GetGUIThreadInfo(tid: u32, info: *mut GuiThreadInfo) -> i32;
    fn ClientToScreen(h: isize, pt: *mut Point) -> i32;
    fn GetCursorPos(pt: *mut Point) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn SystemParametersInfoW(action: u32, param: u32, pv: *mut core::ffi::c_void, winini: u32) -> i32;
    fn SetProcessDPIAware() -> i32;
    fn SetProcessDpiAwarenessContext(ctx: isize) -> i32;
    fn GetDpiForWindow(h: isize) -> u32;
    fn GetModuleHandleW(name: *const u16) -> isize;
    fn GetMessageW(m: *mut Msg, h: isize, a: u32, b: u32) -> i32;
    fn TranslateMessage(m: *const Msg) -> i32;
    fn DispatchMessageW(m: *const Msg) -> isize;
    fn InvalidateRect(h: isize, rc: *const Rect, erase: i32) -> i32;
    fn GetDC(h: isize) -> isize;
    fn GetDeviceCaps(hdc: isize, index: i32) -> i32;
    fn ReleaseDC(h: isize, hdc: isize) -> i32;
    fn BeginPaint(h: isize, ps: *mut PaintStruct) -> isize;
    fn EndPaint(h: isize, ps: *const PaintStruct) -> i32;
    fn GetWindowRect(h: isize, rc: *mut Rect) -> i32;
    fn UpdateLayeredWindow(
        hwnd: isize,
        hdc_dst: isize,
        pt_dst: *mut Point,
        size: *mut Size,
        hdc_src: isize,
        pt_src: *mut Point,
        color_key: u32,
        blend: *const BlendFunction,
        flags: u32,
    ) -> i32;
}

// ------------------------------------------------------------------ 共享状态
/// 边缘光的颜色方案。
///
/// 为什么需要它：白色在深色背景上叠 42% 只是把背景提亮成**灰色雾** ——
/// 用户反馈"看不太清楚"，但再加亮就变成一片白，更难看。
/// 饱和色在同样的 alpha 下辨识度高得多（色彩对比 > 明度对比）。
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum EdgeTint {
/// 纯白（原来的方案）
#[default]
White,
/// 彩虹：四条边各一个色相，绕屏幕转一圈
Rainbow,
/// 暖橙：和 HUD 的琥珀色一致
Amber,
/// 青蓝：冷色调，夜里看不刺眼
Cyan,
/// 极光：青紫渐变，最"科技感"
Aurora,
}

impl EdgeTint {
pub fn from_conf(s: &str) -> EdgeTint {
    match s {
        "rainbow" => EdgeTint::Rainbow,
        "amber" => EdgeTint::Amber,
        "cyan" => EdgeTint::Cyan,
        "aurora" => EdgeTint::Aurora,
        _ => EdgeTint::White,
    }
}
pub fn as_conf(self) -> &'static str {
    match self {
        EdgeTint::White => "white",
        EdgeTint::Rainbow => "rainbow",
        EdgeTint::Amber => "amber",
        EdgeTint::Cyan => "cyan",
        EdgeTint::Aurora => "aurora",
    }
}
pub fn label(self) -> &'static str {
    match self {
        EdgeTint::White => "白光",
        EdgeTint::Rainbow => "彩虹光",
        EdgeTint::Amber => "暖橙光",
        EdgeTint::Cyan => "青蓝光",
        EdgeTint::Aurora => "极光（青紫）",
    }
}
}

/// 悬浮条 / 悬浮球二选一。
///
/// 为什么是互斥的：两个都画会互相压、抢视线。用户明确要求「一次最多只能显示一个」。
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Mode {
    /// 细条：只显示正文（录音/识别中的文字），半透明，不挡字。
    #[default]
    Bar,
    /// 圆球：只显示「正在录音」+ 实时声波波形，最小占位。
    Orb,
    /// 屏幕边缘光：四边渐亮，录音时呼吸、松手后淡出。
    /// 完全无遮挡 —— 不占任何中央区域。
    Edge,
}

#[derive(Clone, Default)]
struct State {
    visible: bool,
    mode: Mode,
    status: String,
    text: String,
    /// 最近若干帧的归一化音量（0..1），供悬浮球画波形。
    /// 固定长度环形缓冲的简单版：push 到尾部，超长就 drain 头部。
    levels: Vec<f32>,
    seq: u64,
    at: Option<(i32, i32)>,


/// 边缘光的当前强度（0..1），每帧朝 `edge_target` 缓动。
    /// 单独存是因为它要**平滑过渡**：松开时不能啪一下消失，要淡出。
    edge_level: f32,
    /// 边缘光的目标强度：录音中 1.0，松手后 0.0。
    edge_target: f32,
    edge_tint: EdgeTint,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

/// 当前光色。用原子量而不是只放 State 里：
/// 托盘换色时不需要 seq 变化（不重定位），只要下一帧重画。
static EDGE_TINT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn tint_code(t: EdgeTint) -> u8 {
    match t {
        EdgeTint::White => 0,
        EdgeTint::Rainbow => 1,
        EdgeTint::Amber => 2,
        EdgeTint::Cyan => 3,
        EdgeTint::Aurora => 4,
    }
}

pub fn set_edge_tint(t: EdgeTint) {
    EDGE_TINT.store(tint_code(t), Ordering::Relaxed);
    if let Ok(mut g) = state().lock() {
        g.edge_tint = t;
    }
}

pub fn edge_tint() -> EdgeTint {
    match EDGE_TINT.load(Ordering::Relaxed) {
        1 => EdgeTint::Rainbow,
        2 => EdgeTint::Amber,
        3 => EdgeTint::Cyan,
        4 => EdgeTint::Aurora,
        _ => EdgeTint::White,
    }
}
static LAST_SEQ: OnceLock<Mutex<u64>> = OnceLock::new();

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}

fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// 悬浮状态条的句柄。
///
/// `start()` 是**启动时就开**；`ensure()` 是**运行时按需开**（托盘开关用）。
/// 两者共用同一份全局窗口状态（`STATE`），所以重复调用不会创建第二个窗口 ——
/// 窗口线程已经活着时，`ensure()` 直接返回一个新句柄。
#[derive(Clone)]
pub struct Overlay {
    enabled: bool,
}

/// 窗口线程是否已经起来了（`start()`/`ensure()` 成功过一次就为 true）。
static THREAD_UP: AtomicBool = AtomicBool::new(false);

impl Overlay {
    /// 运行时按需启动：已经起来就直接返回句柄，没起来就起一次。
    ///
    /// 为什么需要它：托盘里打开「悬浮条」只改了一个原子量，
    /// 而窗口是启动时决定要不要创建的 —— 不补这一条，开关点了永远没反应。
    pub fn ensure() -> Option<Overlay> {
        if THREAD_UP.load(Ordering::Relaxed) {
            return Some(Overlay { enabled: true });
        }
        Overlay::start()
    }

    pub fn start() -> Option<Overlay> {
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        std::thread::Builder::new()
            .name("sainput-overlay".into())
            .spawn(move || {
                let hwnd = unsafe { create_window() };
                match hwnd {
                    Some(h) => {
                        unsafe {
                            SetTimer(h, 1, 16, 0);
                        }
                        let _ = tx.send(true);
                        unsafe {
                            let mut msg: Msg = std::mem::zeroed();
                            while GetMessageW(&mut msg, 0, 0, 0) > 0 {
                                TranslateMessage(&msg);
                                DispatchMessageW(&msg);
                            }
                        }
                    }
                    None => {
                        let _ = tx.send(false);
                    }
                }
            })
            .ok()?;
        match rx.recv_timeout(Duration::from_millis(2500)) {
            Ok(true) => {
                THREAD_UP.store(true, Ordering::Relaxed);
                Some(Overlay { enabled: true })
            }
            _ => None,
        }
    }

    pub fn show(&self, status: &str, text: &str) {
        self.show_at(None, status, text);
    }

    /// 切到「细条」模式并显示文字（不显示状态行 —— 用户要求撤掉）。
    ///
    /// 🔴 空文字时**不显示**，直接隐藏。
    ///
    /// 为什么必须这样：尾巴收完时主循环会调 `show_bar("")` 占位，等 final 回来。
    /// 早先它真的会画出一个空面板 —— 而且如果 final 因为打断/空结果没来，
    /// 这个空面板就**永远挂在屏幕上**（用户实测报过）。
    /// 空内容没有任何信息量，显示它只会碍事。
    pub fn show_bar(&self, text: &str) {
        if !self.enabled {
            return;
        }
        if let Ok(mut s) = state().lock() {
            s.mode = Mode::Bar;
            s.status.clear();
            s.text = text.to_string();
            s.at = None;
            s.seq += 1;
            if text.is_empty() {
                s.visible = false;
            } else {
                s.visible = true;
            }
        }
    }

    /// 切到「悬浮球」模式（只表示正在录音 + 波形）。
    pub fn show_orb(&self) {
        if !self.enabled {
            return;
        }
        if let Ok(mut s) = state().lock() {
            s.visible = true;
            s.mode = Mode::Orb;
            s.status.clear();
            s.text.clear();
            s.at = None;
            s.seq += 1;
        }
    }

    /// 切到「屏幕边缘光」模式并开始亮起（录音开始）。
    ///
    /// 注意：这里**不直接设 edge_level**，只设目标值 ——
    /// 真正的亮度由窗口线程按帧缓动，否则会"啪"一下亮起来，很突兀。
    pub fn show_edge(&self) {
        if !self.enabled {
            return;
        }
        if let Ok(mut s) = state().lock() {
            s.visible = true;
            s.mode = Mode::Edge;
            s.status.clear();
            s.text.clear();
            s.at = None;
            s.edge_target = 1.0;
            s.seq += 1;
        }
    }

    /// 让边缘光淡出（松手后调用）。
    ///
    /// 不直接 hide：淡出要几百毫秒，期间窗口必须还在、还得继续重画。
    /// 亮度降到 0 时窗口线程自己会把它藏起来。
    pub fn fade_edge(&self) {
        if !self.enabled {
            return;
        }
        if let Ok(mut s) = state().lock() {
            s.edge_target = 0.0;
            s.seq += 1;
        }
    }

    /// 推一帧音量（0..1）给悬浮球画波形。
    ///
    /// 只在 Orb 模式下有意义；Bar 模式直接丢掉，免得白占内存。
    /// 不递增 seq —— 波形每 40ms 变一次，若算作"内容变化"会让整个 HUD
    /// 每帧都重新定位+重画，反而闪。
    pub fn push_level(&self, level: f32) {
        if !self.enabled {
            return;
        }
        if let Ok(mut s) = state().lock() {
            if s.mode != Mode::Orb {
                return;
            }
            let v = level.clamp(0.0, 1.0);
            s.levels.push(v);
            // 40 个点 ≈ 1.6 秒的历史，够看出说话节奏，又不至于拖太长
            if s.levels.len() > 40 {
                let drop_n = s.levels.len() - 40;
                s.levels.drain(0..drop_n);
            }
        }
    }

    pub fn show_at(&self, at: Option<(i32, i32)>, status: &str, text: &str) {
        if !self.enabled {
            return;
        }
        if let Ok(mut s) = state().lock() {
            s.visible = true;
            s.status = status.to_string();
            s.text = text.to_string();
            s.at = at;
            s.seq += 1;
        }
    }

    pub fn hide(&self) {
        if !self.enabled {
            return;
        }
        if let Ok(mut s) = state().lock() {
            s.visible = false;
            s.seq += 1;
        }
    }

    pub fn hide_after(&self, ms: u64) {
        if !self.enabled {
            return;
        }
        let cur = state().lock().map(|s| s.seq).unwrap_or(0);
        let me = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            let now = state().lock().map(|s| s.seq).unwrap_or(0);
            if now == cur {
                me.hide();
            }
        });
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

unsafe fn set_dpi_aware() {
    if SetProcessDpiAwarenessContext(-4isize) == 0 {
        SetProcessDPIAware();
    }
}

unsafe fn create_window() -> Option<isize> {
    set_dpi_aware();
    let inst = GetModuleHandleW(ptr::null());
    let cls = wide(OVERLAY_CLASS);
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
    let atom = RegisterClassW(&wc);
    if atom == 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(1410) {
            return None;
        }
    }
    let title = wide("");
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
        cls.as_ptr(),
        title.as_ptr(),
        WS_POPUP,
        -4000,
        -4000,
        8,
        8,
        0,
        0,
        inst,
        0,
    );
    if hwnd == 0 {
        return None;
    }
    Some(hwnd)
}

unsafe extern "system" fn wnd_proc(hwnd: isize, msg: u32, wp: usize, lp: isize) -> isize {
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE,
        WM_ERASEBKGND => 1, // Direct2D 自己画，不让系统擦
        WM_PAINT => {
            // UpdateLayeredWindow 自己推位图，这里只需把窗口内容标脏
            let mut ps: PaintStruct = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let _ = hdc;
            EndPaint(hwnd, &ps);
            0
        }
        WM_TIMER => {
            sync(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 从共享状态同步窗口：显示/隐藏、定位、尺寸。
unsafe fn sync(hwnd: isize) {
    let (visible, seq, mode, status, text, at) = match state().lock() {
        Ok(s) => (
            s.visible,
            s.seq,
            s.mode,
            s.status.clone(),
            s.text.clone(),
            s.at,
        ),
        Err(_) => return,
    };

    // 边缘光：先做缓动（每帧朝目标走一小步），再决定窗口要不要显示。
    // 这段必须在 visible 判断之前 —— 淡出过程中 visible 仍是 true，
    // 但亮度降到 0 之后要能自己收起来。
    let mut edge_done = false;
    if mode == Mode::Edge {
        let mut g = match state().lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let target = g.edge_target;
        let cur = g.edge_level;
        // 每 16ms 一拍（≈60fps）。系数 0.06 → 淡入约 350ms、淡出约 500ms。
        // 早先 40ms + 0.18，理论 200ms 就该亮完，但实测每帧要 247ms，
        // 十几帧被拖成好几秒 —— 用户看到的是"啪一下出现"。帧率才是丝滑的前提。
        let next = if (target - cur).abs() < 0.006 {
            target
        } else {
            cur + (target - cur) * 0.06
        };
        g.edge_level = next;
        if target == 0.0 && next <= 0.01 {
            g.visible = false;
            edge_done = true;
        }
    }
    let visible = if edge_done {
        match state().lock() {
            Ok(g) => g.visible,
            Err(_) => return,
        }
    } else {
        visible
    };

    let changed = {
        let mut last = LAST_SEQ.get_or_init(|| Mutex::new(u64::MAX)).lock().unwrap();
        if *last != seq {
            *last = seq;
            true
        } else {
            false
        }
    };
    // 悬浮球的波形是逐帧变的，但它不该让窗口重新定位/重排 —— 所以单独标一个"要重画"。
    let repaint = changed || mode == Mode::Orb || mode == Mode::Edge;

    if !visible {
        // 不管 seq 变没变都要尝试隐藏 —— 窗口可能刚被外部（或上一轮）弄成可见，
        // 而 seq 恰好没变，就会出现「关了但还挂在屏幕上」。
        if changed || IsWindowVisible(hwnd) != 0 {
            ShowWindow(hwnd, SW_HIDE);
        }
        return;
    }

    // 计算尺寸
    let (w, h) = match mode {
        Mode::Bar => measure(&status, &text),
        Mode::Orb => orb_size(),
        // 边缘光铺满整个屏幕（物理像素）
        Mode::Edge => (GetSystemMetrics(0), GetSystemMetrics(1)),
    };
    // 固定在屏幕**中下方**、任务栏上面一点点（用户明确要求）。
    // 不再跟随光标/鼠标 —— 跟随会在打字时来回跳，反而更干扰。
    // `at` 仍然保留：overlay-demo 用它把窗口摆到指定坐标做预览。
    let (mut x, mut y) = match mode {
        // 边缘光永远贴 (0,0) 铺满，不走锚点
        Mode::Edge => (0, 0),
        _ => match at {
            Some((ax, ay)) => (ax, ay),
            None => anchor_pos(w, h),
        },
    };

    // 屏幕边界钳制
    let sw = GetSystemMetrics(0);
    let sh = GetSystemMetrics(1);
    if x + w > sw {
        x = sw - w - 8;
    }
    if y + h > sh {
        y = sh - h - 8;
    }
    if x < 0 {
        x = 8;
    }
    if y < 0 {
        y = 8;
    }

    SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
    if changed {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    // UpdateLayeredWindow 是"推"模型：每帧主动重画，不依赖 WM_PAINT
    if repaint {
        paint(hwnd);
    }
}

/// 细条尺寸：**只有一行正文**，没有状态行、没有分隔线。
///
/// 用户要求撤掉「录音中…」那一行 —— 状态由悬浮球负责，细条只管显示文字。
/// 这样高度从 96 设计像素降到 44，不再挡字。
fn measure(_status: &str, text: &str) -> (i32, i32) {
    // 粗略估宽：真排版在 paint 里用 DirectWrite 做，这里只需要一个够用的宽度。
    let estimate = |s: &str| -> i32 {
        s.chars()
            .map(|c| if c.is_ascii() { px(BAR_CHAR_ASCII) } else { px(BAR_CHAR_CJK) })
            .sum::<i32>()
    };
    let text_w = estimate(text);
    let w = (text_w + px(BAR_PAD_X) * 2).clamp(px(BAR_MIN_W), px(BAR_MAX_W));

    // 正文行数
    let usable = (w - px(BAR_PAD_X) * 2).max(px(80));
    let lines = if text_w <= usable {
        1
    } else {
        (text_w as f32 / usable as f32).ceil() as i32
    };
    let h = px(BAR_PAD_Y) * 2 + lines * px(BAR_LINE_H);
    (w, h.max(px(BAR_MIN_H)))
}

/// 悬浮球尺寸：正方形，边长固定。
fn orb_size() -> (i32, i32) {
    let d = px(ORB_D);
    (d, d)
}

/// 屏幕中下方、任务栏正上方。
///
/// 为什么不用工作区（`SystemParametersInfo(SPI_GETWORKAREA)`）：
/// 那条 API 在多显示器 / 自动隐藏任务栏下给出的矩形不一定准，
/// 而用户要的是"任务栏上面一点点"这个**视觉**位置 —— 直接用屏幕高度减去
/// 任务栏高度（`SPI_GETWORKAREA` 拿不到时退回固定 56px 间距）最稳。
fn anchor_pos(w: i32, h: i32) -> (i32, i32) {
    let sw = unsafe { GetSystemMetrics(0) };
    let sh = unsafe { GetSystemMetrics(1) };
    let x = (sw - w) / 2;
    // 任务栏高度：用 SPI_GETWORKAREA 的工作区底边反推；拿不到就用屏幕底部留白。
    let work_bottom = unsafe {
        let mut r: Rect = std::mem::zeroed();
        if SystemParametersInfoW(0x0030, 0, &mut r as *mut Rect as *mut _, 0) != 0 {
            r.bottom
        } else {
            sh
        }
    };
    // 工作区底边就是任务栏顶边。再往上留 BOTTOM_GAP，让卡片不贴着任务栏。
    let y = work_bottom - h - BOTTOM_GAP;
    (x.max(0), y.max(0))
}

/// 贴着插入符定位；拿不到就贴鼠标。
///
/// ⚠️ 现在**正常路径不用它**（用户要求固定在屏幕中下方）。
/// 保留是因为 `overlay-demo --at` 之外还有调试价值，且以后若要做"跟随模式"可直接复用。
#[allow(dead_code)]
unsafe fn caret_pos(hwnd: isize, w: i32, h: i32) -> (i32, i32) {
    let mut anchor: Option<Point> = None;
    let fg = GetForegroundWindow();
    if fg != 0 {
        let tid = GetWindowThreadProcessId(fg, ptr::null_mut());
        let mut gi = GuiThreadInfo {
            cb_size: std::mem::size_of::<GuiThreadInfo>() as u32,
            ..Default::default()
        };
        if tid != 0 && GetGUIThreadInfo(tid, &mut gi) != 0 && gi.hwnd_caret != 0 {
            let mut pt = Point {
                x: gi.rc_caret.left,
                y: gi.rc_caret.bottom,
            };
            if ClientToScreen(gi.hwnd_caret, &mut pt) != 0 {
                anchor = Some(pt);
            }
        }
    }
    let mut pt = anchor.unwrap_or_else(|| {
        let mut p = Point { x: 0, y: 0 };
        GetCursorPos(&mut p);
        p
    });
    // 放在插入符右下方，留 6px 间隙
    pt.x += 6;
    pt.y += 6;
    let _ = hwnd;
    (pt.x, pt.y)
}

// ------------------------------------------------------------------ 绘制
/// Direct2D 绘制。
///
/// ⚠️ 这里刻意用**最小可用**的 D2D 调用面：
///   · 圆角矩形（真抗锯齿）
///   · 两层圆角矩形模拟阴影（D2D 没有内建 drop shadow，用偏移+低透明度叠）
///   · DirectWrite 画文字
///
/// 如果 D2D 初始化失败（老系统/被禁用），**降级成什么也不画** ——
/// 悬浮条是锦上添花，绝不能因为它把主功能搞挂。
/// 绘制入口：走 **UpdateLayeredWindow**（分层窗口的正确画法）。
///
/// 为什么不能用 BeginPaint + Window DC：实测（examples/gdiprobe.rs + 本轮 D2D 版）
/// 在这条路径上 FillRect / RoundRect / TextOut / D2D 绑 HDC **全都不落笔** ——
/// 窗口存在、IsWindowVisible=1，但屏幕上什么都没有。
/// WS_EX_LAYERED 窗口要么被 alpha 机制接管（UpdateLayeredWindow），要么全隐形。
///
/// UpdateLayeredWindow 的做法：
///   1. 建一个 32bpp 的 DIBSection（自己的画布，不碰屏幕 DC）
///   2. 把 D2D 绑到这个 DIB 的 HDC 上画
///   3. UpdateLayeredWindow 把整块位图推给窗口（带 per-pixel alpha）
unsafe fn paint(hwnd: isize) {
    let (mode, status, text, levels) = match state().lock() {
        Ok(s) => (s.mode, s.status.clone(), s.text.clone(), s.levels.clone()),
        Err(_) => return,
    };
    // 尺寸由 measure()/orb_size() 决定，**不问窗口要**。
    //
    // 为什么：`WS_EX_LAYERED` 窗口的尺寸是 UpdateLayeredWindow 里那份 bitmap 决定的，
    // 而 SetWindowPos 设的尺寸在第一次 UpdateLayeredWindow 之后会被位图尺寸覆盖。
    // 早先这里用 GetClientRect，结果永远拿到上一次的旧尺寸 ——
    // measure 算出 520x109，窗口却一直画 231x48，看起来像"放大没生效"。
    let (w, h) = match mode {
        Mode::Bar => measure(&status, &text),
        Mode::Orb => orb_size(),
        Mode::Edge => (GetSystemMetrics(0), GetSystemMetrics(1)),
    };
    let rc = Rect { left: 0, top: 0, right: w, bottom: h };
    if w <= 0 || h <= 0 {
        return;
    }

    // 1) 建 32bpp DIB
    let mut bits: *mut u8 = ptr::null_mut();
    let bmi = BitmapInfo {
        header: BitmapInfoHeader {
            bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
            bi_width: w,
            bi_height: -h,          // 负 = 自上而下
            bi_planes: 1,
            bi_bit_count: 32,
            bi_compression: 0,      // BI_RGB
            bi_size_image: (w * h * 4) as u32,
            ..Default::default()
        },
        colors: [0; 3],
    };
    let mem_dc = CreateCompatibleDC(0);
    if mem_dc == 0 {
        return;
    }
    let dib = CreateDIBSection(mem_dc, &bmi, 0, &mut bits, 0, 0);
    if dib == 0 || bits.is_null() {
        DeleteDC(mem_dc);
        return;
    }
    let old_bmp = SelectObject(mem_dc, dib);

    // 2) 清零（全透明）
    ptr::write_bytes(bits, 0, (w * h * 4) as usize);

    // 3) D2D 画到这块 DIB 上
    match mode {
        Mode::Bar => d2d_paint_bar(mem_dc, &rc, &text),
        Mode::Orb => d2d_paint_orb(mem_dc, &rc, &levels),
        Mode::Edge => {
            let lv = state().lock().map(|g| g.edge_level).unwrap_or(0.0);
            d2d_paint_edge(mem_dc, &rc, lv);
        }
    }


    // 4) 推给窗口（带 alpha）
    let mut pt_src = Point { x: 0, y: 0 };
    let mut sz = Size { cx: w, cy: h };
    let blend = BlendFunction {
        blend_op: 0,   // AC_SRC_OVER
        blend_flags: 0,
        source_constant_alpha: 255,
        alpha_format: 1, // AC_SRC_ALPHA
    };
    let mut pt_dst = Point { x: 0, y: 0 };
    // 目标位置从窗口当前 rect 取
    let mut wr: Rect = std::mem::zeroed();
    GetWindowRect(hwnd, &mut wr);
    pt_dst.x = wr.left;
    pt_dst.y = wr.top;

    UpdateLayeredWindow(
        hwnd,
        0,
        &mut pt_dst,
        &mut sz,
        mem_dc,
        &mut pt_src,
        0,
        &blend,
        2, // ULW_ALPHA
    );


    SelectObject(mem_dc, old_bmp);
    DeleteObject(dib);
    DeleteDC(mem_dc);
}

thread_local! {
    /// D2D 渲染目标缓存（窗口线程专用；COM 对象不是 Send）。
    static RT: std::cell::RefCell<Option<(
        windows::Win32::Graphics::Direct2D::ID2D1Factory,
        windows::Win32::Graphics::Direct2D::ID2D1DCRenderTarget,
        windows::Win32::Graphics::Direct2D::ID2D1RenderTarget,
    )>> = const { std::cell::RefCell::new(None) };

    /// 上一次 D2D 报错的时刻和内容。
    /// 窗口线程在设备丢失时会按 60fps 重试，不设限流会把控制台/日志刷爆。
    static LAST_D2D_ERR: std::cell::RefCell<Option<(std::time::Instant, String)>> =
        const { std::cell::RefCell::new(None) };
}

/// 记录 D2D 错误：同一条错误最多 2 秒输出一次。
fn log_d2d_err(stage: &str, e: &windows::core::Error) {
    LAST_D2D_ERR.with(|slot| {
        let mut slot = slot.borrow_mut();
        let msg = format!("{stage}: {e:?}");
        let now = std::time::Instant::now();
        let should = match slot.as_ref() {
            Some((at, prev)) => {
                prev != &msg || now.duration_since(*at) >= std::time::Duration::from_secs(2)
            }
            None => true,
        };
        if should {
            eprintln!("[ov] D2D {msg}");
            *slot = Some((now, msg));
        }
    });
}

/// 建一个绑到 DIB 上的 D2D render target，跑 `f`，收尾。
///
/// 抽出来是因为细条和悬浮球用的是**同一套** D2D 初始化 ——
/// 复制两遍的话，以后改 alpha 模式/像素格式要改两处，容易漏。
fn with_target(hdc: isize, rc: &Rect, f: impl FnOnce(&d2d_target::Target)) {
    use windows::core::Interface;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Direct2D as d2d;
    use windows::Win32::Graphics::Direct2D::Common as d2dc;
    use windows::Win32::Graphics::Gdi::HDC;

    // 🔴 性能关键：factory 和 render target **必须复用**。
    //
    // 早先每次都新建，实测在 3840x2160 下 `draw` 阶段要 **247ms**（≈4fps）——
    // 用户看到的就是"边缘光啪一下出现"，因为淡入的十几帧被拖成了好几百毫秒。
    // 复用后同一段绘制降到毫秒级。
    //
    // 复用缓存；每帧只需要重新 BindDC（尺寸/位置可能变）
    RT.with(|slot| unsafe {
    let mut slot = slot.borrow_mut();
    let mut dead = false;
    if slot.is_none() {
        let factory: d2d::ID2D1Factory =
            match d2d::D2D1CreateFactory(d2d::D2D1_FACTORY_TYPE_SINGLE_THREADED, None) {
                Ok(f) => f,
                Err(e) => {
                    log_d2d_err("D2D1CreateFactory", &e);
                    return;
                }
            };

        let props = d2d::D2D1_RENDER_TARGET_PROPERTIES {
            r#type: d2d::D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: d2dc::D2D1_PIXEL_FORMAT {
                format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                // 必须 PREMULTIPLIED：IGNORE 会让整块变不透明（圆角/半透明全丢）
                alphaMode: d2dc::D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 0.0,
            dpiY: 0.0,
            usage: d2d::D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: d2d::D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let rt: d2d::ID2D1DCRenderTarget = match factory.CreateDCRenderTarget(&props) {
            Ok(r) => r,
            Err(e) => {
                log_d2d_err("CreateDCRenderTarget", &e);
                return;
            }
        };

        let sub = RECT {
            left: rc.left,
            top: rc.top,
            right: rc.right,
            bottom: rc.bottom,
        };
        if let Err(e) = rt.BindDC(HDC(hdc), &sub) {
            log_d2d_err("BindDC(create)", &e);
            return;
        }
        let target: d2d::ID2D1RenderTarget = match rt.cast() {
            Ok(t) => t,
            Err(e) => {
                log_d2d_err("cast<ID2D1RenderTarget>", &e);
                return;
            }
        };
        *slot = Some((factory, rt, target));
    }

    // 复用缓存；每帧只需要重新 BindDC（尺寸/位置可能变）
    let (factory, rt, target) = slot.as_ref().unwrap();
    let _ = factory;
    let sub = RECT {
        left: rc.left,
        top: rc.top,
        right: rc.right,
        bottom: rc.bottom,
    };
    // 🔴 设备丢失的唯一出口。
    //
    // GPU 驱动报错 / 睡眠唤醒后，Direct2D 设备会被重置：旧 target 的
    // EndDraw 返回 D2DERR_RECREATE_TARGET（BindDC 也可能直接失败）。
    // 以前这里只打印一行、继续用同一个死 target —— 之后每一帧都画进
    // 不存在的设备，DIB 保持全透明，用户看到的就是「彩虹边缘光突然
    // 消失，但语音输入一切正常」，只能重启进程。
    // 现在只要出错就丢弃缓存，下一帧用新 target 重建，最多丢一帧。
    if let Err(e) = rt.BindDC(HDC(hdc), &sub) {
        log_d2d_err("BindDC", &e);
        dead = true;
    } else {
        target.BeginDraw();
        target.Clear(Some(&color(0, 0, 0, 0.0)));
        f(&d2d_target::Target { target });
        if let Err(e) = target.EndDraw(None, None) {
            log_d2d_err("EndDraw", &e);
            dead = true;
        }
    }
    if dead {
        *slot = None;
    }
    });
}

/// 把 render target 包一层，避免每个绘制函数都写一长串 windows crate 路径。
mod d2d_target {
    use windows::Win32::Graphics::Direct2D as d2d;

    pub struct Target<'a> {
        pub target: &'a d2d::ID2D1RenderTarget,
    }
}

/// 细条：**只有正文**，半透明圆角矩形。
///
/// 用户要求撤掉「录音中…」状态行 —— 状态交给悬浮球，细条只管文字。
fn d2d_paint_bar(hdc: isize, rc: &Rect, text: &str) {
    use windows::Win32::Graphics::Direct2D as d2d;
    use windows::Win32::Graphics::Direct2D::Common as d2dc;

    with_target(hdc, rc, |t| unsafe {
        let target = t.target;
        let w = (rc.right - rc.left) as f32;
        let h = (rc.bottom - rc.top) as f32;

        let body = d2d::D2D1_ROUNDED_RECT {
            rect: d2dc::D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: w,
                bottom: h,
            },
            radiusX: px(BAR_RADIUS) as f32,
            radiusY: px(BAR_RADIUS) as f32,
        };
        if let Ok(b) = target.CreateSolidColorBrush(&argb(BAR_BG) as *const _, None) {
            target.FillRoundedRectangle(&body, &b);
        }
        if let Ok(b) = target.CreateSolidColorBrush(&argb(BAR_BORDER) as *const _, None) {
            target.DrawRoundedRectangle(&body, &b, 1.0, None);
        }

        draw_bar_text(target, &body.rect, text);
    });
}

/// 悬浮球：圆形底 + 呼吸环 + 实时波形。
///
/// 波形是**真实音量**驱动的（见 `Overlay::push_level`），不是装饰动画 ——
/// 用户说话时才会起伏，安静时是一条平线。
fn d2d_paint_orb(hdc: isize, rc: &Rect, levels: &[f32]) {
    use windows::Win32::Graphics::Direct2D as d2d;
    use windows::Win32::Graphics::Direct2D::Common as d2dc;

    with_target(hdc, rc, |t| unsafe {
        let target = t.target;
        let d = (rc.right - rc.left) as f32;
        let c = d / 2.0;
        let r = c - px(2) as f32;

        // 底：实心圆
        if let Ok(b) = target.CreateSolidColorBrush(&argb(ORB_BG) as *const _, None) {
            target.FillEllipse(
                &d2d::D2D1_ELLIPSE {
                    point: d2dc::D2D_POINT_2F { x: c, y: c },
                    radiusX: r,
                    radiusY: r,
                },
                &b,
            );
        }
        // 环：琥珀细边
        if let Ok(b) = target.CreateSolidColorBrush(&argb(ORB_RING) as *const _, None) {
            target.DrawEllipse(
                &d2d::D2D1_ELLIPSE {
                    point: d2dc::D2D_POINT_2F { x: c, y: c },
                    radiusX: r,
                    radiusY: r,
                },
                &b,
                px(2) as f32,
                None,
            );
        }

        // 波形：在球心横排若干竖条，高度由最近几帧音量决定
        let n = ORB_BARS;
        let bar_w = px(ORB_BAR_W) as f32;
        let gap = px(ORB_BAR_GAP) as f32;
        let total = n as f32 * bar_w + (n as f32 - 1.0) * gap;
        let x0 = c - total / 2.0;
        let max_h = px(ORB_BAR_MAX_H) as f32;
        let min_h = px(ORB_BAR_MIN_H) as f32;

        if let Ok(b) = target.CreateSolidColorBrush(&argb(ACCENT) as *const _, None) {
            for i in 0..n {
                // 取最近 n 帧；不够就用 0 补齐（刚开球时是一条平线，符合预期）
                let lv = levels
                    .get(levels.len().saturating_sub(n) + i)
                    .copied()
                    .unwrap_or(0.0);
                // 开方压一下动态范围：小声说话也能看出起伏
                let bh = min_h + (max_h - min_h) * lv.sqrt();
                let x = x0 + i as f32 * (bar_w + gap);
                let rect = d2dc::D2D_RECT_F {
                    left: x,
                    top: c - bh / 2.0,
                    right: x + bar_w,
                    bottom: c + bh / 2.0,
                };
                // 圆角竖条比直角好看，半径取半宽
                let rr = d2d::D2D1_ROUNDED_RECT {
                    rect,
                    radiusX: bar_w / 2.0,
                    radiusY: bar_w / 2.0,
                };
                target.FillRoundedRectangle(&rr, &b);
            }
        }
    });
}

fn argb(c: u32) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
    let a = ((c >> 24) & 0xFF) as f32 / 255.0;
    let r = ((c >> 16) & 0xFF) as f32 / 255.0;
    let g = ((c >> 8) & 0xFF) as f32 / 255.0;
    let b = (c & 0xFF) as f32 / 255.0;
    windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { r, g, b, a }
}

fn color(r: u8, g: u8, b: u8, a: f32) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
    windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

/// 屏幕边缘光：四条边各画一道从边缘向内的渐变。
///
/// ## 怎么做出「流动的彩色」
///
/// 难点：颜色要**沿边缘**变化（彩虹），亮度要**向内**衰减（光带）——
/// 这两个方向互相垂直，一个线性渐变刷做不到。
///
/// 做法是 **PushLayer + opacityBrush**：
///   1. 用一个「边缘不透明 → 向内全透明」的渐变当**蒙版**压入图层
///   2. 在图层里用**沿边缘的多色标渐变**填满整块矩形
///   3. PopLayer —— 彩虹被蒙版裁成光带形状
///
/// 这比"画很多层半透明矩形"干净得多，也没有层间接缝。
///
/// ## 周长连续
///
/// 四条边的色相按**周长比例**分配（不是每条边固定 0.25）：
/// 屏幕宽高不等时，按长度切才能让流动速度均匀。
/// 再叠加 `phase` 让整条彩虹沿周长滑动 —— 这就是"会动的彩色"。
fn d2d_paint_edge(hdc: isize, rc: &Rect, level: f32) {
    use windows::Win32::Graphics::Direct2D as d2d;
    use windows::Win32::Graphics::Direct2D::Common as d2dc;

    if level <= 0.001 {
        return;
    }

    let tint = edge_tint();

    with_target(hdc, rc, |t| unsafe {
        let target = t.target;
        let w = (rc.right - rc.left) as f32;
        let h = (rc.bottom - rc.top) as f32;
        let spread = px(EDGE_SPREAD) as f32;

        // 呼吸：0.86~1.0 之间缓慢起伏。
        // 相位都在 0..1，见 edge_clock（用绝对时间会丢精度）。
        let (breath_ph, flow_ph) = edge_clock();
        let breath = 0.86 + 0.14 * (breath_ph * std::f32::consts::TAU).sin();

        // 饱和色比白色"显眼"得多，所以不用提 alpha 就能看清 —— 反而要略降。
        let tint_boost = match tint {
            EdgeTint::White => 1.0,
            _ => 0.92,
        };
        let peak = EDGE_PEAK_ALPHA * level * breath * tint_boost;
        if peak <= 0.002 {
            return;
        }

        // ---- 光带蒙版：边缘处不透明 → 向内全透明 ----
        let fade_stops = [
            d2dc::D2D1_GRADIENT_STOP {
                position: 0.0,
                color: d2dc::D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: peak },
            },
            d2dc::D2D1_GRADIENT_STOP {
                position: 1.0,
                color: d2dc::D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.0 },
            },
        ];
        let Ok(fade_coll) = target.CreateGradientStopCollection(
            &fade_stops,
            d2d::D2D1_GAMMA_2_2,
            d2d::D2D1_EXTEND_MODE_CLAMP,
        ) else {
            return;
        };

        // ---- 画一条边 ----
        let draw_edge = |p0: d2dc::D2D_POINT_2F,
                         p1: d2dc::D2D_POINT_2F,
                         t0: f32,
                         t1: f32,
                         rect: d2dc::D2D_RECT_F,
                         fade_from: d2dc::D2D_POINT_2F,
                         fade_to: d2dc::D2D_POINT_2F| {
            // 颜色色标：沿边缘取 N 个采样点。
            // N 越大过渡越细，但色标太多也会慢 —— 17 在 4K 下已经看不出色带。
            const N: usize = 17;
            let mut stops: [d2dc::D2D1_GRADIENT_STOP; N] = [d2dc::D2D1_GRADIENT_STOP {
                position: 0.0,
                color: d2dc::D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 },
            }; N];
            for (k, slot) in stops.iter_mut().enumerate() {
                let u = k as f32 / (N - 1) as f32;
                let tt = t0 + (t1 - t0) * u;
                let (r, g, b) = edge_color_at(tint, tt, flow_ph);
                slot.position = u;
                slot.color = d2dc::D2D1_COLOR_F { r, g, b, a: 1.0 };
            }
            let Ok(ccoll) = target.CreateGradientStopCollection(
                &stops,
                d2d::D2D1_GAMMA_2_2,
                d2d::D2D1_EXTEND_MODE_CLAMP,
            ) else {
                return;
            };
            let Ok(cbrush) = target.CreateLinearGradientBrush(
                &d2d::D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: p0,
                    endPoint: p1,
                },
                None,
                &ccoll,
            ) else {
                return;
            };
            let Ok(fbrush) = target.CreateLinearGradientBrush(
                &d2d::D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: fade_from,
                    endPoint: fade_to,
                },
                None,
                &fade_coll,
            ) else {
                return;
            };

            // 压入带蒙版的图层 → 画彩虹 → 弹出
            let Ok(layer) = target.CreateLayer(None) else {
                return;
            };
            let brush: d2d::ID2D1Brush = fbrush.clone().into();
            let params = d2d::D2D1_LAYER_PARAMETERS {
                contentBounds: rect,
                geometricMask: std::mem::ManuallyDrop::new(None),
                maskAntialiasMode: d2d::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: windows::Foundation::Numerics::Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(Some(brush)),
                layerOptions: d2d::D2D1_LAYER_OPTIONS_NONE,
            };
            target.PushLayer(&params, &layer);
            target.FillRectangle(&rect, &cbrush);
            target.PopLayer();
        };

        // 周长参数按**长度比例**分配，流动速度才均匀
        let per = 2.0 * (w + h);
        let tw = w / per;
        let th = h / per;

        // 上边（左→右）
        draw_edge(
            d2dc::D2D_POINT_2F { x: 0.0, y: 0.0 },
            d2dc::D2D_POINT_2F { x: w, y: 0.0 },
            0.0,
            tw,
            d2dc::D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: spread },
            d2dc::D2D_POINT_2F { x: 0.0, y: 0.0 },
            d2dc::D2D_POINT_2F { x: 0.0, y: spread },
        );
        // 右边（上→下）
        draw_edge(
            d2dc::D2D_POINT_2F { x: w, y: 0.0 },
            d2dc::D2D_POINT_2F { x: w, y: h },
            tw,
            tw + th,
            d2dc::D2D_RECT_F { left: w - spread, top: 0.0, right: w, bottom: h },
            d2dc::D2D_POINT_2F { x: w, y: 0.0 },
            d2dc::D2D_POINT_2F { x: w - spread, y: 0.0 },
        );
        // 下边（右→左）
        draw_edge(
            d2dc::D2D_POINT_2F { x: w, y: h },
            d2dc::D2D_POINT_2F { x: 0.0, y: h },
            tw + th,
            tw + th + tw,
            d2dc::D2D_RECT_F { left: 0.0, top: h - spread, right: w, bottom: h },
            d2dc::D2D_POINT_2F { x: 0.0, y: h },
            d2dc::D2D_POINT_2F { x: 0.0, y: h - spread },
        );
        // 左边（下→上）
        draw_edge(
            d2dc::D2D_POINT_2F { x: 0.0, y: h },
            d2dc::D2D_POINT_2F { x: 0.0, y: 0.0 },
            tw + th + tw,
            1.0,
            d2dc::D2D_RECT_F { left: 0.0, top: 0.0, right: spread, bottom: h },
            d2dc::D2D_POINT_2F { x: 0.0, y: 0.0 },
            d2dc::D2D_POINT_2F { x: spread, y: 0.0 },
        );
    });
}

/// 按周长参数 `t`（0..1）和流动相位 `flow`（0..1）取颜色。
///
/// 单色方案忽略 `t`（整条边一个色）；彩虹/极光沿 `t` 连续变化，
/// 再叠加 `flow` 让整条彩带**沿周长滑动** —— 这就是"流动的彩色"。
/// `flow` 已经在 0..1，直接加上去即可（一整圈）。
fn edge_color_at(tint: EdgeTint, t: f32, flow: f32) -> (f32, f32, f32) {
    match tint {
        EdgeTint::White => (1.0, 1.0, 1.0),
        EdgeTint::Amber => (1.0, 0.62, 0.26),
        EdgeTint::Cyan => (0.28, 0.82, 1.0),
        EdgeTint::Rainbow => {
            // 整条彩虹绕屏幕一圈，随时间滑动
            hue_to_rgb(t + flow)
        }
        EdgeTint::Aurora => {
            // 极光：只在青(0.47)~紫(0.77)之间流动，比全色相彩虹柔和
            let u = (t + flow) * std::f32::consts::TAU;
            let hue = 0.62 + 0.15 * u.sin();
            // 再叠一层亮度起伏，像极光的"帘幕"
            let (r, g, b) = hue_to_rgb(hue);
            let v = 0.82 + 0.18 * (u * 1.7).sin();
            (r * v, g * v, b * v)
        }
    }
}

/// HSV(h, s=1, v=1) -> RGB。h 取 0..1。
fn hue_to_rgb(h: f32) -> (f32, f32, f32) {
    let h = (h.fract() + 1.0).fract() * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    match i.rem_euclid(6) {
        0 => (1.0, f, 0.0),
        1 => (1.0 - f, 1.0, 0.0),
        2 => (0.0, 1.0, f),
        3 => (0.0, 1.0 - f, 1.0),
        4 => (f, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - f),
    }
}

/// 细条的文字绘制：**只有正文一行**，垂直居中。
fn draw_bar_text(
    target: &windows::Win32::Graphics::Direct2D::ID2D1RenderTarget,
    body: &windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F,
    text: &str,
) {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Direct2D as d2d;
    use windows::Win32::Graphics::Direct2D::Common as d2dc;
    use windows::Win32::Graphics::DirectWrite::{
        DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED,
        DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
        DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    };

    unsafe {
        let dwrite: IDWriteFactory = match DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) {
            Ok(f) => f,
            Err(e) => { eprintln!("[ov] DWriteCreateFactory failed: {e:?}"); return; }
        };
        let face = wide("Microsoft YaHei UI");
        let f_text = match dwrite.CreateTextFormat(
            PCWSTR(face.as_ptr()),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            BAR_FONT * scale(),
            PCWSTR(wide("zh-CN").as_ptr()),
        ) {
            Ok(f) => f,
            Err(e) => { eprintln!("[ov] CreateTextFormat failed: {e:?}"); return; }
        };

        // ⚠️ DrawText 默认是**左上对齐**。不设这两项的话，文字会贴着面板上边
        // 往右排 —— 看起来就是"歪的"（用户实测指出）。
        //   PARAGRAPH_ALIGNMENT_CENTER → 垂直居中
        //   TEXT_ALIGNMENT_CENTER      → 水平居中
        let _ = f_text.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        let _ = f_text.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);

        let tx = wide(text);
        let rect = d2dc::D2D_RECT_F {
            left: body.left + px(BAR_PAD_X) as f32,
            top: body.top,
            right: body.right - px(BAR_PAD_X) as f32,
            bottom: body.bottom,
        };
        if let Ok(br) = target.CreateSolidColorBrush(&argb(BAR_INK) as *const _, None) {
            let _ = target.DrawText(
                &tx[..tx.len().saturating_sub(1)],
                &f_text,
                &rect,
                &br,
                d2d::D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：D2D 目标失效后必须丢掉缓存，并在下一帧重建。
    ///
    /// 真实触发条件是 GPU 驱动报错 / 睡眠唤醒后的设备丢失：旧代码只
    /// eprintln、继续复用死 target，边缘光会永久消失（用户实测报过）。
    /// 这里用「已删除的 DC」当替身，让 BindDC 必然失败。
    #[test]
    fn d2d_target_cache_recovers_after_failure() {
        unsafe {
            let mem_dc = CreateCompatibleDC(0);
            assert!(mem_dc != 0, "CreateCompatibleDC failed");
            let bmi = BitmapInfo {
                header: BitmapInfoHeader {
                    bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                    bi_width: 64,
                    bi_height: -64,
                    bi_planes: 1,
                    bi_bit_count: 32,
                    bi_compression: 0,
                    bi_size_image: 64 * 64 * 4,
                    ..Default::default()
                },
                colors: [0; 3],
            };
            let mut bits: *mut u8 = ptr::null_mut();
            let dib = CreateDIBSection(mem_dc, &bmi, 0, &mut bits, 0, 0);
            assert!(dib != 0 && !bits.is_null(), "CreateDIBSection failed");
            let old_bmp = SelectObject(mem_dc, dib);

            let rc = Rect {
                left: 0,
                top: 0,
                right: 64,
                bottom: 64,
            };

            let mut drew = false;
            with_target(mem_dc, &rc, |_| drew = true);
            assert!(drew, "健康 target 应执行绘制");
            assert!(
                RT.with(|s| s.borrow().is_some()),
                "健康绘制后应留下缓存"
            );

            // 失败替身：先建再删，留下的句柄保证 BindDC 失败
            let stale = CreateCompatibleDC(0);
            assert!(stale != 0);
            DeleteDC(stale);
            with_target(stale, &rc, |_| {});
            assert!(
                RT.with(|s| s.borrow().is_none()),
                "BindDC 失败后必须丢弃缓存（旧 bug：继续复用死 target）"
            );

            // 下一帧用健康 DC：必须重建并继续画
            let mut drew_again = false;
            with_target(mem_dc, &rc, |_| drew_again = true);
            assert!(drew_again, "缓存重建后应继续绘制");
            assert!(RT.with(|s| s.borrow().is_some()));

            // 先清缓存再收尾，别让 target 绑着已删除的 DC
            RT.with(|s| *s.borrow_mut() = None);
            SelectObject(mem_dc, old_bmp);
            DeleteObject(dib);
            DeleteDC(mem_dc);
        }
    }
}
