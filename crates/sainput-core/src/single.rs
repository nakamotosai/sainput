//! 单实例保护 —— 双击两次不应该出现两个输入法。
//!
//! 为什么必须做：`run` 会挂 WH_MOUSE_LL / WH_KEYBOARD_LL 全局钩子，
//! 两个进程同时跑就是**两个钩子同时收事件** —— 说一句话屏幕上打两遍，
//! 而且两个托盘图标、两份日志，用户完全看不出是怎么回事。
//! 桌面快捷方式尤其容易双击两次（第一次点了没反应，用户会再点一次）。
//!
//! 做法是命名互斥量 + `ERROR_ALREADY_EXISTS`。句柄**故意不释放**：
//! 进程活着它就在，进程死了内核自动回收，这正是我们要的语义。

use std::ptr;

#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(attrs: *const u8, owner: i32, name: *const u16) -> isize;
    fn CloseHandle(h: isize) -> i32;
}

const ERROR_ALREADY_EXISTS: u32 = 183;

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
pub fn acquire() -> Option<Guard> {
    let name = wide(r"Local\sainput-single-instance-v1");
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
