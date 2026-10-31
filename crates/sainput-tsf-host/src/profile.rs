//! T1.1 的验证器:TSF 的"按进程激活"到底动没动用户的输入法状态。
//!
//! 两件事必须同时成立,TSF 这条路才算能日常用:
//!   ① **不改用户的全局状态** —— 我们在自己的进程里激活自己的 TIP,用户的默认输入法
//!      (`GetDefaultLanguageProfile`)以及**别的进程**看到的当前输入法必须一点不变;
//!   ② **激活之后真的出现文本存储** —— `ITfThreadMgr::GetFocus` 不再为空。
//!      没有它就没有 `ITfContext`,组合态上屏(T1.4)无从谈起。
//!
//! ①为什么要拿**另一个进程**来量:激活是"按进程"生效的,本进程看到的当前输入法
//! 本来就应该变(那正是激活成功的证据)。用户会不会被影响,只有别的进程说了算 ——
//! 所以 `--report-profile` 单跑一次当"旁观者",前后对比。
//!
//! 接口与常量全部来自本机 SDK 的 `um/msctf.idl` / `um/msctf.h`(不靠记忆):
//!   `ITfInputProcessorProfileMgr` = `71c6e74c-0f28-11d8-a82a-00065b84435c`,
//!   槽位顺序 ActivateProfile / DeactivateProfile / GetProfile / EnumProfiles /
//!   ReleaseInputProcessor / RegisterProfile / UnregisterProfile / GetActiveProfile。

use crate::Log;
use sainput_tsf::{Guid, CLSID_SAINPUT_TSF};
use std::ffi::c_void;

/// 我们在注册表里用的语言 profile GUID(与 `tools/tsf_register.ps1` 的默认值一致)。
pub const PROFILE_GUID: Guid = Guid::from_parts(0x6A1F_2B7C, 0x3D4E, 0x4F50, [0x9A, 0x1B, 0x2C, 0x3D, 0x4E, 0x5F, 0x60, 0x71]);
const LANGID_ZH_CN: u16 = 0x0804;
const TF_PROFILETYPE_INPUTPROCESSOR: u32 = 0x0001;
/// 只对**本进程**生效 —— T1.1 的全部要点就在这个标志上。
const TF_IPPMF_FORPROCESS: u32 = 0x1000_0000;
const IID_ITF_INPUT_PROCESSOR_PROFILE_MGR: Guid =
    Guid::from_parts(0x71C6_E74C, 0x0F28, 0x11D8, [0xA8, 0x2A, 0x00, 0x06, 0x5B, 0x84, 0x43, 0x5C]);
/// `GUID_TFCAT_TIP_KEYBOARD`(注册器里写的也是它)。
const GUID_TFCAT_TIP_KEYBOARD: Guid =
    Guid::from_parts(0x3474_5C63, 0xB2F0, 0x4784, [0x8B, 0x67, 0x5E, 0x12, 0xC8, 0x70, 0x1A, 0x31]);

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> isize;
    fn GetProcAddress(module: isize, name: *const u8) -> usize;
}

/// `TF_INPUTPROCESSORPROFILE`(msctf.idl)。字段顺序照抄;对齐交给 Rust 的 repr(C)。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TfInputProcessorProfile {
    dw_profile_type: u32,
    langid: u16,
    clsid: Guid,
    guid_profile: Guid,
    catid: Guid,
    hkl_substitute: isize,
    dw_caps: u32,
    hkl: isize,
    dw_flags: u32,
}

impl Default for TfInputProcessorProfile {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl TfInputProcessorProfile {
    pub fn describe(&self) -> String {
        format!(
            "type={} lang=0x{:04x} clsid={} profile={}",
            self.dw_profile_type,
            self.langid,
            self.clsid.to_hyphen_string(),
            self.guid_profile.to_hyphen_string()
        )
    }
}

#[repr(C)]
struct UnknownVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    add_ref: usize,
    release: usize,
}

#[repr(C)]
struct InputProcessorProfilesVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    add_ref: usize,
    release: usize,
    register: usize,
    unregister: usize,
    add_language_profile: usize,
    remove_language_profile: usize,
    enum_input_processor_info: usize,
    /// `GetDefaultLanguageProfile(langid, catid, out clsid, out guidProfile)`
    get_default_language_profile:
        unsafe extern "system" fn(*mut c_void, u16, *const Guid, *mut Guid, *mut Guid) -> i32,
    /// `SetDefaultLanguageProfile(langid, clsid, guidProfile)`
    set_default_language_profile:
        unsafe extern "system" fn(*mut c_void, u16, *const Guid, *const Guid) -> i32,
    /// `ActivateLanguageProfile(clsid, langid, guidProfile)` —— 老 API。
    /// **实测它只影响当前线程**,不动用户的默认输入法(见 `activate_legacy`)。
    activate_language_profile:
        unsafe extern "system" fn(*mut c_void, *const Guid, u16, *const Guid) -> i32,
    get_active_language_profile: usize,
}

