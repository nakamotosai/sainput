//! TSF 通道的**引擎侧**:发现、帧协议、`Composer` 实现。
//!
//! 薄壳(`crates/sainput-tsf`)跑在目标应用进程里,端点是 `\\.\pipe\sainput-tsf-<pid>`。
//! 引擎侧要做的只有三件事:
//!   1. **发现**:按焦点窗口的 pid 算出端点名,`WaitNamedPipeW(..., 0)` 探一下在不在
//!      —— 在 = 我们的 TIP 在那个进程里活着(见 `sainput-tsf/src/pipe.rs` 的生命周期);
//!   2. **握手**:连上先对一次协议版本,不匹配就明确失败,别发一堆没人懂的命令;
//!   3. **转发**:`set`(组合态)/`commit`(定稿)/`cancel`,语义与 `pebrel::Composer` 对齐
//!      —— 于是上屏那套算术(`StreamWriter`)不用改一行就能复用。
//!
//! 为什么这里不做超时:端点在本机回环、服务端是"收到就回"的同步循环,正常时延是微秒级;
//! 真正的卡死风险在**薄壳那边**(它要是崩了/卡了,连接会断,读会拿到 EOF)。
//! 用阻塞 IO + 明确的错误传播,比在这里堆一套超时状态机更可读。要是真出现"连上不回"的
//! 现场,再按 `PLAN-NEXT.md` 的 T2.x 加看门狗。

use crate::pebrel::Composer;
use anyhow::{bail, Context, Result};
use std::ffi::c_void;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;

/// 与薄壳约定的协议版本。对不上就退回别的通道 —— 绝不"尽力而为"地发。
pub const PROTOCOL_VERSION: &str = "sainput-tsf/1";

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;

#[link(name = "kernel32")]
extern "system" {
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

#[link(name = "user32")]
extern "system" {
    fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 端点名。按 pid 算 —— 不需要任何"谁在哪儿"的发现文件或注册表。
pub fn pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\sainput-tsf-{pid}")
}

/// 窗口所属进程的 pid。
pub fn pid_of_window(hwnd: isize) -> u32 {
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    pid
}

/// 那个进程里我们的 TIP 活着吗?
///
/// 判据就是端点**能不能立刻等到一个空闲实例**(不真的连上去,免得占用薄壳唯一的连接槽)。
/// 薄壳在 `Activate` 时起服务端、`Deactivate` 时收摊,所以这一条同时回答了
/// "TSF 通道在不在"和"要不要退回 SendInput / WM_CHAR"。
pub fn available(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let name = wide(&pipe_name(pid));
    unsafe { WaitNamedPipeW(name.as_ptr(), 0) != 0 }
}

/// 组合态的传输层实现:把 `set`/`commit` 走管道交给目标进程里的薄壳。
pub struct TsfComposer {
    pid: u32,
    io: Option<File>,
}

impl TsfComposer {
    pub fn new(pid: u32) -> Self {
        Self { pid, io: None }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    fn connect(&mut self) -> Result<&mut File> {
        if self.io.is_none() {
            let path = pipe_name(self.pid);
            // 等 300ms:刚激活的薄壳可能还在建端点;等不到就明确报错(上层会回退)。
            let w = wide(&path);
            let waited = unsafe { WaitNamedPipeW(w.as_ptr(), 300) };
            if waited == 0 {
                bail!("薄壳端点没就绪:{path}");
            }
            let h = unsafe {
                CreateFileW(
                    w.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    0,
                    0,
                )
            };
            if h == -1 || h == 0 {
                bail!("连不上薄壳端点:{path}");
            }
            let f = unsafe { File::from_raw_handle(h as *mut _) };
            let mut me = Self { pid: self.pid, io: Some(f) };
            // 握手:版本对不上就别再往下走。
            let reply = me.request(b'P', PROTOCOL_VERSION)?;
            if !reply.starts_with("sainput-tsf/") || !reply.starts_with(PROTOCOL_VERSION) {
                bail!("薄壳协议版本不匹配:{reply:?}");
            }
            let f = me.io.take().context("握手后连接丢了")?;
            self.io = Some(f);
        }
        self.io.as_mut().context("连接没建起来")
    }

    /// 发一条命令,读回应答正文(去掉 `O`/`E` 头)。
    fn request(&mut self, cmd: u8, arg: &str) -> Result<String> {
        let io = self.io.as_mut().context("连接没建起来")?;
        let payload = {
            let mut v = Vec::with_capacity(1 + arg.len());
            v.push(cmd);
            v.extend_from_slice(arg.as_bytes());
            v
        };
        write_frame(io, &payload).context("写管道失败")?;
        let reply = read_frame(io).context("读管道失败")?;
        match reply.split_first() {
            Some((b'O', rest)) => Ok(String::from_utf8_lossy(rest).to_string()),
            Some((b'E', rest)) => bail!("薄壳报错:{}", String::from_utf8_lossy(rest)),
            _ => bail!("薄壳应答格式不对:{reply:?}"),
        }
    }

    /// 取消当前组合态(不发定稿)。句子被打断时用。
    pub fn cancel(&mut self) -> Result<()> {
        self.connect()?;
        self.request(b'X', "").map(|_| ())
    }
}

impl Composer for TsfComposer {
    fn set(&mut self, text: &str) -> Result<()> {
        self.connect()?;
        self.request(b'S', text).map(|_| ())
    }

    fn commit(&mut self, text: &str) -> Result<()> {
        self.connect()?;
        self.request(b'C', text).map(|_| ())
    }
}

fn write_frame(f: &mut File, payload: &[u8]) -> std::io::Result<()> {
    let len = (payload.len() as u32).to_le_bytes();
    f.write_all(&len)?;
    f.write_all(payload)?;
    f.flush()
}

fn read_frame(f: &mut File) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    f.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len) as usize;
    if n == 0 || n > (1 << 20) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame length out of range",
        ));
    }
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// 释放连接(显式关掉,让薄壳那边尽快回到"等下一个客户端")。
impl Drop for TsfComposer {
    fn drop(&mut self) {
        if let Some(f) = self.io.take() {
            drop(f);
        }
    }
}
