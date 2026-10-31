//! 单实例 —— 语义是「后来者接管」，而不是「第二个实例自己退出」。
//!
//! 为什么必须只有一个：`run` 会挂 WH_MOUSE_LL / WH_KEYBOARD_LL 全局钩子，
//! 两个进程同时跑就是**两个钩子同时收事件** —— 说一句话屏幕上打两遍。
//!
//! 为什么是接管而不是退场：每次重新构建后双击快捷方式，跑起来的必须是最新二进制。
//! 如果新进程只是打印一句「已经有一个在跑」然后退出，用户会一直用着几小时前的老进程，
//! 旧进程还留着（旧 bug 也随之留着）。现在的规则：
//!   1. 能拿到锁 → 直接跑；
//!   2. 拿不到 → 让所有旧 sainput 退出（优先托盘退出，保留正常收尾：sidecar 由 Drop
//!      结束、日志正常收尾），赖着不走的硬杀（连它的子进程），然后再拿一次锁。
//! 拿不到锁（例如旧实例以管理员权限运行、杀不掉）就报错退出，
//! 绝不允许两个实例同时挂全局钩子。

use std::ptr;
use std::time::{Duration, Instant};

#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(attrs: *const u8, owner: i32, name: *const u16) -> isize;
    fn CloseHandle(h: isize) -> i32;
    fn ReleaseMutex(h: isize) -> i32;
    fn WaitForSingleObject(h: isize, ms: u32) -> u32;
    fn GetCurrentProcessId() -> u32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
    fn Process32FirstW(snap: isize, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snap: isize, entry: *mut ProcessEntry32W) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
    fn TerminateProcess(h: isize, code: u32) -> i32;
}

const ERROR_ALREADY_EXISTS: u32 = 183;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
const INVALID_HANDLE_VALUE: isize = -1;
const SYNCHRONIZE: u32 = 0x0010_0000;
const PROCESS_TERMINATE: u32 = 0x0001;
const WAIT_TIMEOUT: u32 = 258;
const MAX_PATH: usize = 260;

/// 本程序的可执行文件名 —— 接管时按它找人，别的进程一概不碰。
const EXE_NAME: &str = "sainput.exe";
const RUN_MUTEX: &str = r"Local\sainput-single-instance-v1";
/// 「换人」动作的闸：两个新实例同时启动时串行执行，避免互相乱杀。
const TAKEOVER_MUTEX: &str = r"Local\sainput-takeover-v1";

/// 【回归/诊断专用】`SAINPUT_SINGLE_NS=<名字>` 换一套"单实例命名空间"。
///
/// 为什么必须有:不带命名空间时,**任何**用 `sainput run` 做的自动化测试都会
/// "接管"掉用户正在用的那个实例(按 exe 名找进程、请它退出、赖着不走就硬杀)——
/// 也就是说测试会把用户的输入法踢下线。带上命名空间之后:
///   * 锁名换成 `Local\<ns>-single-instance-v1`;
///   * "接管"只找 `<ns>.exe`,于是永远不会碰到真正的 `sainput.exe`。
/// 不设这个环境变量时,行为与从前**完全一致**。
fn ns() -> String {
    std::env::var("SAINPUT_SINGLE_NS")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| {
            !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .unwrap_or_else(|| "sainput".to_string())
}

fn run_mutex_name() -> String {
    format!(r"Local\{}-single-instance-v1", ns())
}

fn takeover_mutex_name() -> String {
    format!(r"Local\{}-takeover-v1", ns())
}

fn exe_name() -> String {
    let n = ns();
    if n == "sainput" {
        EXE_NAME.to_string()
    } else {
        format!("{n}.exe")
    }
}

#[repr(C)]
struct ProcessEntry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; MAX_PATH],
}

struct ProcInfo {
    pid: u32,
    parent: u32,
    exe: String,
}

fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// 尝试拿到「本机只有一个 sainput」的锁。
///
/// 拿到返回 `Some(guard)`，guard 活着锁就在；已经有实例在跑返回 `None`。
/// 用 `Local\` 前缀：锁的作用域是当前登录会话，不是整台机器 ——
/// 同一台机器上两个用户各跑一个自己的输入法是合理的。
fn try_acquire() -> Option<Guard> {
    let name = wide(&run_mutex_name());
    let h = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
    if h == 0 {
        // 建不出来就当没锁 —— 宁可多开也不要因为一个保护措施起不来。
        return Some(Guard { handle: 0 });
    }
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
    if err == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(h);
        }
        return None;
    }
    Some(Guard { handle: h })
}