#[repr(C)]
struct ProfileMgrVtbl {
    query_interface: usize,
    add_ref: usize,
    release: usize,
    activate_profile:
        unsafe extern "system" fn(*mut c_void, u32, u16, *const Guid, *const Guid, isize, u32) -> i32,
    deactivate_profile:
        unsafe extern "system" fn(*mut c_void, u32, u16, *const Guid, *const Guid, isize, u32) -> i32,
    get_profile: usize,
    /// `EnumProfiles(langid, out IEnumTfInputProcessorProfiles)`
    enum_profiles:
        unsafe extern "system" fn(*mut c_void, u16, *mut *mut c_void) -> i32,
    release_input_processor: usize,
    register_profile: usize,
    unregister_profile: usize,
    get_active_profile:
        unsafe extern "system" fn(*mut c_void, *const Guid, *mut TfInputProcessorProfile) -> i32,
}

type TfCreateInputProcessorProfilesFn = unsafe extern "system" fn(*mut *mut c_void) -> i32;

unsafe fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 拿到 `ITfInputProcessorProfiles` 与它的 `ITfInputProcessorProfileMgr`。
unsafe fn managers() -> Option<(*mut c_void, *mut c_void)> {
    let m = unsafe { LoadLibraryW(wide("msctf.dll").as_ptr()) };
    if m == 0 {
        return None;
    }
    let f = unsafe { GetProcAddress(m, b"TF_CreateInputProcessorProfiles\0".as_ptr()) };
    if f == 0 {
        return None;
    }
    let create: TfCreateInputProcessorProfilesFn = unsafe { std::mem::transmute(f) };
    let mut profiles: *mut c_void = std::ptr::null_mut();
    if unsafe { create(&mut profiles) } != 0 || profiles.is_null() {
        return None;
    }
    let vt = unsafe { *(profiles as *const *const UnknownVtbl) };
    let mut mgr: *mut c_void = std::ptr::null_mut();
    let iid = IID_ITF_INPUT_PROCESSOR_PROFILE_MGR;
    if unsafe { ((*vt).query_interface)(profiles, &iid, &mut mgr) } != 0 || mgr.is_null() {
        return None;
    }
    Some((profiles, mgr))
}

/// 读 zh-CN 当前的**默认**输入法(用户的口径就是这个)。
pub unsafe fn default_profile() -> Option<(Guid, Guid)> {
    let (profiles, _mgr) = unsafe { managers() }?;
    let pvt = unsafe { *(profiles as *const *const InputProcessorProfilesVtbl) };
    let mut clsid = Guid::default();
    let mut prof = Guid::default();
    let cat = GUID_TFCAT_TIP_KEYBOARD;
    let hr = unsafe {
        ((*pvt).get_default_language_profile)(profiles, LANGID_ZH_CN, &cat, &mut clsid, &mut prof)
    };
    if hr == 0 && clsid.to_hyphen_string() != Guid::default().to_hyphen_string() {
        Some((clsid, prof))
    } else {
        None
    }
}

/// 旁观者视角:默认输入法 + 本线程当前输入法。**任何激活动作都不该改变这两项。**
pub unsafe fn snapshot(log: &Log, tag: &str) {
    let Some((profiles, mgr)) = (unsafe { managers() }) else {
        log.line(&format!("profile[{tag}] 拿不到 TSF 的管理器"));
        return;
    };
    let pvt = unsafe { *(profiles as *const *const InputProcessorProfilesVtbl) };
    let mut clsid = Guid::default();
    let mut prof = Guid::default();
    let cat = GUID_TFCAT_TIP_KEYBOARD;
    let hr = unsafe { ((*pvt).get_default_language_profile)(profiles, LANGID_ZH_CN, &cat, &mut clsid, &mut prof) };
    log.line(&format!(
        "profile[{tag}] 默认(clsid={} profile={}) hr={hr:#x}",
        clsid.to_hyphen_string(),
        prof.to_hyphen_string()
    ));
    let mvt = unsafe { *(mgr as *const *const ProfileMgrVtbl) };
    let mut active = TfInputProcessorProfile::default();
    let hr = unsafe { ((*mvt).get_active_profile)(mgr, &cat, &mut active) };
    log.line(&format!(
        "profile[{tag}] 当前 {} hr={hr:#x}",
        active.describe()
    ));
}

