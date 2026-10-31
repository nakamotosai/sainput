//! 引擎 ↔ 薄壳的命名管道。
//!
//! # 为什么是管道
//!
//! 薄壳跑在**目标应用自己的进程**里(那是唯一能碰它 TSF 上下文的地方),而文本在引擎进程里。
//! 管道跨进程、不跨桌面、不受 UIPI 限制 —— 是这条链上唯一无摩擦的通道。
//! 端点名 `\\.\pipe\sainput-tsf-<pid>`:引擎按**焦点窗口的 pid** 直接算出来,
//! 不需要任何"谁在哪儿"的注册或发现文件(对照 Pebrel 那条路要读 `runtime.port` 发现文件)。
//!
//! # 协议(一个请求一个应答,字节流上自己分帧)
//!
//! ```text
//! 帧 = u32(小端,负载字节数) + 负载
//! 负载 = 1 字节命令 + 参数(UTF-8)
//!   'P' ping      参数:客户端版本串        应答:'O' + 薄壳版本串
//!   'S' set       参数:组合态文本          应答:'O' + 当前组合态
//!   'C' commit    参数:定稿文本            应答:'O' + 已定稿字节数
//!   'X' cancel    参数:空                  应答:'O'
//!   'Q' query     参数:空                  应答:'O' + "preedit|committed"(诊断/测试用)
//! 应答 = 'O'(ok) 或 'E'(error,后跟 UTF-8 错误串)
//! ```
//!
//! 为什么不直接用它上屏:`S/C/X` 这三个动作的**语义**已经和 `pebrel::Composer`
//! (`set`/`commit`)对齐了 —— 引擎侧换个传输层就能复用同一条上屏算术。
//! 真正写进 TSF 上下文那一步在组合态(`tfs`/T1.4)那一环,见 `apply` 的注释。
//!
//! # 规矩
//!
//! 服务端跑在**别人的进程**里:线程体整个 `catch_unwind` 兜住,任何异常只断掉当前连接,
//! 绝不让宿主应用崩掉。诊断照旧落文件(`trace`)。

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

#[link(name = "kernel32")]
extern "system" {
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        out_buffer: u32,
        in_buffer: u32,
        timeout: u32,
        security: *mut c_void,
    ) -> isize;
    fn ConnectNamedPipe(h: isize, overlapped: *mut c_void) -> i32;
    fn DisconnectNamedPipe(h: isize) -> i32;
    fn FlushFileBuffers(h: isize) -> i32;
    fn ReadFile(h: isize, buf: *mut u8, n: u32, read: *mut u32, ov: *mut c_void) -> i32;
    fn WriteFile(h: isize, buf: *const u8, n: u32, written: *mut u32, ov: *mut c_void) -> i32;
    fn CloseHandle(h: isize) -> i32;
    fn GetCurrentProcessId() -> u32;
    fn GetTickCount64() -> u64;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: isize,
    ) -> isize;
    fn WaitNamedPipeW(name: *const u16, timeout: u32) -> i32;
}

use crate::trace;

const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
const PIPE_READMODE_BYTE: u32 = 0x0000_0000;
const PIPE_WAIT: u32 = 0x0000_0000;
const BUFFER: u32 = 1 << 16;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
/// 一帧负载的上限(防御性:对面要是发疯了,不能让我们把内存吃光)。
const MAX_FRAME: usize = 1 << 20;

/// 协议版本 —— 引擎拿它做握手,不匹配就退回别的通道,而不是发一堆没人懂的命令。
pub const PROTOCOL_VERSION: &str = "sainput-tsf/1";

/// 服务端循环还在跑(决定 `ensure_server` 要不要重新起)。
static SERVER_ACTIVE: AtomicBool = AtomicBool::new(false);
/// **我们自己的线程**还有几个活着。`DllCanUnloadNow` 必须认它 ——
/// 任何一条线程还在跑就被 `FreeLibrary` 卸掉,等于让宿主应用跳进一片已解除映射的代码。
/// (服务端、看门狗、唤醒线程都算;只算服务端是不够的 —— 看门狗漏算过一次,实测崩。)
static LIVE_THREADS: AtomicU32 = AtomicU32::new(0);
static STOP: AtomicBool = AtomicBool::new(false);


#[derive(Default)]
struct State {
    /// 当前组合态(preedit):还没提交的那一版。
    preedit: String,
    /// 已经定稿的累计字节数(只用于应答,不存正文 —— 正文的归属在应用那边)。
    committed_bytes: usize,
    /// 最后一次动组合态的时刻(看门狗按它判"这句是不是没人管了")。
    last_activity_ms: u64,
}

