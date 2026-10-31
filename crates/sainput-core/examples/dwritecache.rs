// T6.4 探针:DWrite 资源(factory / TextFormat / 画刷)是不是**跨帧只建一次**。
//
// 为什么是示例而不是单测:单测里只能建内存 DC + D2D 目标,而 DWrite 的**文字**绘制
// 在那条路上会挂住(实测 240s 超时);生产里画字是在有窗口、有消息循环的那条线程上。
// 所以这条判据必须在**真窗口**里跑 —— 也就是这个示例:
//   建悬浮条 → 推 N 帧不同文本(每帧都触发一次重画)→ 读创建计数。
//
// 期望:推 30 帧之后 factory / TextFormat / 画刷各只有 **1**。退化回"每帧重建"就是 30/30/30。
//
// 用法(claude/omp 在私有桌面上跑,别占着用户屏幕):
//     cargo run --release --example dwritecache
// 退出码:0 = 只建一次;1 = 又变成每帧重建了。

use sainput_core::overlay::{self, Overlay};

fn main() {
    let Some(ov) = Overlay::ensure() else {
        eprintln!("[dwritecache] 悬浮层起不来(没有桌面?)");
        std::process::exit(2);
    };
    const FRAMES: usize = 30;
    let push = |tag: &str| {
        for i in 0..FRAMES {
            ov.show_bar(&format!("{tag} 第 {i} 帧:语音输入法的悬浮条重画"));
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    };
    push("");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (f, tf, br) = overlay::dwrite_create_counts();
    let line = format!("[dwritecache] {FRAMES} 帧之后:factory={f} TextFormat={tf} 画刷={br}");
    println!("{line}");
    // ⚠️ 0 不是"更好":那说明**一帧都没画过**(counters 读错了线程,或者窗口压根没起来)。
    // 所以三样都必须 ≥1 且 ≤1 —— 正好一次。
    let ok = (1..=1).contains(&f) && (1..=1).contains(&tf) && (1..=1).contains(&br);
    let verdict = if ok {
        "PASS:跨帧只建一次"
    } else {
        "FAIL:资源创建次数不是 1(0 = 一帧都没画过;>1 = 又变成每帧重建)"
    };
    println!("[dwritecache] {verdict}");

    // 自校准:每帧都清缓存 = 旧行为。它**必须**给出接近 FRAMES 的数 ——
    // 如果一个探针在任何情况下都报 PASS,那它就不是判据。
    overlay::dwrite_reset_counts();
    for i in 0..FRAMES {
        overlay::dwrite_schedule_drop_cache(); // 模拟"每帧重建"(绘制线程会照做)
        ov.show_bar(&format!("自校准 第 {i} 帧"));
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    std::thread::sleep(std::time::Duration::from_millis(400));
    let (cf, ctf, cbr) = overlay::dwrite_create_counts();
    let sensitive = cf >= 5 && ctf >= 5 && cbr >= 5;
    println!("[dwritecache] 自校准(每帧清缓存):factory={cf} TextFormat={ctf} 画刷={cbr}");
    println!(
        "[dwritecache] {}",
        if sensitive {
            "SENSITIVE:这条判据真的会红"
        } else {
            "BLIND:探针量不出退化,判据无效"
        }
    );
    // 由调用方指定结果文件(私有桌面上跑的时候 stdout 拿不到;见仓库里 probe 们的同款做法)
    if let Some(out) = std::env::args().nth(1) {
        let _ = std::fs::write(
            &out,
            format!("{line}\n{verdict}\n自校准 factory={cf} TextFormat={ctf} 画刷={cbr}\n"),
        );
    }
    std::process::exit(if ok && sensitive { 0 } else { 1 });
}
