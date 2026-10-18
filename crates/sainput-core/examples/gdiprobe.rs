// 探针：验证这个窗口上到底哪些 GDI 画法能落笔。
//
// 为什么需要：overlay.rs 的注释说「CreateSolidBrush + FillRect/Rectangle 不落笔，
// 只能用文字路径铺底」。如果这是真的，圆角/渐变这类设计全都做不了。
// 但那条结论是早先试出来的，可能只是某个调用姿势不对。
// 在给用户设计新 HUD 之前，先把它测清楚 —— 否则方案是空中楼阁。
//
// 用法：cargo run --release --example gdiprobe
// 结果：在屏幕 (200,200) 弹一个 520x260 的窗口，分四块画不同东西，人工看哪些出来了。

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

#[repr(C)]
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
    w: usize,
    l: isize,
    time: u32,
    x: i32,
    y: i32,
}
type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;
#[repr(C)]
struct WndClassW {
    style: u32,
    lpfn: Option<WndProc>,
    cb_cls: i32,
    cb_wnd: i32,
    h_inst: isize,
    h_icon: isize,
    h_cursor: isize,
    h_br: isize,
    menu: *const u16,
    class: *const u16,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(c: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex: u32, cls: *const u16, title: *const u16, style: u32,
        x: i32, y: i32, w: i32, h: i32, parent: isize, menu: isize, inst: isize, p: isize,
    ) -> isize;
    fn DefWindowProcW(h: isize, m: u32, w: usize, l: isize) -> isize;
    fn GetMessageW(m: *mut Msg, h: isize, a: u32, b: u32) -> i32;
    fn TranslateMessage(m: *const Msg) -> i32;
    fn DispatchMessageW(m: *const Msg) -> isize;
    fn BeginPaint(h: isize, ps: *mut PaintStruct) -> isize;
    fn EndPaint(h: isize, ps: *const PaintStruct) -> i32;
    fn GetClientRect(h: isize, r: *mut Rect) -> i32;
    fn SetLayeredWindowAttributes(h: isize, key: u32, alpha: u8, flags: u32) -> i32;
    fn SetTimer(h: isize, id: usize, ms: u32, p: isize) -> usize;
    fn PostQuitMessage(c: i32);
    fn GetModuleHandleW(n: *const u16) -> isize;
    fn SetWindowRgn(h: isize, rgn: isize, redraw: i32) -> i32;
    fn InvalidateRect(h: isize, r: *const Rect, e: i32) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateSolidBrush(c: u32) -> isize;
    fn CreatePen(s: i32, w: i32, c: u32) -> isize;
    fn CreateFontW(
        h: i32, w: i32, e: i32, o: i32, weight: i32, it: u32, un: u32, st: u32,
        cs: u32, op: u32, cp: u32, q: u32, pf: u32, face: *const u16,
    ) -> isize;
    fn SelectObject(hdc: isize, o: isize) -> isize;
    fn DeleteObject(o: isize) -> i32;
    fn FillRect(hdc: isize, r: *const Rect, br: isize) -> i32;
    fn Rectangle(hdc: isize, l: i32, t: i32, r: i32, b: i32) -> i32;
    fn RoundRect(hdc: isize, l: i32, t: i32, r: i32, b: i32, ew: i32, eh: i32) -> i32;
    fn SetTextColor(hdc: isize, c: u32) -> u32;
    fn SetBkColor(hdc: isize, c: u32) -> u32;
    fn SetBkMode(hdc: isize, m: i32) -> i32;
    fn TextOutW(hdc: isize, x: i32, y: i32, s: *const u16, n: i32) -> i32;
    fn ExtTextOutW(
        hdc: isize, x: i32, y: i32, opts: u32, rc: *const Rect,
        s: *const u16, n: u32, dx: *const i32,
    ) -> i32;
    fn CreateRoundRectRgn(l: i32, t: i32, r: i32, b: i32, ew: i32, eh: i32) -> isize;
    fn GetStockObject(i: i32) -> isize;
}

fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

const W: i32 = 520;
const H: i32 = 260;

