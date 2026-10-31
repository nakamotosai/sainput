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
/// 鼠标穿透:窗口铺满整屏后**绝不能挡住点击**(配合 per-pixel alpha 全透明,用户看不到它)。
const WS_EX_TRANSPARENT: u32 = 0x0000_0020;

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

/// `SetThreadPriority` 的 THREAD_PRIORITY_BELOW_NORMAL。
const THREAD_PRIORITY_BELOW_NORMAL: i32 = -1;

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

    // 速度倍率直接作用在周期上(周期越短越快),下限留 200ms —— 再快就不是"呼吸"是闪了。
    let speed = edge_speed() as f64;
    let breath_ms = (EDGE_BREATH_MS as f64 / speed).max(200.0);
    let flow_ms = (EDGE_FLOW_MS as f64 / speed).max(600.0);
    let breath = ((ms % breath_ms) / breath_ms) as f32;
    let flow = ((ms % flow_ms) / flow_ms) as f32;
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
const EDGE_SPREAD_DEFAULT: i32 = 56;
pub const EDGE_SPREAD_MIN: i32 = 24;
pub const EDGE_SPREAD_MAX: i32 = 140;

// 带宽/亮度/速度做成**运行时**可调(HANDOFF 里那条已知限制:以前写死,托盘只能换色)。
//
// 为什么不是常量:这三个是纯**观感**参数,用户看一眼就知道要往哪边调;写死等于每次微调
// 都要改代码重编译。颜色早就做成了运行时的(`set_edge_tint`),这三个跟它同一套:
// 配置项给初值、托盘现场改、改完写回配置。
//
// f32 不放进原子量,存成千分位整数(量级足够、读回稳定)。
static EDGE_SPREAD_PX: std::sync::atomic::AtomicI32 =
    std::sync::atomic::AtomicI32::new(EDGE_SPREAD_DEFAULT);
static EDGE_PEAK_MILLI: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new((EDGE_PEAK_DEFAULT * 1000.0) as u32);
static EDGE_SPEED_MILLI: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new((EDGE_SPEED_DEFAULT * 1000.0) as u32);

/// 边缘光带宽(px)。
pub fn edge_spread() -> i32 {
    EDGE_SPREAD_PX
        .load(Ordering::Relaxed)
        .clamp(EDGE_SPREAD_MIN, EDGE_SPREAD_MAX)
}

/// 设带宽。越界自动夹到合法区间 —— 配置写错不该让 HUD 变成一堵墙、也不该让它消失。
pub fn set_edge_spread(px: i32) {
    EDGE_SPREAD_PX.store(px.clamp(EDGE_SPREAD_MIN, EDGE_SPREAD_MAX), Ordering::Relaxed);
}

/// 边缘光亮度(最亮处的 alpha,0..1)。
pub fn edge_peak() -> f32 {
    (EDGE_PEAK_MILLI.load(Ordering::Relaxed) as f32 / 1000.0).clamp(EDGE_PEAK_MIN, EDGE_PEAK_MAX)
}

pub fn set_edge_peak(v: f32) {
    let v = v.clamp(EDGE_PEAK_MIN, EDGE_PEAK_MAX);
    EDGE_PEAK_MILLI.store((v * 1000.0) as u32, Ordering::Relaxed);
}

/// 边缘光动画速度倍率(1.0 = 默认节奏)。
pub fn edge_speed() -> f32 {
    (EDGE_SPEED_MILLI.load(Ordering::Relaxed) as f32 / 1000.0)
        .clamp(EDGE_SPEED_MIN, EDGE_SPEED_MAX)
}

pub fn set_edge_speed(v: f32) {
    let v = v.clamp(EDGE_SPEED_MIN, EDGE_SPEED_MAX);
    EDGE_SPEED_MILLI.store((v * 1000.0) as u32, Ordering::Relaxed);
}
/// 最亮处的白色不透明度（0..1）。
const EDGE_PEAK_DEFAULT: f32 = 0.42;
pub const EDGE_PEAK_MIN: f32 = 0.10;
pub const EDGE_PEAK_MAX: f32 = 0.90;
const EDGE_SPEED_DEFAULT: f32 = 1.0;
pub const EDGE_SPEED_MIN: f32 = 0.4;
pub const EDGE_SPEED_MAX: f32 = 3.0;
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