/// 组合态多久没人动就自动取消。
///
/// 为什么必须有:组合态是**留在别人文档里**的一段 preedit。要是引擎崩了、热键被吞了、
/// 或者用户直接切走了窗口,这一版就会一直挂在那儿 —— 用户看到的是"输入法卡住了,
/// 打不出字也退不掉"。宁可自己撤掉(屏幕上恢复原样),也不能把烂摊子留给应用。
const PREEDIT_IDLE_TIMEOUT_MS: u64 = 5000;

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(State::default))
}

pub fn pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\sainput-tsf-{pid}")
}

fn spawn_counted(name: &str, f: impl FnOnce() + Send + 'static) {
    LIVE_THREADS.fetch_add(1, Ordering::SeqCst);
    let started = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
                trace("thread died (panic caught)");
            }
            LIVE_THREADS.fetch_sub(1, Ordering::SeqCst);
        });
    if started.is_err() {
        LIVE_THREADS.fetch_sub(1, Ordering::SeqCst);
        trace(&format!("thread {name} failed to start"));
    }
}

/// 起服务端(在 `Activate` 里调)。已经活着就不重复起。
///
/// 生命周期**跟着激活走**:TSF 反激活(`Deactivate`)就收摊。
/// 这样引擎那边的判据很干净 —— "那个 pid 的管子在不在" == "我们的 TIP 在那个进程里是不是活的"。
pub fn ensure_server() {
    // 先撤销"待停止":TSF 有可能 Deactivate 之后马上又 Activate,
    // 那一次反激活已经把停止位标上了,这里不撤的话服务端会白死一次。
    STOP.store(false, Ordering::SeqCst);
    if SERVER_ACTIVE.swap(true, Ordering::SeqCst) {
        return; // 已经在跑
    }
    // 已知的窄竞态:线程正在退出的那一瞬间(ALIVE 还没落成 false)重复激活会拿不到服务端。
    // 代价只是"这一次引擎找不到管道、退回旧通道",不会出错 —— 所以不为此加锁。
    let name = pipe_name(unsafe { GetCurrentProcessId() });
    // **句柄在这里同步建好**,线程只管用。这样 `request_stop` 永远有一个可关的句柄
    // 去唤醒阻塞中的线程 —— 否则"刚激活就被反激活"会留下一个永远等连接的线程,
    // 而那个线程还活着时宿主一旦 `FreeLibrary`,就是跳进已解除映射的代码。
    let h = create_server(&name);
    if h == 0 {
        trace("pipe CreateNamedPipe failed");
        SERVER_ACTIVE.store(false, Ordering::SeqCst);
        return;
    }
    trace(&format!("pipe server starting at {name} (handle 0x{h:x})"));
    spawn_counted("sainput-tsf-pipe", move || {
        serve(h);
        SERVER_ACTIVE.store(false, Ordering::SeqCst);
        trace("pipe server thread exited");
    });
    spawn_counted("sainput-tsf-watchdog", watchdog);
}

/// 收摊(在 `Deactivate` 里调)。
///
/// **不在调用方线程上 join、也不从外面关句柄** —— 两条都会卡住宿主应用:
/// `Deactivate` 是 msctf 在**别人的线程**里调我们的,在那里等一个阻塞的线程、
/// 或者 `CloseHandle` 一个正有同步 I/O 挂着的句柄(实测会一直等到那次 I/O 结束),
/// 都会把那个应用卡死。
///
/// 做法:标一个停止位,再**自己当客户端连一下** —— 服务端那个阻塞的
/// `ConnectNamedPipe` 就会返回,线程醒来看到停止位就自己收摊、自己关句柄。
/// 唤醒动作放独立线程,调用方立刻返回。
pub fn request_stop() {
    STOP.store(true, Ordering::SeqCst);
    if !SERVER_ACTIVE.load(Ordering::SeqCst) {
        return;
    }
    let name = pipe_name(unsafe { GetCurrentProcessId() });
    spawn_counted("sainput-tsf-wake", move || {
        // 只为了把服务端叫醒;连上就关。
        if let Some(h) = open_client(&name, 1000) {
            unsafe { CloseHandle(h) };
        }
    });
}

/// 以客户端身份连一次(给唤醒用,也给 `is_available` 用)。返回 0 = 没连上。
pub fn open_client(name: &str, wait_ms: u32) -> Option<isize> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if WaitNamedPipeW(wide.as_ptr(), wait_ms) == 0 {
            return None;
        }
        let h = CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            0,
        );
        if h == -1 || h == 0 {
            None
        } else {
            Some(h)
        }
    }
}

/// 我们还有线程活着吗?(`DllCanUnloadNow` 用)
pub fn threads_alive() -> bool {
    LIVE_THREADS.load(Ordering::SeqCst) > 0
}

/// 建服务端点。返回 0 = 失败。
fn create_server(name: &str) -> isize {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let h = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1, // 一次一个客户端:上屏是串行的,多开只会让谁先谁后更难说清
            BUFFER,
            BUFFER,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == -1 || h == 0 {
        0
    } else {
        h
    }
}