#[repr(C)]
struct EnumProfilesVtbl {
    query_interface: usize,
    add_ref: usize,
    release: usize,
    clone: usize,
    /// `Next(ulCount, out pProfile, out pcFetched)`
    next: unsafe extern "system" fn(*mut c_void, u32, *mut TfInputProcessorProfile, *mut u32) -> i32,
    reset: usize,
    skip: usize,
}

/// 把 msctf **眼里**的输入法列出来 —— 判断我们的注册到底认没认,只有它说了算。
pub unsafe fn enum_profiles(log: &Log) -> usize {
    let Some((_profiles, mgr)) = (unsafe { managers() }) else {
        log.line("enum 失败:拿不到 TSF 的管理器");
        return 0;
    };
    let mvt = unsafe { *(mgr as *const *const ProfileMgrVtbl) };
    let mut en: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { ((*mvt).enum_profiles)(mgr, LANGID_ZH_CN, &mut en) };
    log.line(&format!("enum EnumProfiles(lang=0x{LANGID_ZH_CN:04x}) hr={hr:#x}"));
    if hr != 0 || en.is_null() {
        return 0;
    }
    let evt = unsafe { *(en as *const *const EnumProfilesVtbl) };
    let mut n = 0usize;
    loop {
        let mut prof = TfInputProcessorProfile::default();
        let mut fetched: u32 = 0;
        let _ = unsafe { ((*evt).next)(en, 1, &mut prof, &mut fetched) };
        if fetched == 0 {
            break;
        }
        n += 1;
        let ours = prof.clsid.to_hyphen_string() == CLSID_SAINPUT_TSF.to_hyphen_string();
        log.line(&format!(
            "enum  #{n} {}{}",
            prof.describe(),
            if ours { "   <-- 这是我们" } else { "" }
        ));
        if n > 40 {
            break;
        }
    }
    log.line(&format!("enum 共 {n} 个 profile"));
    n
}

/// 把 zh-CN 的**默认**输入法设回指定的那一个。
///
/// 为什么需要:实测(2026-10-31)`ActivateProfile(…, TF_IPPMF_FORPROCESS)` **连带把
/// 用户在这个语言下的默认 profile 也改了** —— 别的进程跟着加载我们的 TIP,
/// 用户的中文输入就废了。这不是"我们没写好",是那个 API 的语义如此。
/// 所以:自己激活过就必须能自己还回去。
pub unsafe fn set_default(log: &Log, clsid: &Guid, prof: &Guid) -> bool {
    let Some((profiles, _mgr)) = (unsafe { managers() }) else {
        log.line("set_default 失败:拿不到 TSF 的管理器");
        return false;
    };
    let pvt = unsafe { *(profiles as *const *const InputProcessorProfilesVtbl) };
    let hr = unsafe { ((*pvt).set_default_language_profile)(profiles, LANGID_ZH_CN, clsid, prof) };
    log.line(&format!(
        "set_default zh-CN -> clsid={} profile={} hr={hr:#x}",
        clsid.to_hyphen_string(),
        prof.to_hyphen_string()
    ));
    hr == 0
}

/// 走**老 API** `ITfInputProcessorProfiles::ActivateLanguageProfile` 激活自己的 TIP。
///
/// 为什么要试这条:新 API `ActivateProfile` 实测会**连带改掉用户的默认输入法**
/// (2026-10-31 把用户坑了一次)。老 API 的语义是"只为当前线程切",不动全局设置 ——
/// 如果它在当前会话里也真能激活,那就该用它。
pub unsafe fn activate_legacy(log: &Log, tm: *mut c_void) -> bool {
    let Some((profiles, _mgr)) = (unsafe { managers() }) else {
        log.line("legacy 激活失败:拿不到 TSF 的管理器");
        return false;
    };
    let before = unsafe { default_profile() };
    log.line(&format!(
        "legacy 激活前:默认={} 当前线程={}",
        before.as_ref().map(|(c, _)| c.to_hyphen_string()).unwrap_or_else(|| "无".into()),
        snapshot_line(log, "legacy 前")
    ));
    let pvt = unsafe { *(profiles as *const *const InputProcessorProfilesVtbl) };
    let hr = unsafe { ((*pvt).activate_language_profile)(profiles, &CLSID_SAINPUT_TSF, LANGID_ZH_CN, &PROFILE_GUID) };
    log.line(&format!("legacy ActivateLanguageProfile hr={hr:#x}"));
    let after = unsafe { default_profile() };
    let same = match (&before, &after) {
        (Some((a, _)), Some((b, _))) => a.to_hyphen_string() == b.to_hyphen_string(),
        _ => false,
    };
    log.line(&format!(
        "legacy 激活后:默认={} —— {}",
        after.as_ref().map(|(c, _)| c.to_hyphen_string()).unwrap_or_else(|| "无".into()),
        if same { "全局默认**没变**(安全)" } else { "⚠ 全局默认被改了" }
    ));
    let focused = unsafe { crate::tip::focused_document(tm) };
    match focused {
        Some(p) => {
            log.line(&format!("legacy GetFocus 非空=0x{:x} —— 文本存储出现了", p as usize));
            hr == 0 && same
        }
        None => {
            log.line("legacy GetFocus 仍为空 —— 这条路上没有文本存储");
            false
        }
    }
}