#[link(name = "kernel32")]
extern "system" {
    /// 伪句柄(`(HANDLE)-2`),指的是"当前线程",**不需要也不能** CloseHandle。
    fn GetCurrentThread() -> isize;
    fn SetThreadPriority(h: isize, p: i32) -> i32;
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

/// 窗口是否已经摆好。窗口**只 ShowWindow 一次**,之后永不 hide/show ——
/// 显隐靠 alpha(见 sync 的注释:分层窗口尺寸/显隐变化会让系统重建表面,而那份表面不回收)。
static WINDOW_PLACED: AtomicBool = AtomicBool::new(false);
/// 上一帧是不是"有内容"的帧。隐藏时据此补一帧全透明(否则最后那帧会留在屏幕上)。
static PAINTED: AtomicBool = AtomicBool::new(false);

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
                // 体检报告 §5.2(线程优先级):这条线程只负责显示,降优先级 ——
                // 它晚一帧没人看得出来(悬浮条/边缘光本来就是提示性的),
                // 而识别线程晚一帧就是掉字。放在进消息循环之前设一次即可,
                // 优先级是线程属性,之后不会再变。
                unsafe {
                    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
                }
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
    // 窗口**一次性铺满整屏**(物理像素)并且之后永不改尺寸、永不 hide:
    // 分层窗口的尺寸一变,系统就会重建它的表面,而那份表面在 NVIDIA 驱动上不回收
    // (见 sync 的注释:每说一句漏一块 32 MiB)。细条/悬浮球改成画在画布内的对应位置。
    // WS_EX_TRANSPARENT 保证这块全屏窗口不吃鼠标点击。
    let sw = GetSystemMetrics(0).max(1);
    let sh = GetSystemMetrics(1).max(1);
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
        cls.as_ptr(),
        title.as_ptr(),
        WS_POPUP,
        0,
        0,
        sw,
        sh,
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

/// 悬浮球的波形自**上次真正画到屏幕上**以来变没变。
///
/// 为什么不能拿 `seq` 当门控:波形变化**不算**「内容变化」——
/// `push_level` 的注释解释过,若算作内容变化会让整个 HUD 每帧重新定位+重画(反而闪)。
/// 所以只能拿「上一帧画出去的那份 levels」和现在这份比。
/// 顺带天然地合并帧:中间漏画几拍没关系,下一帧一定把最新的画出去,不会丢。
/// 安静时主循环会一直推同一个数(比如 0.0),40 点滑窗内容完全不变 —— 那就一帧都不画。
fn orb_levels_changed() -> bool {
    let cur = match state().lock() {
        Ok(s) => s.levels.clone(),
        Err(_) => return false,
    };
    LAST_ORB_LEVELS.with(|slot| {
        let mut last = slot.borrow_mut();
        if last.as_deref() == Some(cur.as_slice()) {
            false
        } else {
            *last = Some(cur);
            true
        }
    })
}

/// 从共享状态同步窗口:内容、位置、显隐。
///
/// 🔴 窗口**尺寸恒定 = 整屏**,而且只在第一次 ShowWindow,之后永不 hide/show。
///
/// 为什么必须这样(2026-10-30 实测,别改回去):原来每说一句都在
/// 「整屏(边缘光)↔ 小面板(细条)」之间改窗口尺寸,并 hide/show 一次。
/// 分层窗口(`WS_EX_LAYERED`)的尺寸由 `UpdateLayeredWindow` 推的那块位图决定 ——
/// 尺寸一变,系统就要为这个窗口**重建一份表面**;实测这份表面在 NVIDIA 驱动上
/// **不会回收**:每说一句进程里多一块 32 MiB(4K 下正好 2^25 字节)的私有提交,
/// 半天 2 GB+,最后把整机提交额度吃光 → DWM `STATUS_FATAL_MEMORY_EXHAUSTION`
/// → 全屏黑 + 程序闪退。ETW 抓到的分配栈:
/// `win32u(分层窗口更新)→ DXGI/D3D11 设备路径 → nvwgf2umx(NVIDIA UMD)→ 内核`。
/// 现在尺寸不再变、窗口不再隐藏(显隐 = alpha),那条"重建表面"的路就永远不会被走到。
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

    // 边缘光:先做缓动(每帧朝目标走一小步),再决定要不要显示。
    // 这段必须在 visible 判断之前 —— 淡出过程中 visible 仍是 true,
    // 但亮度降到 0 之后要能自己收起来。
    let mut edge_done = false;
    if mode == Mode::Edge {
        let mut g = match state().lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let target = g.edge_target;
        let cur = g.edge_level;
        // 每 16ms 一拍(≈60fps)。系数 0.06 → 淡入约 350ms、淡出约 500ms。
        // 早先 40ms + 0.18,理论 200ms 就该亮完,但实测每帧要 247ms,
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
    // 「要不要重画」——
    //
    // 🔴 2026-10-31(AUDIT §5.1):原来是 `changed || mode == Orb || mode == Edge`。
    // 悬浮球的画面是 **`levels` 的纯函数**:圆底和环的几何全是常量,九根柱的高度
    // 只由最近九个音量值决定,没有任何时间项。而主循环每 ~32ms 才推一次 levels,
    // 按 60fps 无条件重画意味着一半以上的帧画出来**和上一帧逐像素相同**,
    // 白付一次 D2D 绘制 + 一次整屏 `UpdateLayeredWindow`。
    // 现在只在「levels 和上次真正画出去的那份不一样」时才画(见 `orb_levels_changed`)。
    //
    // 边缘光**保留逐帧**:它的呼吸(2200ms)和流动(8000ms)是连续动画,门控会看出卡顿。
    // 降帧率是另一条(有观感代价)的选项,不在本次改动里。
    let repaint = changed || mode == Mode::Edge || (mode == Mode::Orb && orb_levels_changed());

    // 这块窗口是整屏的,所以「画布坐标 == 屏幕坐标」;
    // 下面算出来的 (x,y,w,h) 是**内容在画布里的矩形**(不再是窗口的位置/尺寸)。
    let sw = GetSystemMetrics(0);
    let sh = GetSystemMetrics(1);
    if sw <= 0 || sh <= 0 {
        return;
    }
    let (w, h) = match mode {
        Mode::Bar => measure(&status, &text),
        Mode::Orb => orb_size(),
        // 边缘光铺满整个屏幕(物理像素)
        Mode::Edge => (sw, sh),
    };
    // 固定在屏幕**中下方**、任务栏上面一点点(用户明确要求)。
    // 不再跟随光标/鼠标 —— 跟随会在打字时来回跳,反而更干扰。
    // `at` 仍然保留:overlay-demo 用它把内容摆到指定坐标做预览。
    let (mut x, mut y) = match mode {
        // 边缘光永远贴 (0,0) 铺满,不走锚点
        Mode::Edge => (0, 0),
        _ => match at {
            Some((ax, ay)) => (ax, ay),
            None => anchor_pos(w, h),
        },
    };

    // 屏幕边界钳制
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
    let panel = Rect {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };

    // 窗口只在第一次摆一次(位置=(0,0)、尺寸=整屏),之后只推内容。
    if !WINDOW_PLACED.load(Ordering::Relaxed) {
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, sw, sh, SWP_NOACTIVATE);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        WINDOW_PLACED.store(true, Ordering::Relaxed);
    }

    if !visible {
        // 「隐藏」= 推一帧全透明(alpha 全 0 等于看不见,且 WS_EX_TRANSPARENT 不吃点击)。
        // 窗口本身留着 —— 不给系统"销毁/重建表面"的机会。
        if PAINTED.load(Ordering::Relaxed) {
            paint(hwnd, &panel, false);
            PAINTED.store(false, Ordering::Relaxed);
        }
        return;
    }
    // UpdateLayeredWindow 是"推"模型:每帧主动重画,不依赖 WM_PAINT
    if repaint {
        paint(hwnd, &panel, true);
        PAINTED.store(true, Ordering::Relaxed);
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
/// UpdateLayeredWindow 的做法:
///   1. 建一个 32bpp 的 DIBSection(自己的画布,不碰屏幕 DC)——**整屏一块,常驻复用**
///   2. 把 D2D 绑到这个 DIB 的 HDC 上画(绑定矩形恒定 = 整块画布,内容用平移定位)
///   3. UpdateLayeredWindow 把整块位图推给窗口(带 per-pixel alpha)
///
/// 把当前模式的内容画进**整屏画布**,再一次性推给窗口。
///
/// `panel`:内容在画布(= 屏幕)坐标系里的矩形。细条/悬浮球只占其中一小块,
/// 边缘光是整块 —— 但**画布和窗口的尺寸永远不变**。
/// `draw=false`:推一帧全透明,用来"隐藏"(窗口一直留着,只是看不见)。
///
/// ## 🔴 脏矩形只清该清的(2026-10-31,AUDIT §5.1)
///
/// 以前这里是 `ptr::write_bytes(bits, 0, sw*sh*4)`:4K 下 33MB,文档实测 **7ms/帧**,
/// 而且只要在录音就每 16ms 付一次 —— 相当于一个逻辑核在空转(另一个核还要跑 ASR)。
/// 但一帧里真正非透明的只有「上一帧画过的那几块」和「本帧要画的那几块」:
/// 边缘光是沿边的四条光带(4K 下 ≈ 6MB),细条/悬浮球是一小块(几十 KB)。
/// 所以改成只清 **上一帧区域 ∪ 本帧区域**:
///   · 上一帧区域必须清 —— 画布是复用的(DIB 不重建,见 `Canvas`),本帧画到别处时
///     旧位置不会自己消失,屏幕上就留鬼影;
///   · 本帧区域也要清 —— 那里可能残留着更早的帧(内容移动过、模式换过),
///     不能假设它本来就透明,否则残留会一帧帧攒下去。
/// 不变量只有一句:**画布上非透明的像素一定落在 `DIRTY` 里**。
///
/// 清的是「哪些」由 `content_rects` 给(和真正绘制用的是同一份几何),
/// 窗口表面那边不用管:每帧仍把**整张**位图推给 `UpdateLayeredWindow`,
/// 画布上透明的部分自然把它盖成透明。
///
/// ⚠️ 尺寸唯一真源 = 屏幕:以前这里跟着「细条/悬浮球/边缘光」算 (w,h),
/// 于是每句话都在整屏 ↔ 小面板之间重建位图、同时窗口也跟着改尺寸 ——
/// 那正是"一句一块 32 MiB"的来源(见 sync 的注释)。
unsafe fn paint(hwnd: isize, panel: &Rect, draw: bool) {
    let (mode, _status, text, levels) = match state().lock() {
        Ok(s) => (s.mode, s.status.clone(), s.text.clone(), s.levels.clone()),
        Err(_) => return,
    };
    let sw = GetSystemMetrics(0);
    let sh = GetSystemMetrics(1);
    if sw <= 0 || sh <= 0 {
        return;
    }
    let w = (panel.right - panel.left).max(0);
    let h = (panel.bottom - panel.top).max(0);
    if w <= 0 || h <= 0 {
        return;
    }
    // 绘制函数按「内容自己的坐标系」画(左上角 = 0,0),位置由 with_target 里的平移给出。
    let layout = Rect {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    let offset = (panel.left as f32, panel.top as f32);
    // 画布 = 整屏(窗口就铺在 (0,0)),所以画布坐标 == 屏幕坐标
    let canvas = Rect {
        left: 0,
        top: 0,
        right: sw,
        bottom: sh,
    };

    // 本帧内容会落到画布上的区域(画布坐标 = 屏幕坐标)。
    // `draw == false` 是「隐藏」那一帧:本帧什么都不画,于是「本帧区域」是空集,
    // 下面的并集正好退化成「只把上一帧的残留抹掉」——正是隐藏要的效果。
    let cur = if draw {
        content_rects(mode, panel, sw, sh)
    } else {
        [EMPTY_RECT; DIRTY_MAX]
    };

    // 画布:按**屏幕**尺寸缓存(整进程就这一块;屏幕变了才重建)。
    //
    // 这里只借一下 CANVAS 把 (mem_dc, bits) 拿出来就还回去 —— 后面的清零/绘制只用这两个值。
    // (画布本身归缓存管,拿裸指针出去不会被回收,Drop 只在尺寸变化或线程结束时跑。)
    let canvas_slot = CANVAS.with(|slot| unsafe {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().map(|c| (c.w, c.h)) != Some((sw, sh)) {
            // 先把 D2D 的绑定作废(它绑的是旧 DC),再丢旧画布
            RT.with(|rt| {
                if let Some(c) = rt.borrow_mut().as_mut() {
                    c.bound = None;
                }
            });
            *slot = Canvas::create(sw, sh);
            // 换了一块新内存:旧账本在新画布上毫无意义,直接作废
            // (否则下一帧会在新画布上空清一块从没画过的区域 —— 无害,但说明不清不楚)。
            // 顺手整块清一次:屏幕尺寸变化是极罕见事件,这一次全屏清零无所谓,
            // 换来的是不必去假设 CreateDIBSection 给的位图一定是零。
            if let Some(c) = slot.as_ref() {
                ptr::write_bytes(c.bits, 0, (sw * sh * 4) as usize);
            }
            DIRTY.with(|d| *d.borrow_mut() = [EMPTY_RECT; DIRTY_MAX]);
        }
        slot.as_ref().map(|c| (c.mem_dc, c.bits))
    });
    let Some((mem_dc, bits)) = canvas_slot else {
        return; // 画布造不出来:这一帧不画(账本原样留着,下一帧照上面的规则继续清)
    };

    // 把「上一帧区域」挪出来,顺手把「本帧区域」记进去 —— 下一帧要清的正是它。
    // 必须等确认这一帧画得成之后才动账本:提前记下而中途 return,账本就和画布对不上,
    // 那部分残留以后再也清不掉了。
    let prev = DIRTY.with(|d| std::mem::replace(&mut *d.borrow_mut(), cur));

    // 要抹成透明的区域 = **上一帧区域 ∪ 本帧区域**,合并进复用缓冲(稳态零分配)。
    CLEAR.with(|cs| {
        let mut buf = cs.borrow_mut();
        buf.clear();
        buf.extend(prev.iter().chain(cur.iter()).copied());
        let clear: &[Rect] = buf.as_slice();

        // 全透明打底 —— 只清这几块(以前是整屏 33MB,见函数开头的「脏矩形」)。
        for r in clear {
            clear_rect(bits, sw, sh, r);
        }

        if draw {
            match mode {
                Mode::Bar => d2d_paint_bar(mem_dc, &canvas, clear, &layout, offset, &text),
                Mode::Orb => d2d_paint_orb(mem_dc, &canvas, clear, &layout, offset, &levels),
                Mode::Edge => {
                    let lv = state().lock().map(|g| g.edge_level).unwrap_or(0.0);
                    d2d_paint_edge(mem_dc, &canvas, clear, &layout, offset, lv);
                }
            }
        }
    });

    // 推给窗口(带 alpha)。**尺寸恒等于整屏** → 系统不需要重建窗口表面。
    let mut pt_src = Point { x: 0, y: 0 };
    let mut sz = Size { cx: sw, cy: sh };
    let blend = BlendFunction {
        blend_op: 0, // AC_SRC_OVER
        blend_flags: 0,
        source_constant_alpha: 255,
        alpha_format: 1, // AC_SRC_ALPHA
    };
    let mut pt_dst = Point { x: 0, y: 0 }; // 窗口就摆在 (0,0),整屏
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

    // 画布不在这里删:它归 CANVAS 缓存管(屏幕尺寸变了或线程退出时才回收)。
}

/// 本帧内容会覆盖到的矩形(画布坐标 = 屏幕坐标)—— 脏矩形的「本帧区域」。
///
/// · 细条 / 悬浮球:整个内容面板。D2D 是把内容**平移**进这块矩形里画的
///   (见 `with_target` 的 offset),圆角矩形/圆/文字都严格落在它内部。
/// · 边缘光:四条光带。这里直接复用 `edge_geometry` 的 band(和 `EdgeBrushes`
///   拿去 PushLayer 当 contentBounds 的是同一组数),几何以后改了也不会两边对不上。
///
/// 每块都往外放 1px:抗锯齿和驱动取整理论上都不会越出几何边界,但万一越出去,
/// 后果是屏幕上留一条**永远清不掉**的亮线 —— 多清 1px 的成本可以忽略,不值当赌。
///
/// 定长 4(悬浮球/细条只用第 0 个),没用到的槽位是空矩形,由空判断统一过滤。
fn content_rects(mode: Mode, panel: &Rect, sw: i32, sh: i32) -> [Rect; DIRTY_MAX] {
    let mut out = [EMPTY_RECT; DIRTY_MAX];
    match mode {
        Mode::Bar | Mode::Orb => out[0] = pad1(panel),
        Mode::Edge => {
            let spread = px(edge_spread()) as f32;
            for (i, g) in edge_geometry(sw as f32, sh as f32, spread).iter().enumerate() {
                let (l, t, r, b) = g.4;
                out[i] = pad1(&Rect {
                    left: l as i32,
                    top: t as i32,
                    right: r as i32,
                    bottom: b as i32,
                });
            }
        }
    }
    out
}

/// 往外扩 1px(见 `content_rects` 的说明)。
fn pad1(r: &Rect) -> Rect {
    Rect {
        left: r.left - 1,
        top: r.top - 1,
        right: r.right + 1,
        bottom: r.bottom + 1,
    }
}

/// 把画布上的矩形 `r` 抹成全透明(按行 `write_bytes`,不是逐像素)。
///
/// `cw`/`ch` = 画布宽高。矩形先按画布裁一次:内容可以被屏幕边界钳过,
/// 屏幕也可能变小过,越界的行/列必须挡住(`write_bytes` 越界是踩内存,不是画错)。
unsafe fn clear_rect(bits: *mut u8, cw: i32, ch: i32, r: &Rect) {
    let l = r.left.clamp(0, cw);
    let t = r.top.clamp(0, ch);
    let rr = r.right.clamp(0, cw);
    let bt = r.bottom.clamp(0, ch);
    if rr <= l || bt <= t {
        return;
    }
    let row_bytes = ((rr - l) * 4) as usize;
    for y in t..bt {
        let off = (y as usize * cw as usize + l as usize) * 4;
        ptr::write_bytes(bits.add(off), 0, row_bytes);
    }
}

/// D2D 渲染目标,外加「当前绑到哪儿」——用来免得每帧都 BindDC。
struct RtCache {
    factory: windows::Win32::Graphics::Direct2D::ID2D1Factory,
    rt: windows::Win32::Graphics::Direct2D::ID2D1DCRenderTarget,
    target: windows::Win32::Graphics::Direct2D::ID2D1RenderTarget,
    /// 当前绑定:(hdc, left, top, right, bottom)。None = 还没绑/需要重绑。
    bound: Option<(isize, i32, i32, i32, i32)>,
}

/// 一帧画布:32bpp DIB + 装着它的内存 DC。
///
/// 🔴 为什么必须缓存(2026-10-30 实测):原来**每帧**都
/// `CreateCompatibleDC` + `CreateDIBSection(w*h*4)` + `DeleteObject`。
/// 边缘光模式下 w*h*4 就是整块屏幕(3840x2160 = 33 MB),60fps 下每帧
/// 都要系统给一块屏幕大小的内存再还回去;实测这一路上每次给的块在
/// 4K 下正好是 2^25 字节,而且**进程里会越攒越多**(一次听写一块,半天
/// 涨到 2 GB 以上,把整机提交额度吃光 —— 用户报的"pebrel 闪退 + 黑屏"
/// 就是它把 DWM 挤死的)。尺寸没变就没必要重建,留着复用。
struct Canvas {
    w: i32,
    h: i32,
    mem_dc: isize,
    dib: isize,
    bits: *mut u8,
    /// CreateCompatibleDC 自带的那张 1x1 位图,收尾时要换回去。
    old_bmp: isize,
}

impl Canvas {
    /// 造一块 w×h 的 32bpp 画布;任一步失败都自己清理干净并返回 None。
    unsafe fn create(w: i32, h: i32) -> Option<Canvas> {
        let bmi = BitmapInfo {
            header: BitmapInfoHeader {
                bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                bi_width: w,
                bi_height: -h, // 负 = 自上而下
                bi_planes: 1,
                bi_bit_count: 32,
                bi_compression: 0, // BI_RGB
                bi_size_image: (w * h * 4) as u32,
                ..Default::default()
            },
            colors: [0; 3],
        };
        let mem_dc = CreateCompatibleDC(0);
        if mem_dc == 0 {
            return None;
        }
        let mut bits: *mut u8 = ptr::null_mut();
        let dib = CreateDIBSection(mem_dc, &bmi, 0, &mut bits, 0, 0);
        if dib == 0 || bits.is_null() {
            DeleteDC(mem_dc);
            return None;
        }
        let old_bmp = SelectObject(mem_dc, dib);
        Some(Canvas { w, h, mem_dc, dib, bits, old_bmp })
    }

    /// 放掉画布。**绝不能**在 D2D 还绑着这个 DC 的时候调用 ——
    /// 调用的地方(尺寸变化/线程退出)都先丢了 RT 缓存。
    unsafe fn free(&self) {
        SelectObject(self.mem_dc, self.old_bmp);
        DeleteObject(self.dib);
        DeleteDC(self.mem_dc);
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe { self.free() };
    }
}

/// 一帧最多几块脏矩形:细条/悬浮球是 1 块,边缘光是 4 条光带。
const DIRTY_MAX: usize = 4;
/// 空矩形(`left == right`)—— 数组里没用到的槽位都用它,统一由空判断过滤掉,
/// 这样「本帧不画任何东西」不用换一种数据结构来表达。
const EMPTY_RECT: Rect = Rect {
    left: 0,
    top: 0,
    right: 0,
    bottom: 0,
};

thread_local! {
    /// D2D 渲染目标缓存(窗口线程专用;COM 对象不是 Send)。
    ///
    /// `bound` 记住**当前绑的是哪个 DC、哪个矩形**:DC 和矩形都没变就不必
    /// 再 `BindDC`(画布缓存之后 DC 每帧都一样了)。Direct2D 每次 BindDC
    /// 都会为绑上的 DC 重新准备一份内部表面,能省就省。
    static RT: std::cell::RefCell<Option<RtCache>> = const { std::cell::RefCell::new(None) };

    /// 每帧画布缓存(按尺寸复用)。
    static CANVAS: std::cell::RefCell<Option<Canvas>> = const { std::cell::RefCell::new(None) };

    /// 🔴 脏矩形的账本:画布上**当前还残留着非透明内容**的矩形(画布坐标 = 屏幕坐标)。
    ///
    /// 为什么会需要这本账:画布是**复用**的(DIB 不重建,见 `Canvas` 的注释),
    /// 上一帧画出去的东西不会自己消失 —— 本帧若不在同一位置重画,屏幕上就留鬼影。
    /// 完整的推导见 `paint` 里「清哪些」那段注释。
    static DIRTY: std::cell::RefCell<[Rect; DIRTY_MAX]> =
        const { std::cell::RefCell::new([EMPTY_RECT; DIRTY_MAX]) };

    /// 「上一帧区域 ∪ 本帧区域」的复用缓冲 —— 免得每帧为这几个矩形分配一次 Vec(Vec 稳态不扩容)。
    static CLEAR: std::cell::RefCell<Vec<Rect>> = const { std::cell::RefCell::new(Vec::new()) };

    /// 上一帧**真正画到屏幕上**的悬浮球波形 —— 只在 Orb 模式下用(见 `orb_levels_changed`)。
    static LAST_ORB_LEVELS: std::cell::RefCell<Option<Vec<f32>>> =
        const { std::cell::RefCell::new(None) };

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

/// 建一个绑到 DIB 上的 D2D render target,跑 `f`,收尾。
///
/// 抽出来是因为细条和悬浮球用的是**同一套** D2D 初始化 ——
/// 复制两遍的话,以后改 alpha 模式/像素格式要改两处,容易漏。
///
/// `clear`:本次要抹成透明的矩形(画布坐标)= 上一帧 ∪ 本帧的脏矩形,见 `paint`。
fn with_target(
    hdc: isize,
    canvas: &Rect,
    clear: &[Rect],
    offset: (f32, f32),
    f: impl FnOnce(&d2d_target::Target),
) {
    use windows::core::Interface;
    use windows::Foundation::Numerics::Matrix3x2;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Direct2D as d2d;
    use windows::Win32::Graphics::Direct2D::Common as d2dc;
    use windows::Win32::Graphics::Gdi::HDC;

    // 绑定矩形 = **整块画布**(恒定,由 paint 传进来)。
    //
    // 🔴 为什么绑整块画布、而不是绑内容所在的那块小矩形:绑定矩形一变,
    // Direct2D/驱动就要为新的绑定重新准备内部表面(4K 下 33 MB 一份),而且旧的
    // 不会回收 —— 实测一次听写漏一块。绑死整块画布、用平移把内容挪过去,绑定从不变。

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
    // 「这一帧的目标里那份内部位图是新的、内容不可知」——要整块清一次(见下面的打底)。
    // 两种情况都算:一种是刚建的 target,另一种是这一帧重新 BindDC 了(换了 DC 或换了绑定矩形,
    // 对 D2D 来说内部位图同样作废)。
    let mut fresh = slot.is_none();
    // 这一帧要绑的 DC + 矩形:和上次一样就不必再 BindDC
    // (画布缓存之后 DC 每帧都相同,所以这句大多数帧都成立)。
    let want = (hdc, canvas.left, canvas.top, canvas.right, canvas.bottom);
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
            left: canvas.left,
            top: canvas.top,
            right: canvas.right,
            bottom: canvas.bottom,
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
        *slot = Some(RtCache { factory, rt, target, bound: Some(want) });
    }

    // 只有「绑的东西变了」才重新 BindDC —— DC 和矩形都没变就不动它。
    let need_bind = slot.as_ref().and_then(|c| c.bound) != Some(want);
    // 重新绑过之后,内部位图对这块 DC 来说是新的(内容和旧绑定无关)—— 按"新 target"处理。
    fresh |= need_bind;
    if need_bind {
        let sub = RECT {
            left: canvas.left,
            top: canvas.top,
            right: canvas.right,
            bottom: canvas.bottom,
        };
        if let Err(e) = slot.as_ref().unwrap().rt.BindDC(HDC(hdc), &sub) {
            log_d2d_err("BindDC", &e);
            dead = true;
        }
    }

    if !dead {
        let c = slot.as_ref().unwrap();
        // 🔴 设备丢失的唯一出口。
        //
        // GPU 驱动报错 / 睡眠唤醒后,Direct2D 设备会被重置:旧 target 的
        // EndDraw 返回 D2DERR_RECREATE_TARGET(BindDC 也可能直接失败)。
        // 以前这里只打印一行、继续用同一个死 target —— 之后每一帧都画进
        // 不存在的设备,DIB 保持全透明,用户看到的就是「彩虹边缘光突然
        // 消失,但语音输入一切正常」,只能重启进程。
        // 现在只要出错就丢弃缓存,下一帧用新 target 重建,最多丢一帧。
        c.target.BeginDraw();
        // 打底:把 `clear` 给的那几块抹成全透明 —— 就是「上一帧 ∪ 本帧」的脏矩形(见 paint)。
        //
        // 🔴 为什么不能像以前那样无条件清**整块**画布:4K 下 33MB/帧,是本次要省的主角。
        // 裁剪(PushAxisAlignedClip)压在**设平移之前**,所以这里给的是画布坐标;
        // 空矩形要跳过(D2D 对退化矩形会直接失败)。
        //
        // 刚建出来的 target 例外:它内部那份位图是新的,内容不可知,而它每帧都会
        // (在 EndDraw 时)整块贴回 DC —— 只要里面还留着"不可知"的像素,就会一直
        // 盖在我们的账本上。所以建新 target 的这一帧整块清一次,把
        // 「内部位图 = 全透明」这条前提立起来;之后每帧只清脏矩形就是完备的
        // (归纳:上一帧的非透明区域一定在 prev 里,而 prev 每帧都被清)。
        if fresh {
            c.target.Clear(Some(&color(0, 0, 0, 0.0)));
        } else {
            for r in clear {
                if r.left >= r.right || r.top >= r.bottom {
                    continue;
                }
                let clip = d2dc::D2D_RECT_F {
                    left: r.left as f32,
                    top: r.top as f32,
                    right: r.right as f32,
                    bottom: r.bottom as f32,
                };
                c.target
                    .PushAxisAlignedClip(&clip, d2d::D2D1_ANTIALIAS_MODE_ALIASED);
                c.target.Clear(Some(&color(0, 0, 0, 0.0)));
                c.target.PopAxisAlignedClip();
            }
        }
        // 内容整体平移到它在画布里的位置(细条/悬浮球不贴左上角)。
        // 平移而不是"把绑定矩形挪过去" —— 见函数开头的注释。
        c.target.SetTransform(&Matrix3x2::translation(offset.0, offset.1));
        f(&d2d_target::Target { target: &c.target });
        if let Err(e) = c.target.EndDraw(None, None) {
            log_d2d_err("EndDraw", &e);
            dead = true;
        } else {
            slot.as_mut().unwrap().bound = Some(want);
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
fn d2d_paint_bar(hdc: isize, canvas: &Rect, clear: &[Rect], rc: &Rect, offset: (f32, f32), text: &str) {
    use windows::Win32::Graphics::Direct2D as d2d;
    use windows::Win32::Graphics::Direct2D::Common as d2dc;

    with_target(hdc, canvas, clear, offset, |t| unsafe {
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

/// 悬浮球:圆形底 + 琥珀细环 + 实时波形。
///
/// ⚠️ 这里**没有任何时间项**:环是固定几何的(没有呼吸动画),九根柱的高度只由
/// `levels` 决定 —— 所以「levels 没变就不用重画」这条门控是完备的(见 `orb_levels_changed`)。
/// 以后真给球加了呼吸/旋转之类的逐帧动画,**必须**同时改那条门控,否则动画会停住。
///
/// 波形是**真实音量**驱动的（见 `Overlay::push_level`），不是装饰动画 ——
/// 用户说话时才会起伏，安静时是一条平线。
fn d2d_paint_orb(hdc: isize, canvas: &Rect, clear: &[Rect], rc: &Rect, offset: (f32, f32), levels: &[f32]) {
    use windows::Win32::Graphics::Direct2D as d2d;
    use windows::Win32::Graphics::Direct2D::Common as d2dc;

    with_target(hdc, canvas, clear, offset, |t| unsafe {
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

thread_local! {
    /// 边缘光的笔刷/图层缓存(窗口线程专用;COM 对象不是 Send)。
    static EDGE_BRUSHES: std::cell::RefCell<Option<EdgeBrushes>> =
        const { std::cell::RefCell::new(None) };
}

/// 把 `windows` crate 的 D2D 类型收一收,免得字段写成天书。
mod d2d_brush {
    pub use windows::Win32::Graphics::Direct2D::{
        ID2D1Layer as Layer, ID2D1LinearGradientBrush as LinearBrush,
    };
}

/// 边缘光的 D2D 对象集合:**整个进程只建一次**(画布尺寸或色调变了才重建)。
///
/// 🔴 为什么必须缓存(2026-10-30 实测,别改回去):`d2d_paint_edge` 原来**每帧**都
/// `CreateGradientStopCollection` + `CreateLinearGradientBrush`(4 条边 + 4 个蒙版),
/// 外加 `CreateLayer`。渐变刷/图层要占 **D2D+驱动侧的设备资源**,而设备侧资源池是
/// **按 32 MiB 段增长、只增不减**的 —— 实测每说一句话就长一段(4K 下正好 2^25 字节),
/// 半天 2 GB+,最后把整机提交额度吃光 → DWM 被 `STATUS_FATAL_MEMORY_EXHAUSTION`
/// 打死 → 全屏黑 + 程序闪退。受控实验口径(100 秒 / 10-12 句,同一进程内复现):
///   · 只画纯色矩形(无渐变、无图层)      → 0 块
///   · 完整绘制但跳过 PushLayer           → 每句一块
///   · 完整绘制但用纯色刷顶替色彩刷        → 每句一块
/// ⇒ 元凶是**每帧新建渐变刷**;图层与窗口尺寸都被证伪。
/// 现在:对象只建一次 —— **流动** = 平移色带刷的渐变几何,**亮度** = `SetOpacity`,
/// 于是每帧零资源创建。
struct EdgeBrushes {
    w: i32,
    h: i32,
    tint: u8,
    /// 四条边(上/右/下/左)。数组顺序与 `edge_geometry()` 一致。
    color: [d2d_brush::LinearBrush; 4],
    fade: [d2d_brush::LinearBrush; 4],
    layer: d2d_brush::Layer,
    /// 色带刷的几何基线(已按 ±1 个周长延长),每帧在此基础上平移。
    start: [(f32, f32); 4],
    end: [(f32, f32); 4],
    /// t 增大的方向(单位向量);平移量 = flow * per * dir。
    dir: [(f32, f32); 4],
    /// 每条边的光带矩形。
    band: [(f32, f32, f32, f32); 4],
}

/// 四条边的几何(与绘制顺序一致):起点、终点、t 区间、光带矩形、向内渐变起止、方向。
#[allow(clippy::type_complexity)]
fn edge_geometry(w: f32, h: f32, spread: f32) -> [((f32, f32), (f32, f32), f32, f32, (f32, f32, f32, f32), (f32, f32), (f32, f32), (f32, f32)); 4] {
    let per = 2.0 * (w + h);
    let tw = w / per;
    let th = h / per;
    [
        // 上(左→右)
        ((0.0, 0.0), (w, 0.0), 0.0, tw,
         (0.0, 0.0, w, spread), (0.0, 0.0), (0.0, spread), (1.0, 0.0)),
        // 右(上→下)
        ((w, 0.0), (w, h), tw, tw + th,
         (w - spread, 0.0, w, h), (w, 0.0), (w - spread, 0.0), (0.0, 1.0)),
        // 下(右→左)
        ((w, h), (0.0, h), tw + th, tw + th + tw,
         (0.0, h - spread, w, h), (0.0, h), (0.0, h - spread), (-1.0, 0.0)),
        // 左(下→上)
        ((0.0, h), (0.0, 0.0), tw + th + tw, 1.0,
         (0.0, 0.0, spread, h), (0.0, 0.0), (spread, 0.0), (0.0, -1.0)),
    ]
}

impl EdgeBrushes {
    /// 建一整套。任何一步失败返回 None(悬浮层降级成"不画",绝不影响主功能)。
    unsafe fn create(
        target: &windows::Win32::Graphics::Direct2D::ID2D1RenderTarget,
        w: f32,
        h: f32,
        tint: EdgeTint,
    ) -> Option<EdgeBrushes> {
        use windows::Win32::Graphics::Direct2D as d2d;
        use windows::Win32::Graphics::Direct2D::Common as d2dc;

        let per = 2.0 * (w + h);
        let spread = px(edge_spread()) as f32;

        // 蒙版色标:边缘不透明 → 向内透明。**alpha 烘焙成 1**,
        // 整体亮度每帧用 SetOpacity 给 —— 这样这条色标集合是常量,只建一次。
        let fade_stops = [
            d2dc::D2D1_GRADIENT_STOP {
                position: 0.0,
                color: d2dc::D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
            },
            d2dc::D2D1_GRADIENT_STOP {
                position: 1.0,
                color: d2dc::D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.0 },
            },
        ];
        let fade_coll = target
            .CreateGradientStopCollection(
                &fade_stops,
                d2d::D2D1_GAMMA_2_2,
                d2d::D2D1_EXTEND_MODE_CLAMP,
            )
            .ok()?;

        let mut colors = Vec::with_capacity(4);
        let mut fades = Vec::with_capacity(4);
        let mut starts = [(0.0f32, 0.0f32); 4];
        let mut ends = [(0.0f32, 0.0f32); 4];
        let mut dirs = [(0.0f32, 0.0f32); 4];
        let mut bands = [(0.0f32, 0.0f32, 0.0f32, 0.0f32); 4];

        for (i, g) in edge_geometry(w, h, spread).iter().enumerate() {
            let (s, e, t0, t1, band, f0, f1, dir) = *g;

            // 色带刷:色标覆盖 t ∈ [t0-1, t1+1](±1 个周长)—— 平移几何时颜色不会滑出边界。
            // flow 烘焙成 0,流动完全交给几何平移。
            const N: usize = 3 * 17;
            let span = (t1 - t0) + 2.0;
            let mut stops: [d2dc::D2D1_GRADIENT_STOP; N] = [d2dc::D2D1_GRADIENT_STOP {
                position: 0.0,
                color: d2dc::D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 },
            }; N];
            for (k, slot) in stops.iter_mut().enumerate() {
                let u = k as f32 / (N - 1) as f32;
                let (r, gg, b) = edge_color_at(tint, (t0 - 1.0) + span * u, 0.0);
                slot.position = u;
                slot.color = d2dc::D2D1_COLOR_F { r, g: gg, b, a: 1.0 };
            }
            let ccoll = target
                .CreateGradientStopCollection(
                    &stops,
                    d2d::D2D1_GAMMA_2_2,
                    d2d::D2D1_EXTEND_MODE_CLAMP,
                )
                .ok()?;

            // 几何按 ±1 个周长延长:1 个 t 单位 = per 像素(与边长的比例关系对四条边都成立)
            let sx = s.0 - dir.0 * per;
            let sy = s.1 - dir.1 * per;
            let ex = e.0 + dir.0 * per;
            let ey = e.1 + dir.1 * per;
            let color = target
                .CreateLinearGradientBrush(
                    &d2d::D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                        startPoint: d2dc::D2D_POINT_2F { x: sx, y: sy },
                        endPoint: d2dc::D2D_POINT_2F { x: ex, y: ey },
                    },
                    None,
                    &ccoll,
                )
                .ok()?;

            // 蒙版刷:几何固定(边缘 → 向内)
            let fade = target
                .CreateLinearGradientBrush(
                    &d2d::D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                        startPoint: d2dc::D2D_POINT_2F { x: f0.0, y: f0.1 },
                        endPoint: d2dc::D2D_POINT_2F { x: f1.0, y: f1.1 },
                    },
                    None,
                    &fade_coll,
                )
                .ok()?;

            colors.push(color);
            fades.push(fade);
            starts[i] = (sx, sy);
            ends[i] = (ex, ey);
            dirs[i] = dir;
            bands[i] = band;
        }

        let layer = target.CreateLayer(None).ok()?;
        Some(EdgeBrushes {
            w: w as i32,
            h: h as i32,
            tint: tint_code(tint),
            color: colors.try_into().ok()?,
            fade: fades.try_into().ok()?,
            layer,
            start: starts,
            end: ends,
            dir: dirs,
            band: bands,
        })
    }
}

/// 屏幕边缘光:四条边各画一道从边缘向内的渐变。
///
/// ## 怎么做出「流动的彩色」
///
/// 难点:颜色要**沿边缘**变化(彩虹),亮度要**向内**衰减(光带)——
/// 这两个方向互相垂直,一个线性渐变刷做不到。
///
/// 做法是 **PushLayer + opacityBrush**(蒙版):先用「边缘不透明 → 向内全透明」的渐变
/// 压入图层,再在图层里用**沿边缘的色带刷**填满整块矩形,最后 PopLayer。
///
/// ## 每帧只改属性,不建对象(见 EdgeBrushes 的注释)
///
/// · 流动 = 把色带刷的几何沿边平移(`SetStartPoint`/`SetEndPoint`)
/// · 亮度 = 蒙版刷的 `SetOpacity`
/// · 颜色变化 = 色带刷的色标只在**建一次**时烘焙(flow=0),之后靠平移
fn d2d_paint_edge(hdc: isize, canvas: &Rect, clear: &[Rect], rc: &Rect, offset: (f32, f32), level: f32) {
    use windows::core::Interface;
    use windows::Win32::Graphics::Direct2D::Common as d2dc;

    if level <= 0.001 {
        return;
    }

    let tint = edge_tint();

    with_target(hdc, canvas, clear, offset, |t| unsafe {
        let target = t.target;
        let w = (rc.right - rc.left) as f32;
        let h = (rc.bottom - rc.top) as f32;
        let spread = px(edge_spread()) as f32;

        // 呼吸:0.86~1.0 之间缓慢起伏。相位都在 0..1,见 edge_clock。
        let (breath_ph, flow_ph) = edge_clock();
        let breath = 0.86 + 0.14 * (breath_ph * std::f32::consts::TAU).sin();

        // 饱和色比白色"显眼"得多,所以不用提 alpha 就能看清 —— 反而要略降。
        let tint_boost = match tint {
            EdgeTint::White => 1.0,
            _ => 0.92,
        };
        let peak = edge_peak() * level * breath * tint_boost;
        if peak <= 0.002 {
            return;
        }

        EDGE_BRUSHES.with(|slot| {
            let mut slot = slot.borrow_mut();
            let stale = match slot.as_ref() {
                Some(b) => b.w != w as i32 || b.h != h as i32 || b.tint != tint_code(tint),
                None => true,
            };
            if stale {
                *slot = EdgeBrushes::create(target, w, h, tint);
            }
            let Some(b) = slot.as_ref() else {
                return; // 建不出来:这一帧不画边缘光(降级,不影响主功能)
            };

            let per = 2.0 * (w + h);
            let off = flow_ph * per; // 流动量(像素)
            for i in 0..4 {
                let (dx, dy) = b.dir[i];
                b.color[i].SetStartPoint(d2dc::D2D_POINT_2F {
                    x: b.start[i].0 - dx * off,
                    y: b.start[i].1 - dy * off,
                });
                b.color[i].SetEndPoint(d2dc::D2D_POINT_2F {
                    x: b.end[i].0 - dx * off,
                    y: b.end[i].1 - dy * off,
                });
                b.fade[i].SetOpacity(peak);
            }

            for i in 0..4 {
                let (l, tp, r, bt) = b.band[i];
                let rect = d2dc::D2D_RECT_F { left: l, top: tp, right: r, bottom: bt };
                let params = windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS {
                    contentBounds: rect,
                    geometricMask: std::mem::ManuallyDrop::new(None),
                    maskAntialiasMode:
                        windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: windows::Foundation::Numerics::Matrix3x2::identity(),
                    opacity: 1.0,
                    // ⚠️ 这里是**借用一个裸指针**,不是交出所有权:
                    // `opacityBrush` 在 C 结构里只是个指针,D2D 不会接管引用。
                    // 老代码写的是 `fbrush.clone().into()` —— clone 加的引用**永远没人 Release**
                    // (每边每帧一个),这正是「每说一句长一段 32 MiB 设备侧资源」的两个来源之一
                    // (另一个是每帧新建渐变刷,见 EdgeBrushes 的注释)。
                    // 这里用 from_raw 借一下:不加引用,ManuallyDrop 也不会去释放它。
                    opacityBrush: std::mem::ManuallyDrop::new(Some(unsafe {
                        windows::Win32::Graphics::Direct2D::ID2D1Brush::from_raw(b.fade[i].as_raw())
                    })),
                    layerOptions: windows::Win32::Graphics::Direct2D::D2D1_LAYER_OPTIONS_NONE,
                };
                // 图层对象复用:每帧只 Push/Pop,不再 CreateLayer
                target.PushLayer(&params, &b.layer);
                target.FillRectangle(&rect, &b.color[i]);
                target.PopLayer();
            }
        });
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
            with_target(mem_dc, &rc, std::slice::from_ref(&rc), (0.0, 0.0), |_| drew = true);
            assert!(drew, "健康 target 应执行绘制");
            assert!(
                RT.with(|s| s.borrow().is_some()),
                "健康绘制后应留下缓存"
            );

            // 失败替身：先建再删，留下的句柄保证 BindDC 失败
            let stale = CreateCompatibleDC(0);
            assert!(stale != 0);
            DeleteDC(stale);
            with_target(stale, &rc, std::slice::from_ref(&rc), (0.0, 0.0), |_| {});
            assert!(
                RT.with(|s| s.borrow().is_none()),
                "BindDC 失败后必须丢弃缓存（旧 bug：继续复用死 target）"
            );

            // 下一帧用健康 DC：必须重建并继续画
            let mut drew_again = false;
            with_target(mem_dc, &rc, std::slice::from_ref(&rc), (0.0, 0.0), |_| drew_again = true);
            assert!(drew_again, "缓存重建后应继续绘制");
            assert!(RT.with(|s| s.borrow().is_some()));

            // 先清缓存再收尾,别让 target 绑着已删除的 DC
            RT.with(|s| *s.borrow_mut() = None);
            SelectObject(mem_dc, old_bmp);
            DeleteObject(dib);
            DeleteDC(mem_dc);
        }
    }

    /// 回归:`clear_rect` 只抹给定矩形 —— 脏矩形的正确性全押在它身上。
    ///
    /// 关键的一条是**裁剪**:矩形可能被屏幕边界钳成负坐标或超出画布,
    /// 越界写不是"画错"而是踩内存。
    #[test]
    fn clear_rect_zeroes_only_that_rect() {
        let (w, h) = (8i32, 4i32);
        let mut buf = vec![0xFFu8; (w * h * 4) as usize];
        unsafe {
            clear_rect(
                buf.as_mut_ptr(),
                w,
                h,
                &Rect { left: -2, top: 1, right: 3, bottom: 3 },
            );
        }
        for y in 0..h {
            for x in 0..w {
                let a = buf[((y * w + x) * 4 + 3) as usize];
                let inside = (1..3).contains(&y) && (0..3).contains(&x);
                assert_eq!(a == 0, inside, "({x},{y}) 该清/不该清判断错了");
            }
        }
        // 空矩形(账本里的"没用到的槽位")必须什么都不做
        let mut buf2 = vec![0xFFu8; (w * h * 4) as usize];
        unsafe { clear_rect(buf2.as_mut_ptr(), w, h, &EMPTY_RECT) };
        assert!(buf2.iter().all(|b| *b == 0xFF));
    }

    /// 回归:`content_rects` 必须把内容**圈得住**(圈不住 = 屏幕上留残留)。
    #[test]
    fn content_rects_covers_drawn_area() {
        let (sw, sh) = (1920, 1080);
        // 细条/悬浮球:内容面板本身
        let panel = Rect { left: 100, top: 200, right: 400, bottom: 260 };
        let bar = content_rects(Mode::Bar, &panel, sw, sh);
        assert!(
            bar[0].left <= panel.left
                && bar[0].top <= panel.top
                && bar[0].right >= panel.right
                && bar[0].bottom >= panel.bottom
        );
        assert!(
            bar[1..].iter().all(|r| r.left >= r.right),
            "没用到的槽位应该是空矩形"
        );
        let orb = content_rects(Mode::Orb, &panel, sw, sh);
        assert_eq!(
            (orb[0].left, orb[0].top, orb[0].right, orb[0].bottom),
            (bar[0].left, bar[0].top, bar[0].right, bar[0].bottom)
        );

        // 边缘光:四条光带(顺序同 edge_geometry:上/右/下/左),贴边且向内延伸
        let e = content_rects(Mode::Edge, &Rect::default(), sw, sh);
        assert!(e[0].left <= 0 && e[0].top <= 0 && e[0].right >= sw && e[0].bottom > 0);
        assert!(e[1].left > 0 && e[1].left < sw && e[1].right >= sw && e[1].bottom >= sh);
        assert!(e[2].left <= 0 && e[2].right >= sw && e[2].top > 0 && e[2].bottom >= sh);
        assert!(e[3].left <= 0 && e[3].right > 0 && e[3].top <= 0 && e[3].bottom >= sh);
    }

    /// 🔴 实测:被裁剪的 `Clear` 会不会动到画布上**没被清到**的地方。
    ///
    /// 这条决定了「脏矩形清零」能不能做。`ID2D1DCRenderTarget` 的官方说明是
    /// 「先画进一张内部位图,再用 GDI 贴回 DC」,但**没说贴的范围**:
    /// 万一它每帧把整张内部位图原样贴回来,那"只清脏矩形"就是错的 ——
    /// 内部位图里上一帧的旧内容会被贴回来,在屏幕上留鬼影,而且会盖掉我们
    /// 按账本刚擦干净的区域(那样只能用"整块清"或改成累计区域,方案完全不同)。
    ///
    /// 做法:第 1 帧在 A 画东西(此后 D2D 内部 A 处有内容);按真实流程把 A
    /// **自己擦干净**(模拟账本里"清上一帧区域"这一步);第 2 帧只在 B 画、
    /// 且只让 D2D 清 B —— 然后回头看 A 是不是又被贴上了内容。
    ///
    /// 2026-10-31 本机(Direct2D 软件路径)结果:A 保持全透明 ⇒ 没被清到的地方
    /// 不会被 D2D 改写,裁剪清零成立(AUDIT §5.1 的前提)。
    #[test]
    fn clipped_clear_keeps_untouched_area() {
        use windows::Win32::Graphics::Direct2D::Common as d2dc;
        unsafe {
            let n = 64;
            let mem_dc = CreateCompatibleDC(0);
            assert!(mem_dc != 0, "CreateCompatibleDC failed");
            let bmi = BitmapInfo {
                header: BitmapInfoHeader {
                    bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                    bi_width: n,
                    bi_height: -n,
                    bi_planes: 1,
                    bi_bit_count: 32,
                    bi_compression: 0,
                    bi_size_image: (n * n * 4) as u32,
                    ..Default::default()
                },
                colors: [0; 3],
            };
            let mut bits: *mut u8 = ptr::null_mut();
            let dib = CreateDIBSection(mem_dc, &bmi, 0, &mut bits, 0, 0);
            assert!(dib != 0 && !bits.is_null(), "CreateDIBSection failed");
            let old_bmp = SelectObject(mem_dc, dib);
            ptr::write_bytes(bits, 0, (n * n * 4) as usize);

            let canvas = Rect {
                left: 0,
                top: 0,
                right: n,
                bottom: n,
            };
            let a = Rect { left: 8, top: 8, right: 24, bottom: 24 };
            let b = Rect { left: 40, top: 40, right: 56, bottom: 56 };
            // 画布上 (x,y) 处的 4 个字节(BGRA),第 4 个是 alpha
            let at = |x: i32, y: i32| -> [u8; 4] {
                let mut out = [0u8; 4];
                for (k, slot) in out.iter_mut().enumerate() {
                    *slot = *bits.add((y as usize * n as usize + x as usize) * 4 + k);
                }
                out
            };
            let fill = |r: &Rect, color_word: u32| {
                with_target(mem_dc, &canvas, std::slice::from_ref(r), (0.0, 0.0), |t| {
                    if let Ok(br) =
                        t.target.CreateSolidColorBrush(&argb(color_word) as *const _, None)
                    {
                        t.target.FillRectangle(
                            &d2dc::D2D_RECT_F {
                                left: r.left as f32,
                                top: r.top as f32,
                                right: r.right as f32,
                                bottom: r.bottom as f32,
                            },
                            &br,
                        );
                    }
                });
            };

            // 第 1 帧:清整块(把 A 写进 D2D 的内部位图),在 A 里画一块不透明的红
            fill(&a, 0xFF_FF0000);
            assert!(at(16, 16)[3] > 0, "第 1 帧后 A 应该有内容,否则这条测试没有意义");

            // 真实流程里「账本把上一帧区域擦掉」这一步
            for y in a.top..a.bottom {
                ptr::write_bytes(
                    bits.add((y as usize * n as usize + a.left as usize) * 4),
                    0,
                    ((a.right - a.left) * 4) as usize,
                );
            }
            assert_eq!(at(16, 16)[3], 0, "A 应该已被账本擦干净");

            // 第 2 帧:只让 D2D 清 B,并在 B 里画东西(保证这一帧确实有绘制、有回贴)
            fill(&b, 0xFF_00FF00);
            assert_eq!(
                at(16, 16)[3], 0,
                "A 又被贴上了内容 —— D2D 把整张内部位图回贴了,脏矩形清零不成立"
            );
            assert_eq!(at(48, 48), [0, 255, 0, 255], "B 里应该画上了不透明的绿");

            RT.with(|s| *s.borrow_mut() = None);
            SelectObject(mem_dc, old_bmp);
            DeleteObject(dib);
            DeleteDC(mem_dc);
        }
    }
}
