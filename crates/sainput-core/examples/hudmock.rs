// HUD 设计原型 —— 同时画四种方案，供用户挑选。
//
// 为什么用文字路径：实测这个 layered 窗口的 Window DC 上
// FillRect / RoundRect / Rectangle / TextOut 全部不落笔，
// 只有 ExtTextOutW（文字路径）能画。所以所有视觉元素都必须是**字符**。
//
// 可用字符：█ ▉▊▋▌▍▎▏ ▀▄ ▁▂▃▄▅▆▇ ● ○ ◉ ╭╮╰╯ │─ ┌┐└┘ ═ ║ → ← ↑ ↓ ▸
//
// 用法：cargo run --release --example hudmock

use std::ptr;

const WS_POPUP: u32 = 0x8000_0000;
const WS_EX_TOPMOST: u32 = 0x0000_0008;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_LAYERED: u32 = 0x0008_0000;
const LWA_ALPHA: u32 = 0x0000_0002;
const WM_PAINT: u32 = 0x000F;
const WM_DESTROY: u32 = 0x0002;
const WM_TIMER: u32 = 0x0113;
const WM_MOUSEACTIVATE: u32 = 0x0021;
const MA_NOACTIVATE: isize = 3;
const WM_LBUTTONUP: u32 = 0x0202;

#[repr(C)]
struct Rect { left: i32, top: i32, right: i32, bottom: i32 }
#[repr(C)]
struct PaintStruct { hdc: isize, erase: i32, rc: Rect, restore: i32, inc: i32, reserved: [u8; 32] }
#[repr(C)]
struct Msg { hwnd: isize, message: u32, w: usize, l: isize, time: u32, x: i32, y: i32 }
type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;
#[repr(C)]
struct WndClassW {
    style: u32, lpfn: Option<WndProc>, cb_cls: i32, cb_wnd: i32,
    h_inst: isize, h_icon: isize, h_cursor: isize, h_br: isize,
    menu: *const u16, class: *const u16,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(c: *const WndClassW) -> u16;
    fn CreateWindowExW(ex: u32, cls: *const u16, t: *const u16, st: u32,
        x: i32, y: i32, w: i32, h: i32, p: isize, m: isize, i: isize, q: isize) -> isize;
    fn DefWindowProcW(h: isize, m: u32, w: usize, l: isize) -> isize;
    fn GetMessageW(m: *mut Msg, h: isize, a: u32, b: u32) -> i32;
    fn TranslateMessage(m: *const Msg) -> i32;
    fn DispatchMessageW(m: *const Msg) -> isize;
    fn BeginPaint(h: isize, ps: *mut PaintStruct) -> isize;
    fn EndPaint(h: isize, ps: *const PaintStruct) -> i32;
    fn GetClientRect(h: isize, r: *mut Rect) -> i32;
    fn SetLayeredWindowAttributes(h: isize, k: u32, a: u8, f: u32) -> i32;
    fn SetTimer(h: isize, i: usize, ms: u32, p: isize) -> usize;
    fn PostQuitMessage(c: i32);
    fn GetModuleHandleW(n: *const u16) -> isize;
    fn InvalidateRect(h: isize, r: *const Rect, e: i32) -> i32;
    fn DestroyWindow(h: isize) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateFontW(h: i32, w: i32, e: i32, o: i32, wt: i32, it: u32, un: u32, st: u32,
        cs: u32, op: u32, cp: u32, q: u32, pf: u32, face: *const u16) -> isize;
    fn SelectObject(hdc: isize, o: isize) -> isize;
    fn DeleteObject(o: isize) -> i32;
    fn SetTextColor(hdc: isize, c: u32) -> u32;
    fn SetBkColor(hdc: isize, c: u32) -> u32;
    fn SetBkMode(hdc: isize, m: i32) -> i32;
    fn TextOutW(hdc: isize, x: i32, y: i32, s: *const u16, n: i32) -> i32;
    fn ExtTextOutW(hdc: isize, x: i32, y: i32, o: u32, rc: *const Rect,
        s: *const u16, n: u32, dx: *const i32) -> i32;
    fn GetTextExtentPoint32W(hdc: isize, s: *const u16, n: i32, sz: *mut Size) -> i32;
}

#[repr(C)]
struct Size { cx: i32, cy: i32 }

fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

