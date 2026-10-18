//! 开机自启 —— 写 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`。
//!
//! 为什么用 Run 键而不是启动文件夹：Run 键是**值**不是文件，
//! 用户看不到一个来路不明的快捷方式，程序自己也能干净地开关和改写路径。
//!
//! 关键点：这里注册的**不是 exe**，而是 `sainput.exe run --hide-console`。
//! 直接注册 exe 会因为没有子命令而打印用法就退出。

use std::ptr;

#[link(name = "advapi32")]
extern "system" {
    fn RegCreateKeyExW(
        key: isize, sub: *const u16, reserved: u32, class: *const u16, options: u32,
        sam: u32, sec: *const u8, out: *mut isize, disposition: *mut u32,
    ) -> i32;
    fn RegSetValueExW(key: isize, name: *const u16, reserved: u32, ty: u32,
                      data: *const u8, bytes: u32) -> i32;
    fn RegDeleteValueW(key: isize, name: *const u16) -> i32;
    fn RegQueryValueExW(key: isize, name: *const u16, reserved: *mut u32, ty: *mut u32,
                        data: *mut u8, bytes: *mut u32) -> i32;
    fn RegCloseKey(key: isize) -> i32;
}

const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
const KEY_SET_VALUE: u32 = 0x0002;
const KEY_QUERY_VALUE: u32 = 0x0001;
const REG_SZ: u32 = 1;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "sainput";

fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

unsafe fn open_run_key(sam: u32) -> Option<isize> {
    let sub = wide(RUN_KEY);
    let mut h: isize = 0;
    let mut disp: u32 = 0;
    let rc = RegCreateKeyExW(
        HKEY_CURRENT_USER, sub.as_ptr(), 0, ptr::null(), 0, sam, ptr::null(), &mut h, &mut disp,
    );
    if rc == 0 {
        Some(h)
    } else {
        None
    }
}

/// 当前注册的命令行（没有就是 None）。
pub fn current() -> Option<String> {
    unsafe {
        let h = open_run_key(KEY_QUERY_VALUE)?;
        let name = wide(VALUE_NAME);
        let mut ty: u32 = 0;
        let mut bytes: u32 = 0;
        let rc = RegQueryValueExW(h, name.as_ptr(), ptr::null_mut(), &mut ty, ptr::null_mut(), &mut bytes);
        if rc != 0 || bytes == 0 {
            RegCloseKey(h);
            return None;
        }
        let mut buf = vec![0u8; bytes as usize];
        let rc = RegQueryValueExW(h, name.as_ptr(), ptr::null_mut(), &mut ty, buf.as_mut_ptr(), &mut bytes);
        RegCloseKey(h);
        if rc != 0 {
            return None;
        }
        // 去掉尾部 NUL，再按 UTF-16 解
        while buf.len() >= 2 && buf[buf.len() - 1] == 0 && buf[buf.len() - 2] == 0 {
            buf.truncate(buf.len() - 2);
        }
        let units: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).ok()
    }
}

/// 是否已启用。
pub fn is_enabled() -> bool {
    current().is_some()
}

/// 写入自启项。`exe` 是 sainput.exe 的完整路径。
pub fn enable(exe: &std::path::Path) -> Result<(), String> {
    let cmd = format!("\"{}\" run --hide-console", exe.display());
    let mut data: Vec<u16> = cmd.encode_utf16().collect();
    data.push(0);
    let bytes = (data.len() * 2) as u32;
    unsafe {
        let h = open_run_key(KEY_SET_VALUE).ok_or_else(|| "打不开注册表 Run 键".to_string())?;
        let name = wide(VALUE_NAME);
        let rc = RegSetValueExW(
            h, name.as_ptr(), 0, REG_SZ, data.as_ptr() as *const u8, bytes,
        );
        RegCloseKey(h);
        if rc != 0 {
            return Err(format!("写注册表失败 (code {rc})"));
        }
    }
    Ok(())
}

/// 删掉自启项。
pub fn disable() -> Result<(), String> {
    unsafe {
        let h = open_run_key(KEY_SET_VALUE).ok_or_else(|| "打不开注册表 Run 键".to_string())?;
        let name = wide(VALUE_NAME);
        let rc = RegDeleteValueW(h, name.as_ptr());
        RegCloseKey(h);
        // 值本来就不存在时也算成功
        if rc != 0 && rc != 2 {
            return Err(format!("删注册表值失败 (code {rc})"));
        }
    }
    Ok(())
}