fn serve(h: isize) {
    while !STOP.load(Ordering::SeqCst) {
        // 阻塞等一个客户端。反激活那条路会 `CloseHandle` 把它唤醒(随后返回失败),
        // 所以这里不需要超时轮询。
        unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) };
        if STOP.load(Ordering::SeqCst) {
            break;
        }
        // 取消连接时错误码是 ERROR_PIPE_CONNECTED(535),仍然算成功。
        let mut guard_bytes = [0u8; 4];
        loop {
            let Some(payload) = read_frame(h, &mut guard_bytes) else {
                break;
            };
            let reply = handle(&payload);
            if write_frame(h, &reply).is_err() {
                break;
            }
        }
        unsafe {
            FlushFileBuffers(h);
            DisconnectNamedPipe(h);
        }
    }
    unsafe { CloseHandle(h) };
    trace("pipe server stopped");
}

fn read_exact(h: isize, buf: &mut [u8]) -> Result<(), ()> {
    let mut off = 0usize;
    while off < buf.len() {
        let mut got: u32 = 0;
        let ok = unsafe {
            ReadFile(
                h,
                buf[off..].as_mut_ptr(),
                (buf.len() - off) as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || got == 0 {
            return Err(());
        }
        off += got as usize;
    }
    Ok(())
}

fn read_frame(h: isize, len_buf: &mut [u8; 4]) -> Option<Vec<u8>> {
    read_exact(h, len_buf).ok()?;
    let n = u32::from_le_bytes(*len_buf) as usize;
    if n == 0 || n > MAX_FRAME {
        trace(&format!("pipe bad frame length {n}"));
        return None;
    }
    let mut buf = vec![0u8; n];
    read_exact(h, &mut buf).ok()?;
    Some(buf)
}

fn write_frame(h: isize, payload: &[u8]) -> Result<(), ()> {
    let len = (payload.len() as u32).to_le_bytes();
    for chunk in [&len[..], payload] {
        let mut off = 0usize;
        while off < chunk.len() {
            let mut wrote: u32 = 0;
            let ok = unsafe {
                WriteFile(
                    h,
                    chunk[off..].as_ptr(),
                    (chunk.len() - off) as u32,
                    &mut wrote,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 || wrote == 0 {
                return Err(());
            }
            off += wrote as usize;
        }
    }
    Ok(())
}

fn now_ms() -> u64 {
    unsafe { GetTickCount64() }
}

/// 看门狗:组合态闲置超过 `PREEDIT_IDLE_TIMEOUT_MS` 就自己撤掉。
fn watchdog() {
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if STOP.load(Ordering::SeqCst) {
            return;
        }
        let dropped = with_state(|st| {
            if !st.preedit.is_empty()
                && now_ms().saturating_sub(st.last_activity_ms) > PREEDIT_IDLE_TIMEOUT_MS
            {
                st.preedit.clear();
                true
            } else {
                false
            }
        });
        if dropped {
            trace("pipe watchdog: 组合态闲置超时,已取消(不让半句话留在应用里)");
        }
    }
}

fn ok(extra: &str) -> Vec<u8> {
    let mut v = vec![b'O'];
    v.extend_from_slice(extra.as_bytes());
    v
}

fn err(msg: &str) -> Vec<u8> {
    let mut v = vec![b'E'];
    v.extend_from_slice(msg.as_bytes());
    v
}

/// 处理一条命令。**这是薄壳的数据面**:文本在这里进出。
fn handle(payload: &[u8]) -> Vec<u8> {
    let Some((&cmd, rest)) = payload.split_first() else {
        return err("empty frame");
    };
    let arg = String::from_utf8_lossy(rest).to_string();
    match cmd {
        b'P' => {
            if arg != PROTOCOL_VERSION {
                return err(&format!("version mismatch: got {arg:?}"));
            }
            ok(&format!("{} pid={}", PROTOCOL_VERSION, unsafe { GetCurrentProcessId() }))
        }
        b'S' => with_state(|st| {
            st.preedit = arg.clone();
            st.last_activity_ms = now_ms();
            trace(&format!("pipe set preedit={arg:?}"));
            ok(&st.preedit)
        }),
        b'C' => with_state(|st| {
            let n = arg.len();
            st.committed_bytes += n;
            st.preedit.clear();
            st.last_activity_ms = now_ms();
            trace(&format!("pipe commit {n} bytes"));
            ok(&format!("{}", st.committed_bytes))
        }),
        b'X' => with_state(|st| {
            st.preedit.clear();
            st.last_activity_ms = now_ms();
            trace("pipe cancel");
            ok("")
        }),
        b'Q' => with_state(|st| ok(&format!("{}|{}", st.preedit, st.committed_bytes))),
        other => err(&format!("unknown command {:?}", other as char)),
    }
}