unsafe extern "system" fn wndproc(h: isize, m: u32, w: usize, l: isize) -> isize {
    match m {
        WM_PAINT => {
            let mut ps: PaintStruct = std::mem::zeroed();
            let hdc = BeginPaint(h, &mut ps);
            if hdc == 0 {
                return 0;
            }
            let mut rc: Rect = std::mem::zeroed();
            GetClientRect(h, &mut rc);

            // ---- 区域 1：整窗用 ExtTextOut 铺底（已知能work）
            let bg = wide("                    ");
            SetBkColor(hdc, 0x0028_2220);
            SetBkMode(hdc, 2 /* OPAQUE */);
            let mut r1 = Rect { left: 0, top: 0, right: W, bottom: 60 };
            ExtTextOutW(hdc, 0, 0, 2 /* ETO_OPAQUE */, &r1, bg.as_ptr(), 0, ptr::null());

            // ---- 区域 2：CreateSolidBrush + FillRect（注释说会失败）
            let br = CreateSolidBrush(0x00A0_5030);
            let mut r2 = Rect { left: 10, top: 70, right: 250, bottom: 120 };
            FillRect(hdc, &r2, br);
            DeleteObject(br);

            // ---- 区域 3：RoundRect + 刷子（注释说会失败）
            let br2 = CreateSolidBrush(0x0030_9040);
            let old = SelectObject(hdc, br2);
            RoundRect(hdc, 270, 70, 510, 120, 16, 16);
            SelectObject(hdc, old);
            DeleteObject(br2);

            // ---- 区域 4：Rectangle + 刷子 + 笔
            let br3 = CreateSolidBrush(0x0050_3090);
            let pen = CreatePen(0, 2, 0x00FF_FFFF);
            let ob = SelectObject(hdc, br3);
            let op = SelectObject(hdc, pen);
            Rectangle(hdc, 10, 140, 250, 190);
            SelectObject(hdc, ob);
            SelectObject(hdc, op);
            DeleteObject(br3);
            DeleteObject(pen);

            // ---- 区域 5：文字
            let face = wide("Microsoft YaHei UI");
            let f = CreateFontW(-22, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
            let of = SelectObject(hdc, f);
            SetBkMode(hdc, 1 /* TRANSPARENT */);
            SetTextColor(hdc, 0x00FF_FFFF);
            let t = wide("文字：能不能画");
            TextOutW(hdc, 270, 145, t.as_ptr(), (t.len() - 1) as i32);
            SelectObject(hdc, of);
            DeleteObject(f);

            // 标注
            let labels = [
                ("① ExtTextOut 铺底", 10, 10),
                ("② FillRect+刷子", 10, 72),
                ("③ RoundRect 圆角", 270, 72),
                ("④ Rectangle+刷子", 10, 142),
            ];
            let f2 = CreateFontW(-14, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
            let of2 = SelectObject(hdc, f2);
            SetBkMode(hdc, 1);
            SetTextColor(hdc, 0x00E0_E0E0);
            for (s, x, y) in labels {
                let u = wide(s);
                TextOutW(hdc, x, y, u.as_ptr(), (u.len() - 1) as i32);
            }
            SelectObject(hdc, of2);
            DeleteObject(f2);

            EndPaint(h, &ps);
            0
        }
        WM_TIMER => {
            InvalidateRect(h, ptr::null(), 0);
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE,
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(h, m, w, l),
    }
}

fn main() {
    unsafe {
        let inst = GetModuleHandleW(ptr::null());
        let cls = wide("SainputGdiProbe");
        let wc = WndClassW {
            style: 0,
            lpfn: Some(wndproc),
            cb_cls: 0,
            cb_wnd: 0,
            h_inst: inst,
            h_icon: 0,
            h_cursor: 0,
            h_br: 0,
            menu: ptr::null(),
            class: cls.as_ptr(),
        };
        RegisterClassW(&wc);
        let title = wide("GDI 能力探针");
        let h = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            cls.as_ptr(), title.as_ptr(), WS_POPUP,
            200, 200, W, H, 0, 0, inst, 0,
        );
        if h == 0 {
            eprintln!("create window failed");
            return;
        }
        // 窗口本体做圆角（这招不需要 per-pixel alpha）
        let rgn = CreateRoundRectRgn(0, 0, W + 1, H + 1, 28, 28);
        if rgn != 0 {
            SetWindowRgn(h, rgn, 1);
        }
        SetLayeredWindowAttributes(h, 0, 242, LWA_ALPHA);
        SetTimer(h, 1, 60, 0);
        println!("probe window at 200,200 size {W}x{H}");
        let mut msg: Msg = std::mem::zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