// 调色板
const INK: u32 = 0x00F0_EEE8;       // 主文字
const DIM: u32 = 0x0088_8C94;       // 次要文字
const ACCENT: u32 = 0x00FF_8A3C;    // 橙（录音中）
const ACCENT2: u32 = 0x0050_D0FF;   // 青（识别中）
const CARD: u32 = 0x001C_1A18;      // 卡片底

struct Ctx { hdc: isize }

impl Ctx {
    unsafe fn fill(&self, rc: &Rect, color: u32) {
        // 用全块字符铺底（唯一可靠的方式）
        let w = rc.right - rc.left;
        if w <= 0 { return; }
        let old = SetBkMode(self.hdc, 1);
        let oldc = SetTextColor(self.hdc, color);
        let f = self.font(-14, 400, "Microsoft YaHei UI");
        let of = SelectObject(self.hdc, f);
        let cols = (w / 4 + 4) as usize;
        let s: String = std::iter::repeat('\u{2588}').take(cols).collect();
        let u = wide(&s);
        let mut y = rc.top;
        while y < rc.bottom {
            let mut line = Rect { left: rc.left, top: y, right: rc.right, bottom: rc.bottom };
            ExtTextOutW(self.hdc, rc.left, y, 0, ptr::null(), u.as_ptr(), (u.len() - 1) as u32, ptr::null());
            let _ = &mut line;
            y += 12;
        }
        SelectObject(self.hdc, of);
        DeleteObject(f);
        SetTextColor(self.hdc, oldc);
        SetBkMode(self.hdc, old);
    }

    unsafe fn font(&self, h: i32, weight: i32, face: &str) -> isize {
        let f = wide(face);
        CreateFontW(h, 0, 0, 0, weight, 0, 0, 0, 1, 0, 0, 5, 0, f.as_ptr())
    }

    unsafe fn text(&self, x: i32, y: i32, s: &str, color: u32, h: i32, weight: i32) {
        let f = self.font(h, weight, "Microsoft YaHei UI");
        let of = SelectObject(self.hdc, f);
        let oc = SetTextColor(self.hdc, color);
        SetBkMode(self.hdc, 1);
        let u = wide(s);
        ExtTextOutW(self.hdc, x, y, 0, ptr::null(), u.as_ptr(), (u.len() - 1) as u32, ptr::null());
        SetTextColor(self.hdc, oc);
        SelectObject(self.hdc, of);
        DeleteObject(f);
    }

    unsafe fn mono(&self, x: i32, y: i32, s: &str, color: u32, h: i32) {
        let f = self.font(h, 400, "Consolas");
        let of = SelectObject(self.hdc, f);
        let oc = SetTextColor(self.hdc, color);
        SetBkMode(self.hdc, 1);
        let u = wide(s);
        ExtTextOutW(self.hdc, x, y, 0, ptr::null(), u.as_ptr(), (u.len() - 1) as u32, ptr::null());
        SetTextColor(self.hdc, oc);
        SelectObject(self.hdc, of);
        DeleteObject(f);
    }
}

const W: i32 = 760;
const H: i32 = 560;