/// 启动入口：拿得到锁就直接跑；拿不到就把旧实例全部换掉，再拿一次。
///
/// 返回 `None` 表示旧实例退不掉（权限不足 / 卡在无法终止的状态），
/// 调用方必须放弃启动 —— 两个全局钩子同时在跑比起不来更糟。
pub fn acquire_or_take_over() -> Option<Guard> {
    if let Some(g) = try_acquire() {
        return Some(g);
    }

    // 串行化「换人」：两个新实例同时启动时，一个先换完并跑起来，另一个再换它。
    // 最终仍然只有一个进程，且是最新启动的那个。
    let gate = unsafe { CreateMutexW(ptr::null(), 0, wide(&takeover_mutex_name()).as_ptr()) };
    if gate != 0 {
        unsafe {
            WaitForSingleObject(gate, 15_000);
        }
    }

    replace_others();

    let deadline = Instant::now() + Duration::from_secs(6);
    let mut guard = None;
    loop {
        if let Some(g) = try_acquire() {
            guard = Some(g);
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if gate != 0 {
        unsafe {
            ReleaseMutex(gate);
            CloseHandle(gate);
        }
    }
    guard
}

/// 让所有其它 sainput 进程退出。
///
/// 先请求走托盘退出（正常收尾），超时才硬杀；硬杀时连它的子进程一起 ——
/// python sidecar 是子进程，只杀父进程会留下孤儿。
fn replace_others() {
    let me = unsafe { GetCurrentProcessId() };
    let old: Vec<u32> = snapshot()
        .into_iter()
        .filter(|p| p.pid != me && p.exe.eq_ignore_ascii_case(&exe_name()))
        .map(|p| p.pid)
        .collect();
    if old.is_empty() {
        return;
    }

    // 1) 有托盘窗口的走正常退出：主循环收尾，sidecar 由 Drop 结束。
    //    没有窗口的（旧版没起托盘 / 托盘线程已死 / 还没起窗口）没法优雅退出，直接硬杀。
    let mut graceful: Vec<u32> = Vec::new();
    let mut stubborn: Vec<u32> = Vec::new();
    for pid in &old {
        if crate::tray::request_exit_of(*pid) {
            graceful.push(*pid);
        } else {
            stubborn.push(*pid);
        }
    }
    if !stubborn.is_empty() {
        hard_kill(&stubborn);
    }
    if graceful.is_empty() || wait_gone(&graceful, Duration::from_secs(4)) {
        return;
    }

    // 2) 有窗口却赖着不走的：硬杀，先杀子进程再杀本体。
    hard_kill(&graceful);
    let _ = wait_gone(&graceful, Duration::from_secs(3));
}

/// 强制结束这些进程，连它们的子进程一起 —— python sidecar 是子进程，
/// 只杀父进程会留下孤儿。终止前按进程快照核对可执行文件名：
/// 目标可能已经退出、PID 又被别的进程复用，绝不能误杀。
fn hard_kill(pids: &[u32]) {
    let table = snapshot();
    for pid in pids.iter().copied() {
        let still_ours = table
            .iter()
            .any(|p| p.pid == pid && p.exe.eq_ignore_ascii_case(&exe_name()));
        if !still_ours {
            continue;
        }
        for child in descendants(pid, &table) {
            terminate(child);
        }
        terminate(pid);
    }
}

fn wait_gone(pids: &[u32], limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if pids.iter().all(|p| !is_alive(*p)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe {
        let h = OpenProcess(SYNCHRONIZE, 0, pid);
        if h == 0 {
            // 打不开：可能已经退出，也可能权限不够（旧实例是管理员）。
            // 用进程快照确认真身，权限不够时按「还活着」处理，交给上层报错。
            return snapshot().iter().any(|p| p.pid == pid);
        }
        let r = WaitForSingleObject(h, 0);
        CloseHandle(h);
        r == WAIT_TIMEOUT
    }
}

fn terminate(pid: u32) {
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE | SYNCHRONIZE, 0, pid);
        if h != 0 {
            TerminateProcess(h, 1);
            WaitForSingleObject(h, 2_000);
            CloseHandle(h);
        }
    }
}

/// 收集 root 的所有后代 pid（用来连带清理 sidecar）。
fn descendants(root: u32, table: &[ProcInfo]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let mut frontier = vec![root];
    while let Some(p) = frontier.pop() {
        for c in table.iter().filter(|x| x.parent == p) {
            if c.pid != root && !out.contains(&c.pid) {
                out.push(c.pid);
                frontier.push(c.pid);
            }
        }
    }
    out
}

fn snapshot() -> Vec<ProcInfo> {
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: ProcessEntry32W = std::mem::zeroed();
        e.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
        if Process32FirstW(snap, &mut e) != 0 {
            loop {
                let len = e
                    .sz_exe_file
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(MAX_PATH);
                out.push(ProcInfo {
                    pid: e.th32_process_id,
                    parent: e.th32_parent_process_id,
                    exe: String::from_utf16_lossy(&e.sz_exe_file[..len]),
                });
                if Process32NextW(snap, &mut e) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

/// 锁的所有者。Drop 时释放句柄（进程退出时内核本来也会回收，这是双保险）。
pub struct Guard {
    handle: isize,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.handle != 0 {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}