/// 只把"当前线程看到的 profile"打出来(返回一行文本,给日志用)。
pub unsafe fn snapshot_line(_log: &Log, _tag: &str) -> String {
    let Some((_profiles, mgr)) = (unsafe { managers() }) else {
        return "?".into();
    };
    let mvt = unsafe { *(mgr as *const *const ProfileMgrVtbl) };
    let cat = GUID_TFCAT_TIP_KEYBOARD;
    let mut active = TfInputProcessorProfile::default();
    let hr = unsafe { ((*mvt).get_active_profile)(mgr, &cat, &mut active) };
    if hr != 0 {
        return format!("hr={hr:#x}");
    }
    active.clsid.to_hyphen_string()
}

/// 在**本进程**里激活我们自己的 TIP,然后看文本存储出没出来。
pub unsafe fn activate_for_process(log: &Log, tm: *mut c_void, hold_ms: u64) -> bool {
    let Some((_profiles, mgr)) = (unsafe { managers() }) else {
        log.line("profile 激活失败:拿不到 TSF 的管理器");
        return false;
    };
    // **必须先把用户原来的默认输入法记下来**:实测 `ActivateProfile(FORPROCESS)`
    // 会**连带改掉这个语言下的默认 profile**(别的进程跟着加载我们的 TIP,
    // 用户的中文输入就废了 —— 2026-10-31 真把用户坑了一次,当场发现并还原)。
    let saved = unsafe { default_profile() };
    match &saved {
        Some((c, p)) => log.line(&format!(
            "profile 记下原默认 clsid={} profile={}(收尾会还回去)",
            c.to_hyphen_string(),
            p.to_hyphen_string()
        )),
        None => log.line("profile ⚠ 读不到原默认输入法,收尾将无法还原"),
    }
    snapshot(log, "激活前(本进程)");
    let mvt = unsafe { *(mgr as *const *const ProfileMgrVtbl) };
    let hr = unsafe {
        ((*mvt).activate_profile)(
            mgr,
            TF_PROFILETYPE_INPUTPROCESSOR,
            LANGID_ZH_CN,
            &CLSID_SAINPUT_TSF,
            &PROFILE_GUID,
            0,
            TF_IPPMF_FORPROCESS,
        )
    };
    log.line(&format!("profile ActivateProfile(FORPROCESS) hr={hr:#x}"));
    if hr != 0 {
        return false;
    }
    snapshot(log, "激活后(本进程)");

    // 【实验/测试】激活后停一会儿:这段窗口里本进程才是"活动 profile 是我们"的状态 ——
    // 有些事(比如写事务)只有在这个状态下才谈得上。窗口一过立刻还原。
    if hold_ms > 0 {
        log.line(&format!("profile 保持激活 {hold_ms}ms(这段窗口里做实验)"));
        let t0 = std::time::Instant::now();
        while t0.elapsed() < std::time::Duration::from_millis(hold_ms) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    // 收尾:**把默认输入法还给用户**(不还就等于把用户的输入法弄坏了)。
    if let Some((c, p)) = saved {
        unsafe { set_default(log, &c, &p) };
        // 写下去之后 msctf 不是立刻反映到读取侧(实测:同一毫秒读回还是我们那条,
        // 于是把"已还原"记成了"⚠ 没还原成功" —— 又一条谎报)。给一拍再确认。
        std::thread::sleep(std::time::Duration::from_millis(600));
        let back = unsafe { default_profile() };
        let ok = matches!(&back, Some((c2, _)) if c2.to_hyphen_string() == c.to_hyphen_string());
        log.line(&format!("profile 默认输入法已还原:{}", if ok { "确认" } else { "⚠ 没还原成功" }));
    }

    // 关键判据:焦点文档管理器还在不在?
    let focused = unsafe { crate::tip::focused_document(tm) };
    match focused {
        Some(ptr) => {
            log.line(&format!(
                "profile GetFocus 非空(有文本存储)=0x{:x} —— 组合态上屏的前提成立",
                ptr as usize
            ));
            true
        }
        None => {
            log.line("profile GetFocus 仍为空 —— 还没有文本存储,组合态做不了");
            false
        }
    }
}