unsafe extern "system" fn wndproc(h: isize, m: u32, w: usize, l: isize) -> isize {
    match m {
        WM_PAINT => {
            let mut ps: PaintStruct = std::mem::zeroed();
            let hdc = BeginPaint(h, &mut ps);
            if hdc == 0 { return 0; }
            let c = Ctx { hdc };

            // 整窗底
            let all = Rect { left: 0, top: 0, right: W, bottom: H };
            c.fill(&all, 0x0020_1E1C);

            // ============ 方案 A：极简胶囊 ============
            c.text(20, 14, "A · 极简胶囊（最接近系统原生，克制）", DIM, -15, 400);
            // 胶囊：用圆角字符 + 全块铺
            let ay = 44;
            c.fill(&Rect { left: 20, top: ay, right: 420, bottom: ay + 38 }, CARD);
            c.text(30, ay + 6, "●", ACCENT, -20, 400);
            c.text(52, ay + 8, "录音中", INK, -17, 600);
            c.text(120, ay + 9, "今天下午三点十五分开会", 0x00D0_CCC4, -16, 400);

            // ============ 方案 B：双层卡片 ============
            c.text(20, 110, "B · 双层卡片（状态行 + 文本行分离，信息层次清楚）", DIM, -15, 400);
            let by = 140;
            c.fill(&Rect { left: 20, top: by, right: 520, bottom: by + 76 }, CARD);
            // 左侧竖条用全块字符堆
            for i in 0..6 {
                c.text(24, by + 4 + i * 12, "█", ACCENT, -14, 400);
            }
            c.text(40, by + 8, "录音中", ACCENT, -15, 600);
            c.text(110, by + 8, "00:04", DIM, -14, 400);
            c.text(40, by + 32, "今天下午三点十五分开会，讨论一下这个方案", INK, -17, 400);
            // 底部进度条
            let bar = "▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁";
            c.mono(40, by + 56, bar, 0x0038_3430, -12);
            c.mono(40, by + 56, "▔▔▔▔▔▔▔▔▔▔▔▔▔▔", ACCENT, -12);

            // ============ 方案 C：终端 / 等宽 ============
            c.text(20, 240, "C · 终端风格（等宽字体，极客感，信息密度最高）", DIM, -15, 400);
            let cy = 268;
            c.fill(&Rect { left: 20, top: cy, right: 620, bottom: cy + 84 }, 0x0014_1618);
            c.mono(28, cy + 6, "▌", ACCENT, -16);
            c.mono(40, cy + 6, "REC", ACCENT, -14);
            c.mono(72, cy + 6, "00:04", 0x0070_7478, -14);
            c.mono(120, cy + 6, "▸ 今天下午三点十五分开会", INK, -16);
            c.mono(28, cy + 32, "  ", INK, -14);
            c.mono(40, cy + 32, "  讨论一下这个方案，顺便把合同带过来", 0x00C8_C4BC, -16);
            c.mono(28, cy + 58, "  ", INK, -14);
            c.mono(40, cy + 58, "▏", 0x0050_5050, -14);

            // ============ 方案 D：光晕 / 呼吸 ============
            c.text(20, 380, "D · 光晕呼吸（用字符密度模拟发光，最「原生」）", DIM, -15, 400);
            let dy = 408;
            // 多层半块字符模拟外发光
            let glow_outer = "░".repeat(0); // 占位
            let _ = glow_outer;
            c.fill(&Rect { left: 20, top: dy, right: 560, bottom: dy + 96 }, 0x0018_1614);
            // 顶部一条由密到疏的"光"
            let grad = "█▉▊▋▌▍▎▏";
            let mut g = String::new();
            for _ in 0..8 { g.push_str(grad); }
            let g40: String = g.chars().take(40).collect();   // 按**字符**截，别按字节（会 panic）
            c.mono(24, dy + 4, &g40, 0x00A0_5A30, -10);
            c.text(34, dy + 20, "◉", ACCENT, -22, 400);
            c.text(62, dy + 22, "正在听…", 0x00F0_D8C8, -17, 600);
            c.text(34, dy + 52, "今天下午三点十五分开会，讨论一下这个方案", INK, -17, 400);
            // 右下角波纹
            c.mono(470, dy + 62, "▁▂▄▆█▆▄▂▁", 0x00A0_5A30, -13);

            // 底部说明
            c.text(20, 520, "（这是静态原型，四种同时显示便于对比）", DIM, -14, 400);

            EndPaint(h, &ps);
            0
        }
        WM_TIMER => { InvalidateRect(h, ptr::null(), 0); 0 }
        WM_LBUTTONUP => { DestroyWindow(h); 0 }
        WM_MOUSEACTIVATE => MA_NOACTIVATE,
        WM_DESTROY => { PostQuitMessage(0); 0 }
        _ => DefWindowProcW(h, m, w, l),
    }
}

fn main() {
    unsafe {
        let inst = GetModuleHandleW(ptr::null());
        let cls = wide("SainputHudMock");
        let wc = WndClassW {
            style: 0, lpfn: Some(wndproc), cb_cls: 0, cb_wnd: 0,
            h_inst: inst, h_icon: 0, h_cursor: 0, h_br: 0,
            menu: ptr::null(), class: cls.as_ptr(),
        };
        RegisterClassW(&wc);
        let t = wide("HUD 方案对比");
        let h = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            cls.as_ptr(), t.as_ptr(), WS_POPUP, 120, 80, W, H, 0, 0, inst, 0);
        if h == 0 {
            eprintln!("CreateWindowExW failed, err={}", std::io::Error::last_os_error());
            return;
        }
        eprintln!("window created hwnd={h}");
        SetLayeredWindowAttributes(h, 0, 248, LWA_ALPHA);
        SetTimer(h, 1, 80, 0);
        println!("mock window 120,80 {W}x{H}");
        let mut msg: Msg = std::mem::zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
