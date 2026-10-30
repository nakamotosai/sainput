//! 上屏方式真机探针:看**当前焦点窗口**被算成哪种模式,并按那条路把字真的打出去。
//!
//! 为什么要有它:`auto` 的判据是「窗口类名 + 目标线程挂不挂输入法」,
//! 单测只能钉住纯函数那一半(`decide_key_mode` / `class_needs_message_channel`);
//! 真机上到底选中哪条路、那条路在这个窗口上落不落字,只有跑一遍才算数。
//! 2026-10-30 微信「说英文整句没了」就是拿它定的现场:
//! `Weixin.exe`(`Qt51514QWindowIcon`)上 SendInput 的字母一个都不落(进输入法候选框),
//! `WM_CHAR` 逐字落地。
//!
//! 用法(**给窗口标题,让它自己把窗口拿到前台** —— 别指望「跑之前手动点一下」,
//! 命令之间焦点随时会被别的窗口抢走,我就这么把字打到过 Pebrel 的编辑器里):
//!   cargo run --release -p sainput-core --example injectprobe -- "helloTOKEN混排OK" auto WeChat
//!   cargo run --release -p sainput-core --example injectprobe -- "helloTOKEN混排OK" pebrel WeChat
//!
//! 参数:`<text> [policy] [窗口标题子串]`。`policy` 写法和配置里的 `ime_wmchar` 一致,
//! 省略 = `auto`;不写窗口标题 = 谁在前台就打谁(不推荐)。
//! ⚠️ 打出去的字会**留在目标窗口里**(它不回车、不发消息),清干净再走。

use std::time::Duration;

use sainput_core::inject;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let text = args.next().unwrap_or_else(|| "helloTOKEN混排OK".to_string());
    let policy = args.next().unwrap_or_else(|| "auto".to_string());
    let title = args.next();

    if let Some(t) = title.as_deref() {
        match focus_window_with_title(t) {
            Some(h) => println!("[probe] 已把标题含 {t:?} 的窗口拿到前台:{h:#x}"),
            None => {
                println!("[probe] 找不到(或抢不到前台)标题含 {t:?} 的窗口,放弃 —— 宁可什么都不打");
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(400));
    }

    let hwnd = inject::focused_window();
    let class = class_of(hwnd);
    println!("[probe] 焦点窗口 = {hwnd:#x}  class = {class}");
    if hwnd == 0 {
        println!("[probe] 没有焦点窗口,没法测(先点一下目标窗口)");
        return Ok(());
    }

    let mode = inject::target_key_mode(&policy);
    println!("[probe] 策略 = {policy} → {}", inject::describe_target(mode));
    if mode.is_compose() {
        println!("[probe] 组合态那条路要 Pebrel 的控制面,这里只测按键/消息两条通道,跳过");
        return Ok(());
    }

    // 和主循环同一套写法:流式逐帧 + 定稿,前缀也照它来。
    let mut wr = inject::StreamWriter::new(8, true);
    wr.set_prefix(" ");
    wr.set_mode(mode);

    // 逐字累加(真实流式就是「越来越长」),末尾再逼一次「回退一个字又补回来」
    // —— 消息通道的退格(VK_BACK)在这个窗口上认不认,就看这一下。
    let chars: Vec<char> = text.chars().collect();
    let mut acc = String::new();
    for c in &chars {
        acc.push(*c);
        wr.update(&acc)?;
        std::thread::sleep(Duration::from_millis(60));
    }
    let shorter: String = chars[..chars.len().saturating_sub(1)].iter().collect();
    wr.update(&shorter)?;
    std::thread::sleep(Duration::from_millis(120));
    let full: String = chars.iter().collect();
    wr.finalize(&full, 32)?;

    println!(
        "[probe] 上屏 {} 字,退格 {} 次,记账内容 = \"{}\"",
        wr.typed_chars,
        wr.backspaces,
        wr.injected()
    );
    println!("[probe] 现在去目标窗口看:**内容要和上面这行一模一样**才算这条通道可用");
    Ok(())
}

/// 只给探针看的类名读取(生产代码里那份在 `inject::target` 内部,不外露)。
fn class_of(hwnd: isize) -> String {
    #[link(name = "user32")]
    extern "system" {
        fn GetClassNameW(hwnd: isize, buf: *mut u16, max: i32) -> i32;
    }
    if hwnd == 0 {
        return "-".into();
    }
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 0 {
        return "?".into();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

/// 把标题含 `needle` 的可见顶层窗口拿到前台,成功返回它的句柄。
///
/// 为什么要自己抢:探针跑起来的时候「前台」很容易已经是别的东西(命令之间的控制台、
/// 用户随手点的窗口)。抢不到就返回 `None` —— **宁可什么都不打**,也不要打错窗口。
fn focus_window_with_title(needle: &str) -> Option<isize> {
    use std::sync::Mutex;
    // EnumWindows 的回调是裸函数,拿不到闭包环境 —— 用两个静态量把参数和结果递过去。
    static NEEDLE: Mutex<String> = Mutex::new(String::new());
    static FOUND: Mutex<Vec<isize>> = Mutex::new(Vec::new());

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(cb: extern "system" fn(isize, isize) -> i32, l: isize) -> i32;
        fn GetWindowTextW(hwnd: isize, buf: *mut u16, max: i32) -> i32;
        fn IsWindowVisible(hwnd: isize) -> i32;
        fn GetForegroundWindow() -> isize;
        fn SetForegroundWindow(hwnd: isize) -> i32;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
        fn keybd_event(vk: u8, scan: u8, flags: u32, extra: usize);
    }

    extern "system" fn cb(h: isize, _l: isize) -> i32 {
        unsafe {
            if IsWindowVisible(h) == 0 {
                return 1;
            }
            let mut buf = [0u16; 512];
            let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
            if n > 0 {
                let title = String::from_utf16_lossy(&buf[..n as usize]).to_lowercase();
                let hit = NEEDLE.lock().map(|n| title.contains(&*n)).unwrap_or(false);
                if hit {
                    if let Ok(mut v) = FOUND.lock() {
                        v.push(h);
                    }
                }
            }
        }
        1
    }

    *NEEDLE.lock().ok()? = needle.to_lowercase();
    FOUND.lock().ok()?.clear();
    unsafe { EnumWindows(cb, 0) };
    let hwnd = *FOUND.lock().ok()?.first()?;

    for _ in 0..10 {
        unsafe {
            if GetForegroundWindow() == hwnd {
                return Some(hwnd);
            }
            ShowWindow(hwnd, 9); // SW_RESTORE
            // 先敲一下 ALT,绕开 Windows 的 foreground lock
            keybd_event(0x12, 0, 0, 0);
            keybd_event(0x12, 0, 2, 0);
            SetForegroundWindow(hwnd);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    unsafe { (GetForegroundWindow() == hwnd).then_some(hwnd) }
}
