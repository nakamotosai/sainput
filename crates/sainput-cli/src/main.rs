//! sainput CLI —— 本地语音输入法（全本地、零网络）。
//!
//!   devices                 列出输入设备
//!   record <out.wav>        采集并写出 16k 单声道 WAV
//!   probe                   采集并打印帧节奏 / 抖动 / 丢帧
//!   replay <in.wav>         把一段 WAV 走完整流水线（离线验收）
//!   render <text>           只跑 ITN + 标点 + 润色（文本后处理单测）
//!   config                  打印当前生效的配置
//!   run                     常驻：按住热键说话，松开上屏
//!   selftest inject <text>  只测上屏路径

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use sainput_core::asr::{Msg, Sidecar};
use sainput_core::config::{self, Settings};
use sainput_core::overlay::{EdgeTint, Overlay};
use sainput_core::tray::Tray;
use sainput_core::{audio, autostart, caretctx, context, hotkey, inject, metrics, single, tray, wav};

/// 默认模型：x-asr-160ms。
///
/// 换掉 2023 双语档的原因（同一批真实语音实测，见 README「模型」一节）：
/// 老档在真机中文上会出现重复循环（「重点重点」「动动动荡荡荡」，重复标记 4 处），
/// x-asr-160ms 是 0 处，同时还能认出英文（MONDAY / THE DAY AFTER TOMORROW / ALWAYS /
/// FREQUENTLY 都对），RTF 0.10（比实时快约 10 倍）。
const DEFAULT_MODEL: &str =
    "sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05";
const DEFAULT_PUNCT: &str = "sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8";
const CONF_NAME: &str = "sainput.conf";
const HOTWORDS_NAME: &str = "hotwords.txt";

fn usage() {
    println!(
        "sainput {} — 本地语音输入法（全本地，不联网）\n\n\
         USAGE:\n  \
         sainput devices                          列出输入设备\n  \
         sainput record <out.wav> [--seconds N]   采集并写出 16k 单声道 WAV\n  \
         sainput probe [--seconds N]              采集并打印帧节奏 / 抖动 / 丢帧\n  \
         sainput replay <in.wav> [--dry-run]      把 WAV 走完整流水线（离线验收）\n  \
         sainput render \"一段文字\"                只跑 ITN + 标点 + 润色\n  \
         sainput config                           打印当前生效的配置\n  \
         sainput overlay-demo [--mode bar|orb|edge] [--seconds N]  预览悬浮显示\n  \
         sainput run [options]                    常驻：按住热键说话，松开上屏\n  \
         sainput release-keys                     释放卡住的修饰键（键盘失灵时用）\n  \
         sainput selftest inject <text>           只测上屏路径\n\n\
         OPTIONS:\n  \
         --model-dir <DIR>      ASR 模型目录（默认 models/{DEFAULT_MODEL}）\n  \
         --punct-dir <DIR>      标点模型目录（默认 models/{DEFAULT_PUNCT}）\n  \
         --no-punct             关闭标点\n  \
         --no-itn               关闭阿拉伯数字归一化（「三十四个」保持汉字）\n  \
         --hotwords <FILE>      热词表（一行一个词，默认 hotwords.txt）\n  \
         --beam                 用 modified_beam_search（热词需要；更准、稍慢）\n  \
         --throttle-ms <N>      流式渲染节拍，默认 80ms（小于模型更新间隔就等于不限流）\n  \
         --hotkey <KEY>         说话热键（默认 rightctrl）\n  \
                                 键盘：rightctrl / rightalt / f8 / f9 / f10 / 0xXX\n  \
                                 鼠标：mousex1 / mousex2（侧键）/ mousemiddle（中键）\n  \
         --toggle               按一下开始、再按一下结束（默认按住说话）\n  \
         --hold                 按住说话（默认）\n  \
         --device <NAME>        指定麦克风（默认系统默认设备）\n  \
         --no-stream            当前不可用（未实现：选了它一个字都不会上屏）\n  \
         --overlay-mode <M>     悬浮显示：off / bar（细条）/ orb（悬浮球）/ edge（屏幕边缘光）\n  \
         --no-tray              关掉系统托盘图标\n  \
         --show-console         不自动收起控制台窗口\n  \
         --lazy-mic             按需开麦（默认常驻预热，避免吞掉开头）\n  \
         --max-backspace <N>    流式修正安全阀(默认 8)\n  \
         --ime-wmchar <MODE>    上屏方式:auto(默认)/ on / off / on:app.exe。目标窗口挂着\n  \
                                输入法时,注入的 ASCII 会被它收走,这时改走 WM_CHAR\n  \
         --tail-ms <N>          松键后最少再收多久的尾巴音频,默认 200ms\n  \
         --tail-max-ms <N>      尾巴硬上限，默认 1200ms\n  \
         --tail-quiet-ms <N>    尾巴里连续安静这么久就收工，默认 250ms\n  \
         --tail-pad-ms <N>      收工时一次性补灌多少毫秒底噪（模型的右上下文），默认 480ms\n  \
         --tail-rms <F>         静音判定阈值（帧 RMS），默认 0.012\n  \
         --feed-release-at <S>  测试用：--feed 喂到第 S 秒自动松键（吞字回归）\n  \
         --python <EXE>         Python 解释器（默认自动探测）\n  \
         --dry-run              replay 时不真的注入，只打印文本\n\n\
         CONFIG:\n  \
         仓库根下的 {CONF_NAME} 可写日常偏好（key = value），命令行参数优先级更高。\n  \
         可用键：hotkey / model_dir / punct_dir / hotwords / punct / itn / beam / toggle / tray /\n  hide_console / \
         overlay_mode / stream / lazy_mic / max_backspace / throttle_ms / threads / device /\n         \
         tail_ms / tail_max_ms / tail_quiet_ms / tail_pad_ms / tail_rms /\n         \
         punct_mode / merge_terms / fix_case / ctx_bias / ime_wmchar",
        env!("CARGO_PKG_VERSION")
    );
}

// ------------------------------------------------------------------ 小工具
fn opt(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1).cloned())
}

fn flag(args: &[String], key: &str) -> bool {
    args.iter().any(|a| a == key)
}

/// 托盘开关的**实时**状态（主循环写、sidecar 读）。
///
/// 为什么不是启动参数：托盘菜单是**运行中**点的，改完必须立刻生效，
/// 不能要求重开。所以这几个开关放在共享原子量里，每句开始时问一次当前值。
static PUNCT_SPACE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static MERGE_TERMS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static CTX_BIAS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static VERBOSE_CTX: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 光标前有字时，在前面补一个空格（默认开 —— 用户明确要的）。
static LEADING_SPACE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// 英文大小写修复（默认开：本仓库模型吐不出小写）。
static FIX_CASE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// 静音多少秒算一句说完（0 = 不断句，只认松手）。托盘可选 5/10/15/30。
static SILENCE_SEC: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(15);
/// 悬浮条开关。**默认关** —— 用户的文字本来就逐字上屏到光标处，
/// 悬浮条再显示一遍是重复信息，还挡视线。
/// 悬浮显示方式：0=关 1=细条 2=悬浮球 3=屏幕边缘光。**互斥** —— 一次只显示一个。
/// 用 u32 而不是 bool：用户要的是"细条/球/边缘光/都不显示"四选一。
static OVERLAY_MODE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// 当前热键的配置写法（托盘菜单打勾用）。候选表里都是 &'static str，直接存引用即可。
static CUR_HOTKEY: std::sync::Mutex<&'static str> = std::sync::Mutex::new("rightctrl");

/// 把按键名变成人话（托盘标题、提示里用）。
pub fn hotkey_label(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "rightctrl" | "rctrl" => "右Ctrl".into(),
        "leftctrl" | "lctrl" => "左Ctrl".into(),
        "rightalt" | "ralt" => "右Alt".into(),
        "leftalt" | "lalt" => "左Alt".into(),
        "rightshift" | "rshift" => "右Shift".into(),
        "leftshift" | "lshift" => "左Shift".into(),
        other => other.to_ascii_uppercase(),
    }
}

fn clip(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

/// 控制台横幅的显示宽度（中日韩全角字符占 2 列，其余占 1 列）。
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let w = matches!(c as u32,
                0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3
                | 0xF900..=0xFAFF | 0xFE30..=0xFE6F | 0xFF00..=0xFF60
                | 0xFFE0..=0xFFE6 | 0x20000..=0x3FFFD);
            if w { 2 } else { 1 }
        })
        .sum()
}

/// 按显示宽度补空格，让横幅的右边框对齐（宽度不够就原样返回）。
fn pad_display(s: &str, width: usize) -> String {
    let pad = width.saturating_sub(display_width(s));
    format!("{s}{}", " ".repeat(pad))
}

/// 从可执行文件位置往上找仓库根（认 tools/asr_sidecar.py）。
fn find_root() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let mut d = exe.parent().map(Path::to_path_buf);
        for _ in 0..6 {
            let Some(cur) = d else { break };
            if cur.join("tools").join("asr_sidecar.py").exists() {
                return cur;
            }
            d = cur.parent().map(Path::to_path_buf);
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn resolve_path(root: &Path, p: &str) -> PathBuf {
    let pb = PathBuf::from(p);
    if pb.is_absolute() {
        pb
    } else {
        root.join(pb)
    }
}

/// 找一个能用的 Python（双击 .bat 启动时 PATH 可能不含 Python 目录）。
///
/// 优先级：环境变量 `SAINPUT_PYTHON`（非空则直接用）→ PATH 里的
/// python / python3 / py → 兜底 `"python"`。
fn resolve_python() -> String {
    if let Ok(p) = std::env::var("SAINPUT_PYTHON") {
        let p = p.trim();
        if !p.is_empty() {
            return p.to_string();
        }
    }
    for c in ["python", "python3", "py"] {
        let ok = std::process::Command::new(c)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return c.to_string();
        }
    }
    "python".to_string()
}

// ------------------------------------------------- 配置：命令行 > sainput.conf > 默认
struct Args<'a> {
    cli: &'a [String],
    st: &'a Settings,
}

impl<'a> Args<'a> {
    fn s(&self, key: &str, cli_key: &str) -> Option<String> {
        opt(self.cli, cli_key).or_else(|| self.st.s(key))
    }
    fn b(&self, key: &str, on: &str, off: &str, dflt: bool) -> bool {
        if flag(self.cli, on) {
            return true;
        }
        if flag(self.cli, off) {
            return false;
        }
        self.st.b(key).unwrap_or(dflt)
    }
    fn f(&self, key: &str, cli_key: &str, dflt: f64) -> Result<f64> {
        match self.s(key, cli_key) {
            Some(v) => v
                .trim()
                .parse::<f64>()
                .with_context(|| format!("配置项 {key} 不是数字: {v}")),
            None => Ok(dflt),
        }
    }

    fn u(&self, key: &str, cli_key: &str, dflt: usize) -> Result<usize> {
        match self.s(key, cli_key) {
            Some(v) => v
                .trim()
                .parse::<usize>()
                .with_context(|| format!("配置项 {key} 不是整数: {v}")),
            None => Ok(dflt),
        }
    }
}

struct Cfg {
    python: PathBuf,
    script: PathBuf,
    model_dir: PathBuf,
    punct_dir: Option<PathBuf>,
    hotwords: Option<PathBuf>,
    threads: u32,
    beam: bool,
    itn: bool,
    throttle_ms: u32,
    rule1: f64,
    rule2: f64,
    rule3: f64,
    /// 标点策略：keep（默认，原样）| space（中文标点转空格，保留问号）
    punct_mode: String,
    /// 英文拆字合并开关（默认开：'R E A' -> 'REA'）
    merge_terms: bool,
    /// 上下文偏置用的临时热词文件（由光标前文生成，每句可能不同；不存在就跳过）
    hotwords_extra: Option<PathBuf>,
    /// 上下文偏置总开关（`ctx_bias`）。
    ctx_bias: bool,
    /// 英文大小写修复(`fix_case`)。
    fix_case: bool,
    /// 上屏方式策略(`ime_wmchar`):auto(默认)| on | off。
    /// 目标窗口挂着输入法时,注入的字母会被输入法吃掉(中文不受影响),这时改用
    /// PostMessage(WM_CHAR) 绕开键盘那条链。见 `sainput_core::inject::decide_key_mode`。
    ime_wmchar: String,
}

impl Cfg {
    fn asr_args(&self) -> Vec<String> {
        let mut v = vec![
            "--model-dir".to_string(),
            self.model_dir.display().to_string(),
            "--threads".to_string(),
            self.threads.to_string(),
            "--throttle-ms".to_string(),
            self.throttle_ms.to_string(),
        ];
        if let Some(p) = &self.punct_dir {
            v.push("--punct-dir".to_string());
            v.push(p.display().to_string());
        }
        if let Some(h) = &self.hotwords {
            v.push("--hotwords".to_string());
            v.push(h.display().to_string());
        }
        if self.beam {
            v.push("--beam".to_string());
        }
        if !self.itn {
            v.push("--no-itn".to_string());
        }
        if self.punct_mode != "keep" {
            v.push("--punct-mode".to_string());
            v.push(self.punct_mode.clone());
        }
        if !self.merge_terms {
            v.push("--no-merge-terms".to_string());
        }
        if !self.fix_case {
            v.push("--no-fix-case".to_string());
        }
        // 注意：**不能**在这里判 `extra.exists()` —— 这个文件是 run 主循环在第一句开始时
        // 才创建的，而 asr_args 是在 spawn（更早）时调的。早期判存在会让这个参数永远传不出去，
        // 偏置就静默失效了。sidecar 那边对"文件不存在"是容忍的（返回 None）。
        if let Some(extra) = &self.hotwords_extra {
            v.push("--hotwords-extra".to_string());
            v.push(extra.display().to_string());
        }
        v.push("--rule1".to_string());
        v.push(self.rule1.to_string());
        v.push("--rule2".to_string());
        v.push(self.rule2.to_string());
        v.push("--rule3".to_string());
        v.push(self.rule3.to_string());
        v
    }

    fn spawn(&self) -> Result<Sidecar> {
        let mut s = Sidecar::spawn(&self.python, &self.script, &self.asr_args())?;
        s.wait_ready(Duration::from_secs(300))?;
        Ok(s)
    }

    fn describe(&self) {
        println!(
            "[sainput] 模型={}  标点={}  ITN={}  热词={}",
            self.model_dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            match &self.punct_dir {
                Some(_) => "开",
                None => "关",
            },
            if self.itn { "开" } else { "关" },
            match &self.hotwords {
                Some(h) => h.display().to_string(),
                None => "无".to_string(),
            }
        );
    }
}

fn cfg(a: &Args) -> Result<Cfg> {
    let root = find_root();

    let model_dir = resolve_path(
        &root,
        &a.s("model_dir", "--model-dir")
            .unwrap_or_else(|| format!("models/{DEFAULT_MODEL}")),
    );
    if !model_dir.is_dir() {
        anyhow::bail!(
            "找不到模型目录: {}\n提示：把模型放到 {}\\models\\，或用 --model-dir 指定",
            model_dir.display(),
            root.display()
        );
    }

    // 标点：默认开，目录不存在就明确说一声然后关掉
    let punct_dir = if a.b("punct", "--punct", "--no-punct", true) {
        let p = resolve_path(
            &root,
            &a.s("punct_dir", "--punct-dir")
                .unwrap_or_else(|| format!("models/{DEFAULT_PUNCT}")),
        );
        if p.is_dir() {
            Some(p)
        } else {
            eprintln!(
                "[sainput] 标点模型不在 {} —— 本次不加标点",
                p.display()
            );
            None
        }
    } else {
        None
    };

    // 热词：命令行/配置指定的一定要存在，否则报错；默认位置存在就用
    let hotwords = match a.s("hotwords", "--hotwords") {
        Some(h) => {
            let p = resolve_path(&root, &h);
            if p.is_file() {
                Some(p)
            } else {
                anyhow::bail!("热词文件不存在: {}", p.display());
            }
        }
        None => {
            let p = root.join(HOTWORDS_NAME);
            if p.is_file() {
                Some(p)
            } else {
                None
            }
        }
    };

    let script = match a.s("script", "--script") {
        Some(p) => resolve_path(&root, &p),
        None => root.join("tools").join("asr_sidecar.py"),
    };
    if !script.exists() {
        anyhow::bail!("找不到 sidecar 脚本: {}", script.display());
    }
    let python = match a.s("python", "--python") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(resolve_python()),
    };

    // 上下文偏置总开关：run 用它决定要不要写偏置文件，sidecar 用它决定要不要挂 per-stream 热词。
    let ctx_bias = a.b("ctx_bias", "--ctx-bias", "--no-ctx-bias", true);

    Ok(Cfg {
        python,
        script,
        model_dir,
        punct_dir,
        hotwords,
        threads: a.u("threads", "--threads", 2)? as u32,
        beam: a.b("beam", "--beam", "--no-beam", false),
        itn: a.b("itn", "--itn", "--no-itn", true),
        throttle_ms: a.u("throttle_ms", "--throttle-ms", 80)? as u32,
        // 断句门槛：1.5s 太小 —— 用户"说着说着想很久"会被切碎成好几句。
        // 拉到 10s（用户明确要求）。代价：说完一句后要等 10s 才落定稿，
        // 但松手（finish）仍然立刻定稿，所以正常使用不受影响 ——
        // 只有"按住不放、中途停顿"的情况才走这条兜底。
        rule1: a.f("rule1", "--rule1", 15.0)?,
        rule2: a.f("rule2", "--rule2", 15.0)?,
        rule3: a.f("rule3", "--rule3", 20.0)?,
        punct_mode: a
            .s("punct_mode", "--punct-mode")
            .unwrap_or_else(|| "keep".to_string())
            .to_ascii_lowercase(),
        merge_terms: a.b("merge_terms", "--merge-terms", "--no-merge-terms", true),
        fix_case: a.b("fix_case", "--fix-case", "--no-fix-case", true),
        ime_wmchar: a
            .s("ime_wmchar", "--ime-wmchar")
            .unwrap_or_else(|| "auto".to_string())
            .to_ascii_lowercase(),
        ctx_bias,
        // 上下文偏置的临时热词文件：默认放仓库根下 .ctx_hotwords.txt，由 run 主循环写。
        // 关掉偏置时给 None —— 这样 sidecar 连 per-stream 热词都不会去碰。
        hotwords_extra: if ctx_bias {
            a.s("ctx_hotwords", "--ctx-hotwords")
                .map(|p| resolve_path(&find_root(), &p))
                .or_else(|| Some(find_root().join(".ctx_hotwords.txt")))
        } else {
            None
        },
    })
}

fn args_of() -> (Settings, PathBuf) {
    let root = find_root();
    let st = Settings::load(&root.join(CONF_NAME));
    (st, root)
}

// ---------------------------------------------------------------- devices
fn cmd_devices() -> Result<()> {
    let mics = audio::list_mics()?;
    if mics.is_empty() {
        println!("没有找到输入设备");
        return Ok(());
    }
    println!("{:<3} {:<46} {:>3} {:>7} {:<10}", "#", "NAME", "CH", "RATE", "FORMAT");
    for (i, m) in mics.iter().enumerate() {
        println!(
            "{:<3} {:<46} {:>3} {:>7} {:<10}{}",
            i,
            clip(&m.name, 46),
            m.channels,
            m.sample_rate,
            m.sample_format,
            if m.is_default { "  [默认]" } else { "" }
        );
    }
    Ok(())
}

// ------------------------------------------------------------ probe/record
fn report_stats(cap: &audio::Capture) {
    println!(
        "[stats] 出帧={} 原始样本={} 丢帧={} 流错误={}",
        cap.stats.frames_out.load(Ordering::Relaxed),
        cap.stats.samples_in.load(Ordering::Relaxed),
        cap.stats.frames_dropped.load(Ordering::Relaxed),
        cap.stats.stream_errors.load(Ordering::Relaxed),
    );
}

fn cmd_record(args: &[String], a: &Args) -> Result<()> {
    let out = args
        .iter()
        .find(|x| !x.starts_with("--"))
        .ok_or_else(|| anyhow::anyhow!("缺少输出路径：sainput record <out.wav>"))?
        .clone();
    let seconds = match a.s("seconds", "--seconds") {
        Some(v) => v.trim().parse::<f64>()?,
        None => 5.0,
    };
    let device = a.s("device", "--device");
    let cap = audio::start(device.as_deref())?;
    println!(
        "[record] 设备={} 源={}Hz/{}ch → {}Hz/mono {:.1}s",
        cap.device_name,
        cap.src_sample_rate,
        cap.src_channels,
        audio::TARGET_RATE,
        seconds
    );
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let mut samples = Vec::new();
    while Instant::now() < deadline {
        match cap.rx.recv_timeout(Duration::from_millis(250)) {
            Ok(f) => samples.extend_from_slice(&f),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    wav::write_pcm16_mono(&out, audio::TARGET_RATE, &samples)?;
    println!(
        "[record] 写出 {} 样本 / {:.2}s → {}",
        samples.len(),
        samples.len() as f64 / audio::TARGET_RATE as f64,
        out
    );
    report_stats(&cap);
    Ok(())
}

fn cmd_probe(a: &Args) -> Result<()> {
    let seconds = match a.s("seconds", "--seconds") {
        Some(v) => v.trim().parse::<f64>()?,
        None => 10.0,
    };
    let device = a.s("device", "--device");
    let cap = audio::start(device.as_deref())?;
    println!(
        "[probe] 设备={} 源={}Hz/{}ch → {}Hz/mono  期望帧间隔={}ms",
        cap.device_name,
        cap.src_sample_rate,
        cap.src_channels,
        audio::TARGET_RATE,
        audio::FRAME_MS
    );
    println!("[probe] 开始采集，请正常说话……");
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let mut last: Option<Instant> = None;
    let mut deltas: Vec<f64> = Vec::new();
    while Instant::now() < deadline {
        match cap.rx.recv_timeout(Duration::from_millis(250)) {
            Ok(_) => {
                let now = Instant::now();
                if let Some(p) = last {
                    deltas.push((now - p).as_secs_f64() * 1000.0);
                }
                last = Some(now);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    match metrics::summarize(deltas) {
        Some(s) => {
            let e = audio::FRAME_MS as f64;
            println!("[probe] 帧间隔(ms): {s}");
            println!(
                "[probe] 相对期望 {e:.0}ms 的偏差: p50={:+.1}ms p95={:+.1}ms max={:+.1}ms",
                s.p50 - e,
                s.p95 - e,
                s.max - e
            );
            if s.max - e > 100.0 {
                println!("[probe] ⚠ 最大间隔超标，采集路径有卡顿");
            }
        }
        None => println!("[probe] 没有采到任何帧"),
    }
    report_stats(&cap);
    Ok(())
}

// ------------------------------------------------------------------ 上屏
/// 悬浮条当前是否该显示（托盘开关的实时值）。
fn overlay_enabled() -> bool {
    OVERLAY_MODE.load(std::sync::atomic::Ordering::Relaxed) != 0
}

fn overlay_mode() -> u32 {
    OVERLAY_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

/// 当前边缘光颜色的菜单序号（0=白 1=彩虹 2=暖橙 3=青蓝 4=极光）。
fn tint_code_of() -> usize {
    match sainput_core::overlay::edge_tint() {
        EdgeTint::White => 0,
        EdgeTint::Rainbow => 1,
        EdgeTint::Amber => 2,
        EdgeTint::Cyan => 3,
        EdgeTint::Aurora => 4,
    }
}

/// 全局悬浮条槽位。
///
/// 为什么是全局而不是从 `RunOpts` 里传引用：托盘开关可以在**运行中**把窗口建起来，
/// 而 worker 手里的 `Option<&Overlay>` 是启动那一刻的快照 —— 建了它也看不见。
/// 槽位是唯一真源，worker 每次要用时现取。
static OV_SLOT: std::sync::Mutex<Option<Overlay>> = std::sync::Mutex::new(None);

fn ov_put(o: Option<Overlay>) {
    if let Ok(mut g) = OV_SLOT.lock() {
        *g = o;
    }
}

fn ov_get() -> Option<Overlay> {
    OV_SLOT.lock().ok().and_then(|g| g.clone())
}

/// 统一入口：只有开关打开时才碰悬浮条。
/// 为什么统一：以前 `if let Some(ov) = &o.overlay` 散落在 8 处，
/// 加一个运行时开关就得改 8 遍 —— 漏一处就会出现「关了还在闪」。
fn ov_show(_o: &Option<Overlay>, _status: &str, text: &str) {
    if !overlay_enabled() {
        return;
    }
    if let Some(ov) = ov_get() {
        // 细条**只显示正文**，状态行已撤（状态交给悬浮球）。
        ov.show_bar(text);
    }
}

/// 进入录音状态：按当前模式显示悬浮球或屏幕边缘光。
fn ov_record_start(_o: &Option<Overlay>) {
    if !overlay_enabled() {
        return;
    }
    if let Some(ov) = ov_get() {
        match overlay_mode() {
            2 => ov.show_orb(),
            3 => ov.show_edge(),
            // 细条模式：录音时先清空（还没有文字）
            _ => ov.show_bar(""),
        }
    }
}

/// 录音结束（松手）：边缘光淡出，球/细条保持到出文字。
fn ov_record_end(_o: &Option<Overlay>) {
    if !overlay_enabled() {
        return;
    }
    if overlay_mode() == 3 {
        if let Some(ov) = ov_get() {
            ov.fade_edge();
        }
    }
}

/// 推一帧音量给悬浮球。
fn ov_level(_o: &Option<Overlay>, level: f32) {
    if !overlay_enabled() {
        return;
    }
    if let Some(ov) = ov_get() {
        ov.push_level(level);
    }
}

fn ov_hide(_o: &Option<Overlay>) {
    if let Some(ov) = ov_get() {
        ov.hide();
    }
}

fn ov_hide_after(_o: &Option<Overlay>, ms: u64) {
    if let Some(ov) = ov_get() {
        ov.hide_after(ms);
    }
}

struct Ui<'a> {
    ov: Option<&'a Overlay>,
    log: Option<&'a Logger>,
    dry: bool,
    max_bs: usize,
    recording: bool,
    t0: Instant,
}

fn handle_msg(m: Msg, wr: &mut inject::StreamWriter, ui: &Ui) {
    let ms = ui.t0.elapsed().as_millis();
    match m {
        Msg::Ready {
            model,
            decoding,
            punct,
            itn,
            hotwords,
        } => {
            println!(
                "[asr] 就绪 model={model} decoding={decoding} 标点={} ITN={} 热词={hotwords}",
                if punct { "开" } else { "关" },
                if itn { "开" } else { "关" }
            );
        }
        Msg::Partial { text, raw, ms: rms } => {
            if let Some(l) = ui.log {
                l.line(&format!("P {ms:6}ms  {text}   [raw] {raw}  ({rms:.1}ms)"));
            }
            if ui.dry {
                println!("[{ms:6}ms] partial: {text}   (渲染 {rms:.1}ms | 原文 {raw})");
            } else {
                // 只有「细条」模式才用文字更新悬浮窗；球模式保持球（波形照走）。
                if overlay_mode() == 1 {
                    if let Some(ov) = ov_get() {
                        ov.show_bar(&text);
                    }
                }
                if let Err(e) = wr.update(&text) {
                    eprintln!("[inject] partial 失败: {e}");
                }
            }
        }
        Msg::Final { text, raw, reason } => {
            if let Some(l) = ui.log {
                l.line(&format!("F {ms:6}ms  ({reason})  {text}   [raw] {raw}"));
            }
            if ui.dry {
                println!("[{ms:6}ms] FINAL({reason}): {text}");
                if raw != text {
                    println!("[{ms:6}ms]   ASR 原文: {raw}");
                }
            } else {
                // 空 final 绝不能当成「把屏幕清掉」——只更新内部状态。
                if !text.is_empty() {
                    if let Err(e) = wr.finalize(&text, ui.max_bs * 4) {
                        eprintln!("[inject] final 失败: {e}");
                    }
                }
                if overlay_mode() == 1 {
                    if let Some(ov) = ov_get() {
                        if text.is_empty() {
                            if reason == "finish" {
                                ov.hide();
                            }
                        } else {
                            ov.show_bar(&text);
                            ov.hide_after(1100);
                        }
                    }
                }
                // 控制台也给一行回执，不然用户只能靠猜（overlay 是给眼睛看的，
                // 日志是给排查用的）
                if !text.is_empty() {
                    println!("[sainput] ✔ {} {}", reason, clip(&text, 40));
                }
                // VAD 自动断句 = 这一句已经定稿。必须丢开跟踪，
                // 否则下一句的第一个 partial 会被当成"对上一句的修正"而退格掉它。
                if reason == "endpoint" {
                    wr.reset();
                }
            }
        }
        Msg::Rendered { .. } => {}
        Msg::ResetAck => {}
        Msg::Error { msg } => eprintln!("[asr] 错误: {msg}"),
    }
}

// ------------------------------------------------------------------ replay
fn cmd_replay(args: &[String], a: &Args) -> Result<()> {
    let path = args
        .iter()
        .find(|x| !x.starts_with("--"))
        .ok_or_else(|| anyhow::anyhow!("缺少 WAV：sainput replay <in.wav>"))?
        .clone();
    let c = cfg(a)?;
    let dry = flag(args, "--dry-run");
    let streaming = a.b("stream", "--stream", "--no-stream", true);
    let max_bs = a.u("max_backspace", "--max-backspace", 8)?;

    let mut side = c.spawn()?;
    let samples =
        wav::read_as_pcm16_mono_16k(&path).with_context(|| format!("读取 WAV 失败: {path}"))?;
    println!(
        "[replay] {}  {:.2}s  {} 样本  dry_run={} stream={}",
        Path::new(&path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        samples.len() as f64 / audio::TARGET_RATE as f64,
        samples.len(),
        dry,
        streaming
    );

    let mut wr = inject::StreamWriter::new(max_bs, streaming && !dry);
    let t0 = Instant::now();
    let ov = None;
    let mut buf: Vec<Msg> = Vec::new();

    for chunk in samples.chunks(audio::FRAME_SAMPLES) {
        side.send_audio(chunk)?;
        side.drain(&mut buf);
        for m in buf.drain(..) {
            let ui = Ui { ov, log: None, dry, max_bs, recording: true, t0 };
            handle_msg(m, &mut wr, &ui);
        }
        // 按采样的真实节奏喂（半速，够逼近实时又不至于太慢）
        std::thread::sleep(Duration::from_millis(audio::FRAME_MS as u64 / 2));
    }
    side.finish()?;

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut got_final = false;
    while Instant::now() < deadline && !got_final {
        side.drain(&mut buf);
        for m in buf.drain(..) {
            if matches!(m, Msg::Final { .. } | Msg::Partial { .. }) {
                // 只看 final 决定结束
            }
            let is_final = matches!(m, Msg::Final { .. });
            let ui = Ui { ov, log: None, dry, max_bs, recording: false, t0 };
            handle_msg(m, &mut wr, &ui);
            if is_final {
                got_final = true;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!(
        "[replay] 完成：上屏 {} 字，退格 {} 次，最终屏幕内容 = \"{}\"",
        wr.typed_chars,
        wr.backspaces,
        wr.injected()
    );
    Ok(())
}

// ------------------------------------------------------------------ render
fn cmd_render(args: &[String], a: &Args) -> Result<()> {
    // 只收真正的文本参数：跳过 --选项 以及它后面紧跟的值
    let mut text: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i].starts_with("--") {
            i += 1;
            if i < args.len() && !args[i].starts_with("--") {
                i += 1;
            }
            continue;
        }
        text.push(args[i].clone());
        i += 1;
    }
    let text = if text.is_empty() {
        "二零二五年三月十五日下午三点十五分开会我们讨论百分之三十七的增长".to_string()
    } else {
        text.join(" ")
    };
    let c = cfg(a)?;
    let mut side = c.spawn()?;
    println!(
        "[render] 模型={} 标点={} ITN={}",
        side.model,
        if side.punct { "开" } else { "关" },
        if side.itn { "开" } else { "关" }
    );
    println!("[render] 输入: {text}");
    side.send_render(&text)?;
    match side.wait_rendered(Duration::from_secs(20)) {
        Some(Msg::Rendered { text, ms }) => {
            println!("[render] 输出: {text}   ({ms:.1}ms)");
        }
        _ => eprintln!("[render] 没等到结果"),
    }
    Ok(())
}

// ------------------------------------------------------------------ config
fn cmd_config(a: &Args, root: &Path) -> Result<()> {
    let c = cfg(a)?;
    println!("仓库根    : {}", root.display());
    println!(
        "配置文件  : {}",
        a.st
            .source
            .clone()
            .unwrap_or_else(|| format!("(无，可新建 {})", root.join(CONF_NAME).display()))
    );
    if !a.st.is_empty() {
        println!("配置项    : {}", a.st.keys().join(", "));
    }
    println!("Python    : {}", c.python.display());
    println!("sidecar   : {}", c.script.display());
    println!("ASR 模型  : {}", c.model_dir.display());
    println!(
        "标点模型  : {}",
        c.punct_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(关闭)".into())
    );
    println!(
        "热词表    : {}",
        c.hotwords
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(无)".into())
    );
    println!("线程数    : {}", c.threads);
    println!("beam      : {}", if c.beam { "开" } else { "关" });
    println!("ITN       : {}", if c.itn { "开" } else { "关" });
    println!(
        "标点策略  : {}",
        if c.punct_mode == "space" {
            "space（中文标点转空格，问号保留）"
        } else {
            "keep（原样）"
        }
    );
    println!("英文合并  : {}", if c.merge_terms { "开" } else { "关" });
    // 上屏方式:把原始策略原样打出来 —— 名单形式(on:pebrel.exe)光看标签看不出是谁
    let (label, list) = match c.ime_wmchar.split_once(':') {
        Some((h, l)) => (h.trim(), l.trim()),
        None => (c.ime_wmchar.trim(), ""),
    };
    let label = match label {
        "on" | "wmchar" => "消息通道(流式)",
        "nostream" | "final" => "消息通道(整句一次,不流式)",
        "off" | "keys" | "input" => "键盘注入(强制)",
        _ => "auto(输入法吃字母的窗口改走消息通道)",
    };
    if list.is_empty() {
        println!("上屏方式  : {label}");
    } else {
        println!("上屏方式  : {label},只对 {list}");
    }
    println!("上下文偏置: {}", if c.ctx_bias { "开" } else { "关" });
    println!("渲染节拍  : {}ms", c.throttle_ms);
    {
        let t = tail_of(a)?;
        println!(
            "尾巴      : 松手后最少收 {}ms / 安静 {}ms 收工 / 上限 {}ms / 补 {}ms 底噪（RMS {:.3}）",
            t.min_ms, t.quiet_ms, t.max_ms, t.pad_ms, t.rms
        );
    }
    println!(
        "热键      : {}",
        a.s("hotkey", "--hotkey").unwrap_or_else(|| "rightctrl".into())
    );
    println!(
        "说话方式  : {}",
        if a.b("toggle", "--toggle", "--hold", false) {
            "按一下开始/再按一下结束"
        } else {
            "按住说话"
        }
    );
    println!(
        "流式上屏  : {}",
        if a.b("stream", "--stream", "--no-stream", true) {
            "开"
        } else {
            "关"
        }
    );
    let om = match a.s("overlay_mode", "--overlay-mode").as_deref() {
        Some("bar") => 1,
        Some("orb") => 2,
        Some("edge") => 3,
        Some("off") => 0,
        _ => {
            if a.b("overlay", "--overlay", "--no-overlay", false) { 1 } else { 3 }
        }
    };
    println!(
        "悬浮显示  : {}",
        match om {
            1 => "细条（只显示文字，半透明）",
            2 => "悬浮球（录音波形）",
            3 => "屏幕边缘光",
            _ => "关",
        }
    );
    println!(
        "麦克风    : {}",
        match a.b("lazy_mic", "--lazy-mic", "--warm-mic", false) {
            true => "按需开麦",
            false => "常驻预热",
        }
    );
    println!("sidecar   : {}", c.asr_args().join(" "));
    Ok(())
}


// ---------------------------------------------------------------- 文件日志
/// 把这次会话的识别原文 / 渲染结果 / 上屏动作写进文件。
/// 用户报「吞字」「慢」这类问题时，只有拿到真实会话记录才谈得上定位。
pub struct Logger {
    file: std::sync::Mutex<std::fs::File>,
    path: String,
}

impl Logger {
    pub fn open(dir: &Path) -> Option<Logger> {
        std::fs::create_dir_all(dir).ok()?;
        let name = format!("sainput-{}.log", chrono_like_stamp());
        let path = dir.join(name);
        let f = std::fs::File::create(&path).ok()?;
        Some(Logger {
            file: std::sync::Mutex::new(f),
            path: path.display().to_string(),
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn line(&self, s: &str) {
        use std::io::Write;
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{s}");
        }
    }
}

/// 本地时间戳。不引 chrono，直接问 Windows 要（GetLocalTime 返回的就是本地时间）。
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    ms: u16,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLocalTime(st: *mut SystemTime);
}

fn chrono_like_stamp() -> String {
    let mut st = SystemTime::default();
    unsafe { GetLocalTime(&mut st) };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        st.year, st.month, st.day, st.hour, st.minute, st.second
    )
}

// --------------------------------------------------------------------- run
struct RunOpts {
    /// 测试用：用一段 WAV 顶替麦克风，走**完全相同的** run 路径。
    /// 没有它就只能靠「放音箱 + 麦克风收音」，不可复现。
    feed: Option<Vec<i16>>,
    streaming: bool,
    max_backspace: usize,
    device: Option<String>,
    toggle: bool,
    lazy_mic: bool,
    overlay: Option<Overlay>,
    log: Option<Logger>,
    /// 松键之后还要收多久的尾巴（吞字 / 跟手感的取舍都在这里）。
    tail: TailCfg,
    /// 测试用：喂到第几秒音频时自动「松键」（模拟松手时尾巴还在路上）。
    feed_release: Option<f64>,
    /// 上下文偏置的临时热词文件（每句开始写一次；None = 关闭偏置）。
    ctx_hotwords: Option<PathBuf>,
    /// 偏置生效时打一行提示（默认安静）。
    verbose_ctx: bool,
    /// feed 测试:每句从 wav 头重放(测两句以上的场景必需)。
    feed_restart: bool,
    /// 上屏方式策略(`ime_wmchar`):每句开始时按目标窗口重新决定。
    ime_wmchar: String,
}

/// 松键之后还要继续喂多久的「尾巴」音频。
///
/// 为什么需要：松键那一刻，麦克风里往往还有最后几个字没送进模型 ——
/// 无线麦自己的缓冲 + Windows 采集缓冲（本机实测 30~40ms 一块）
/// + 模型 160ms 分块需要右上下文才能把最后一个字吐出来。
/// 立刻 finish 就等于把这几个字扔掉，用户实测到的「吞尾/吞字」就是这么来的。
#[derive(Clone, Copy, Debug)]
struct TailCfg {
    /// 最短尾巴：松键后至少再喂这么多毫秒的音频。
    min_ms: u32,
    /// 最长尾巴：还在说话 / 模型还在吐字也不能无限等。
    max_ms: u32,
    /// 收工条件之一：尾巴里连续安静这么多毫秒。
    /// 松手之后用户还可能补半句（甚至只是喘口气），安静门限太短会把话切掉，
    /// 太长又拖慢落句尾 —— 250ms 是实测折中（真人句中的换气一般 150~350ms）。
    quiet_ms: u32,
    /// 收工时一次性灌进去的静音补偿（毫秒）。
    /// 实测这个模型有右上下文：最后一个字要等它「听到」后面约 480ms 音频才吐得出来，
    /// 真等就得再喂 640ms 实时音频；改成一次性灌底噪只要几十毫秒。
    pad_ms: u32,
    /// 安静判定阈值（帧 RMS，0.0~1.0）。
    /// 本机 DJI MIC MINI 房间底噪实测 p50=0.0028 / p99=0.0102，所以默认 0.012。
    rms: f32,
}

/// 一句话的生命周期。文字只有在离开 Idle 之后才允许上屏。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    /// 空闲：等热键
    Idle,
    /// 正在录音
    Recording,
    /// 松键了，还在把尾巴喂进模型
    Tail,
    /// 尾巴收完、finish 已发出，等这一句的 final 回来
    AwaitFinal,
}

/// 松键之后仍然在往模型里喂音频的那一小段时间。
struct Tail {
    start: Instant,
    /// 模型最近一次吐出「新字」的时刻（尾巴够不够长主要看它）。
    last_text_at: Instant,
    /// 到点之后才允许因为「已经安静了」提前结束。
    until_min: Instant,
    /// 硬上限。
    until_max: Instant,
    /// 尾巴里最近连续安静了多久（毫秒）。
    quiet: f64,
    /// 安静帧的 RMS 之和 / 帧数：静音补偿就按这个量级的底噪灌。
    quiet_rms_sum: f64,
    quiet_frames: usize,
    /// 尾巴里喂进去的真音频帧数（诊断用）。
    voiced_frames: usize,
    /// 尾巴里最大的帧 RMS（诊断用）。
    rms_max: f32,
    /// 收工那一刻「模型已经多久没吐新字」（诊断用）。
    text_idle_at_end: f64,
}

/// feed 模式的喂入状态（--feed 是确定性测试入口）。
struct FeedState {
    pos: usize,
    silence: usize,
    next: Option<Instant>,
    /// `--feed-restart`：每次新起一句时把读指针归零（重放整个 wav）。
    /// 没有它就没法测**任何两句以上**的场景 —— 文件放完就只剩静音，
    /// 第二句永远是空的（上下文偏置 / 连说两句的回归都需要这个）。
    restart: bool,
}

impl FeedState {
    /// 新起一句时调用：按需把读指针归零，让 wav 从头重放。
    fn begin_utterance(&mut self) {
        if self.restart {
            self.pos = 0;
            self.silence = 0;
            self.next = None;
        }
    }
}

/// feed 模式：按真实 32ms 节拍最多吐一帧；文件放完继续吐静音让 VAD 自然端点。
fn feed_tick(o: &RunOpts, st: &mut FeedState) -> Vec<Vec<i16>> {
    let Some(f) = &o.feed else {
        return Vec::new();
    };
    let now = Instant::now();
    if st.next.map(|t| now >= t).unwrap_or(true) {
        st.next = Some(now + Duration::from_millis(audio::FRAME_MS as u64));
        if st.pos < f.len() {
            let end = (st.pos + audio::FRAME_SAMPLES).min(f.len());
            let fr = f[st.pos..end].to_vec();
            st.pos = end;
            return vec![fr];
        }
        if st.silence < 140 {
            st.silence += 1;
            return vec![vec![0i16; audio::FRAME_SAMPLES]];
        }
    }
    Vec::new()
}

/// 麦克风模式：把当前已经到达的帧全部取走（非阻塞）。
fn drain_mic(cap: &Option<audio::Capture>) -> Vec<Vec<i16>> {
    let mut out = Vec::new();
    if let Some(c) = cap {
        while let Ok(f) = c.rx.try_recv() {
            out.push(f);
        }
    }
    out
}

/// 采集路径至今被丢掉的帧数（诊断用）。
///
/// 为什么要有它：`audio::Capture` 的通道是**有界**的，`try_send` 满了就丢帧并
/// 累加 `frames_dropped`，但那个计数以前只有 `probe` 子命令会打印 —— run 模式里
/// 真丢了帧，日志上一点痕迹都没有。排查「吞字」第一个要回答的就是「音频丢没丢」，
/// 所以把尾巴那一行 T 带上它。没有采集会话（feed 模式/按需麦）时返回 0。
fn dropped_now(cap: &Option<audio::Capture>) -> u64 {
    cap.as_ref()
        .map(|c| c.stats.frames_dropped.load(Ordering::Relaxed))
        .unwrap_or(0)
}

fn frame_rms(f: &[i16]) -> f32 {
    if f.is_empty() {
        return 0.0;
    }
    let s: f64 = f
        .iter()
        .map(|&x| {
            let v = x as f64 / 32768.0;
            v * v
        })
        .sum();
    (s / f.len() as f64).sqrt() as f32
}

/// 松键：不立刻 finish，改为进入「收尾巴」状态。
fn begin_tail(tail: &mut Option<Tail>, o: &RunOpts) {
    if tail.is_some() {
        return;
    }
    let c = o.tail;
    let now = Instant::now();
    *tail = Some(Tail {
        start: now,
        last_text_at: now,
        until_min: now + Duration::from_millis(c.min_ms as u64),
        until_max: now + Duration::from_millis(c.max_ms.max(c.min_ms) as u64),
        quiet: 0.0,
        quiet_rms_sum: 0.0,
        quiet_frames: 0,
        voiced_frames: 0,
        rms_max: 0.0,
        text_idle_at_end: 0.0,
    });
}

/// 尾巴阶段的一拍：把能拿到的音频喂进去，够安静了（或到硬上限）就 finish。
/// 返回值 = 这一拍里 finish 了。
fn tail_tick(
    side: &mut Sidecar,
    o: &RunOpts,
    cap: &Option<audio::Capture>,
    fst: &mut FeedState,
    t: &mut Tail,
    text_idle_ms: f64,
) -> bool {
    // text_idle_ms 只进日志（调参用），不参与收工判断：灌完底噪模型一定会吐字。
    let frames = if o.feed.is_some() {
        feed_tick(o, fst)
    } else {
        drain_mic(cap)
    };
    let mut n = 0usize;
    for fr in frames {
        let rms = frame_rms(&fr);
        let ms = fr.len() as f64 * 1000.0 / audio::TARGET_RATE as f64;
        if rms >= o.tail.rms {
            t.quiet = 0.0;
            t.voiced_frames += 1;
            if rms > t.rms_max {
                t.rms_max = rms;
            }
        } else {
            t.quiet += ms;
            t.quiet_rms_sum += rms as f64;
            t.quiet_frames += 1;
        }
        n += 1;
        let _ = side.send_audio(&fr);
    }
    let now = Instant::now();
    let quiet_ok = t.quiet >= o.tail.quiet_ms as f64;
    if now >= t.until_max || (now >= t.until_min && quiet_ok) {
        finish_with_pad(side, o, t, text_idle_ms);
        return true;
    }
    if n == 0 {
        std::thread::sleep(Duration::from_millis(4));
    }
    false
}

/// 收尾定稿：**先补底噪**（按当前环境实测底噪灌 `tail_pad_ms`），再 `finish`。
///
/// 为什么必须合成一个函数：这个模型有右上下文，最后一个字要等它「听到」后面
/// 约 480ms 音频才肯吐；而且补的必须是**底噪不是纯 0**（纯 0 实测会让模型多吐一个
/// 虚词，见 FROZEN.md 第 6 节第 2 条）。也就是说「补底噪」和「finish」是一个不可拆的
/// 整体 —— 任何一条收尾路径漏掉补底噪，就会退回吞字行为。
///
/// 今天有两条收尾路径：正常尾巴到点（`tail_tick`）和**尾巴没收完又按下热键**（打断）。
/// 后者原本直接 `finish()` 没补底噪，等于在打断这条路上把冻结的修复又丢了。
fn finish_with_pad(side: &mut Sidecar, o: &RunOpts, t: &mut Tail, text_idle_ms: f64) {
    let rms = if t.quiet_frames > 0 {
        (t.quiet_rms_sum / t.quiet_frames as f64) as f32
    } else {
        0.003
    };
    t.text_idle_at_end = text_idle_ms;
    let _ = side.pad_silence(o.tail.pad_ms, rms.max(0.0005));
    let _ = side.finish();
}

/// 把尾巴的实测数字写进会话日志（调参就靠它）。
/// `dropped` = **本次采集会话至今**被丢掉的帧数（见 audio::CaptureStats::frames_dropped）。
/// 注意它不是「本句」的：默认常驻预热下 `cap` 从启动起就一直在，所以这个数是整段会话累计；
/// 只有 `--lazy-mic`（松手就释放麦克风）才是每句重新起算。
/// 以前这个数字只在 `probe` 子命令里打印，run 模式里丢了帧也看不出来 ——
/// 「吞字」排查的第一个问题就是「音频到底丢没丢」，不能没有它。
fn report_tail(o: &RunOpts, t: &Tail, abs_ms: u128, why: &str, dropped: u64) {
    let dur = t.start.elapsed().as_millis();
    let quiet_rms = if t.quiet_frames > 0 {
        t.quiet_rms_sum / t.quiet_frames as f64
    } else {
        0.0
    };
    if let Some(l) = &o.log {
        l.line(&format!(
            "T {abs_ms:6}ms  松键→收尾 {dur}ms | 尾巴真音频 {} 帧 | 连续安静 {:.0}ms | 底噪 RMS {:.4} | 补 {}ms 底噪 | 模型停吐 {:.0}ms | 峰值 RMS {:.4} | 累计丢帧 {dropped} | {why}",
            t.voiced_frames, t.quiet, quiet_rms, o.tail.pad_ms, t.text_idle_at_end, t.rms_max
        ));
    }
    println!(
        "[sainput] ~ 尾巴 {dur}ms（真音频 {} 帧 / 安静 {:.0}ms / 底噪 {:.4}）→ 补 {}ms 底噪后定稿（累计丢帧 {dropped}）",
        t.voiced_frames, t.quiet, quiet_rms, o.tail.pad_ms
    );
}

fn worker(
    rx: mpsc::Receiver<hotkey::HookEvent>,
    mut side: Sidecar,
    o: RunOpts,
    ctl: mpsc::Receiver<String>,
) -> Result<()> {
    let mut wr = inject::StreamWriter::new(o.max_backspace, o.streaming);
    let mut cap: Option<audio::Capture> = None;
    let mut recording = false;
    let mut tail: Option<Tail> = None;
    // 这一句走到哪一步了。Idle 之后到达的 partial / final 一律丢掉：
    // 模型偶尔会把静音「听」出字来，落到屏幕上就是垃圾。
    let mut stage = Stage::Idle;
    // 上一句的 final 还在路上就又按下了热键：这时不能 wr.reset()，
    // 否则那条 final 会因为「屏幕上什么都没有」而把整句重打一遍。
    let mut defer_reset = false;
    let mut buf: Vec<Msg> = Vec::new();
    let t0 = Instant::now();
    let mut utterances = 0usize;
    let mut fst = FeedState {
        pos: 0,
        silence: 0,
        next: None,
        restart: o.feed_restart,
    };
    // 模型最近一次吐出「新字」的时刻：尾巴要不要继续等，看的就是它。
    let mut last_text_at = Instant::now();
    // 上下文偏置：记住刚打出去的原文，下一句开始时抽成热词喂给识别器。
    // 喂的是自己刚发出去的原文（云端方案也是这么做的）。
    let mut history = context::SendHistory::new();

    // 默认常驻预热：否则「按下热键 → 开麦」要 100ms 左右，第一个字会被吞掉。
    // feed 模式不需要麦克风，直接跳过。
    if o.feed.is_none() && !o.lazy_mic {
        match audio::start(o.device.as_deref()) {
            Ok(c) => cap = Some(c),
            Err(e) => eprintln!("[sainput] 麦克风预热失败，改为按需开麦: {e}"),
        }
    }

    loop {
        // 1) 热键
        match rx.try_recv() {
            Ok(hotkey::HookEvent::Down) => {
                let start = if o.toggle { !recording } else { true };
                if start {
                    // 上一句的尾巴还在收就又要说下一句：先把尾巴收干净再开新的，
                    // 否则两句音频会混进同一个 stream。
                    if tail.is_some() {
                        for fr in drain_mic(&cap) {
                            let _ = side.send_audio(&fr);
                        }
                        for fr in feed_tick(&o, &mut fst) {
                            let _ = side.send_audio(&fr);
                        }
                        // 打断路径也必须补底噪再 finish —— 见 finish_with_pad 的注释：
                        // 漏掉它等于在「没收完就再按」这条路上退回吞字。
                        let text_idle_ms = last_text_at.elapsed().as_millis() as f64;
                        let dropped = dropped_now(&cap);
                        if let Some(t) = tail.as_mut() {
                            finish_with_pad(&mut side, &o, t, text_idle_ms);
                        }
                        if let Some(t) = tail.take() {
                            report_tail(&o, &t, t0.elapsed().as_millis(), "被下一次按下打断", dropped);
                        }
                        stage = Stage::AwaitFinal;
                    }
                    if o.feed.is_none() && cap.is_none() {
                        match audio::start(o.device.as_deref()) {
                            Ok(c) => cap = Some(c),
                            Err(e) => {
                                eprintln!("[sainput] 开麦失败: {e}");
                                continue;
                            }
                        }
                    }
                    recording = true;
                    if stage == Stage::AwaitFinal {
                        // 上一句的 final 还在路上：等它到了再 reset，
                        // 否则它会以为屏幕上什么都没有，把整句重打一遍。
                        defer_reset = true;
                    } else {
                        wr.reset();
                    }
                    // 前置空格：**每句开始时问一次**光标左边有没有字。
                    // ⚠️ 必须在 reset **之后**设 —— reset 会清掉前缀（前缀属于"上一句"的决策），
                    // 顺序反了就等于永远不补空格（我第一版就写反了）。
                    // 为什么只问一次：这一句的头定了就不该再变，中途重判会让已上屏的字被改来改去；
                    // 而且此刻用户还没说话，前台窗口/光标最稳定。
                    if LEADING_SPACE.load(std::sync::atomic::Ordering::Relaxed) {
                        let lc = caretctx::left_context();
                        wr.set_prefix(if lc.wants_leading_space() { " " } else { "" });
                    } else {
                        wr.set_prefix("");
                    }
                    // 上屏方式:①目标窗口挂着输入法时,注入的字母会被输入法当成它自己的
                    // 输入(掉进组字串、弹出候选面板,程序一个字都收不到),中文却不受影响
                    // —— 这就是「说英文它就被吃掉」。②输入法状态读不到也改不了(跨进程
                    // ImmGetContext 返回 NULL),所以对这类窗口改用 PostMessage(WM_CHAR)。
                    // 每句开始时重判一次:用户可能已经换了窗口。
                    let mode = inject::target_key_mode(&o.ime_wmchar);
                    if mode.is_wmchar() && !wr.mode().is_wmchar() {
                        println!(
                            "[sainput] 上屏改用 WM_CHAR:{} —— 这类窗口里注入的字母会被输入法吃掉",
                            inject::describe_target(mode)
                        );
                    }
                    wr.set_mode(mode);
                    // 上下文偏置：把「刚打出去的历史」抽成热词写盘，sidecar 在开新流时读。
                    // 必须在 reset 之前做（reset 之后 sidecar 才会重读文件）。
                    // 每句都问一次实时开关 —— 托盘里改了要立刻生效，不能要求重开。
                    let ctx_on = CTX_BIAS.load(std::sync::atomic::Ordering::Relaxed);
                    if let Some(p) = &o.ctx_hotwords {
                        if ctx_on {
                            let terms = history.terms();
                            let n = terms.len();
                            if let Err(e) = context::write_hotwords(p, &terms) {
                                eprintln!("[sainput] 上下文偏置写入失败（不影响识别）: {e}");
                            } else if n > 0 && VERBOSE_CTX.load(std::sync::atomic::Ordering::Relaxed) {
                                println!("[sainput] 上下文偏置: {n} 个词（来自最近 {} 句）", history.len());
                            }
                        } else {
                            // 关掉偏置时把文件清空，免得 sidecar 还在用上一轮的旧词
                            let _ = context::write_hotwords(p, &[]);
                        }
                    }
                    stage = Stage::Recording;
                    utterances += 1;
                    // feed 测试模式：新的一句从 wav 头重放（否则第二句只有静音）
                    fst.begin_utterance();
                    println!("[sainput] ● 第 {utterances} 句 开始采集");
                    // 录音中用**悬浮球**（只表示"正在听"+波形），细条留给识别出来的文字。
                    ov_record_start(&o.overlay);
                } else {
                    recording = false;
                    stage = Stage::Tail;
                    begin_tail(&mut tail, &o);
                    println!("[sainput] ○ 松开 —— 先把尾巴收完再识别……");
                    // 松手 = 不再听；边缘光从这里开始淡出
                    ov_record_end(&o.overlay);
                }
            }
            Ok(hotkey::HookEvent::Up) => {
                if !o.toggle && recording {
                    recording = false;
                    stage = Stage::Tail;
                    begin_tail(&mut tail, &o);
                    println!("[sainput] ○ 松开 —— 先把尾巴收完再识别……");
                    ov_record_end(&o.overlay);
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
        }

        // 2) 采集 → sidecar（录音时喂；松键后继续喂尾巴，喂够了才 finish）
        // 本轮采集流是否已断开：断开后必须丢掉 Capture，否则下一次按下只会
        // 立刻再次收到 Disconnected —— 表现为「按了没反应」，永远恢复不了。
        let mut cap_dead = false;
        if recording {
            if o.feed.is_some() {
                for fr in feed_tick(&o, &mut fst) {
                    ov_level(&o.overlay, frame_rms(&fr) * 6.0);
                    let _ = side.send_audio(&fr);
                }
                // 测试用：喂到指定位置就当用户松手了（和真按一下右 Ctrl 抬起等价）
                if let Some(rel) = o.feed_release {
                    let at = (rel * audio::TARGET_RATE as f64) as usize;
                    if fst.pos >= at {
                        let fed_ms = fst.pos as f64 * 1000.0 / audio::TARGET_RATE as f64;
                        recording = false;
                        stage = Stage::Tail;
                        begin_tail(&mut tail, &o);
                        println!("[sainput] ○ 测试：喂到 {fed_ms:.0}ms 自动松手 —— 收尾巴……");
                    }
                }
                std::thread::sleep(Duration::from_millis(4));
            } else if let Some(c) = &cap {
                // 每轮把队列**取空**，而不是只取一帧。
                //
                // 原写法（每轮一帧）在循环某轮变慢时会永久性落后：
                // 长句修正要先退格再重打，那一段是同步 SendInput，
                // 一旦耗时超过 32ms，麦克风队列就积压；
                // 之后仍然每轮只取一帧 —— 追不上，屏幕长时间不动，
                // 直到某轮偶然快了补上几帧，文字突然跳一大段。
                // 用户看到的就是「卡住，松手后一次全出来」。
                //
                // 上限 32 帧（约 1 秒音频）：防止极端积压时单轮阻塞太久，
                // 剩下的留给下一轮。
                let mut got = 0;
                let mut disconnected = false;
                loop {
                    match c.rx.try_recv() {
                        Ok(f) => {
                            // 悬浮球波形：真实音量驱动（不是装饰动画）。
                            // 麦克风原始 RMS 很小（正常说话 0.02~0.2），
                            // 乘 6 再钳到 0..1，让"正常说话"能占到波形高度的 6~7 成。
                            ov_level(&o.overlay, frame_rms(&f) * 6.0);
                            let _ = side.send_audio(&f);
                            got += 1;
                            if got >= 32 {
                                break;
                            }
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
                if disconnected {
                    // 采集流断了（设备被拔/被独占/驱动报错）。
                    // 语义上就是「这一句到此为止」：必须走和松手一样的收尾，
                    // 包括把录音指示收掉、把死掉的 Capture 丢掉。
                    //
                    // 🔴 以前这里只改 recording/stage，既不淡出边缘光、也不丢 Capture：
                    //    · 边缘光永远挂在屏幕上，而且它是 60fps 全屏重绘，白烧一个核；
                    //    · 之后每次按下热键都会在这里立刻再断一次，用户看到的是
                    //      「输入法彻底卡死、还关不掉」。
                    recording = false;
                    stage = Stage::AwaitFinal;
                    let _ = side.finish();
                    ov_record_end(&o.overlay);
                    cap_dead = true;
                    if let Some(l) = &o.log {
                        l.line(&format!(
                            "D {:6}ms  麦克风流断开 → 按松手收尾（丢弃采集会话，下次按下重开）",
                            t0.elapsed().as_millis()
                        ));
                    }
                    eprintln!(
                        "[sainput] ⚠ 麦克风流已断开 —— 本句按松手收尾，下次按下时重新打开麦克风"
                    );
                } else if got == 0 {
                    // 没数据就小睡一下，别空转烧 CPU
                    std::thread::sleep(Duration::from_millis(4));
                }
            }
        } else if tail.is_some() {
            let finished = {
                let text_idle_ms = last_text_at.elapsed().as_millis() as f64;
                let t = tail.as_mut().unwrap();
                tail_tick(&mut side, &o, &cap, &mut fst, t, text_idle_ms)
            };
            if finished {
                if let Some(t) = tail.take() {
                    report_tail(&o, &t, t0.elapsed().as_millis(), "安静了/到上限", dropped_now(&cap));
                }
                stage = Stage::AwaitFinal;
                println!("[sainput] ○ 尾巴收完，识别中……");
                // 收尾结束 → 细条模式才切回细条（这时才开始出文字）。
                //
                // 🔴 这里原本不看模式，无条件 show_bar("")。后果是：
                //   边缘光/悬浮球模式在松手后会被**强行切成空细条** ——
                //   边缘光瞬间消失（不是淡出），而且留下一个空的半透明条挂在屏幕上。
                //   必须按模式分流。
                if overlay_mode() == 1 {
                    if let Some(ov) = ov_get() {
                        ov.show_bar("");
                    }
                }
            }
        } else if o.lazy_mic {
            cap = None; // 按需模式：松手就释放麦克风
        } else {
            let mut errored = false;
            if let Some(c) = &cap {
                while c.rx.try_recv().is_ok() {} // 常驻模式：空闲时排掉，防堆积
                // 驱动报过错 = 这条流已经不可信。留着它，下次按下热键只会
                // 收不到任何音频（用户看到「没输入」）；丢掉它，下一次
                // audio::start 会重新打开麦克风。
                errored = c.stats.stream_errors.load(Ordering::Relaxed) > 0;
            }
            if errored {
                cap = None;
                eprintln!("[sainput] ⚠ 麦克风流报过错，已释放；下次按下热键时重新打开");
            }
            std::thread::sleep(Duration::from_millis(8));
        }

        if cap_dead {
            cap = None; // 死流必须丢，否则下次按下会立刻再断一次
        }

        // 2.5) 托盘开关 → sidecar（立刻改策略，不必重开）
        while let Ok(key) = ctl.try_recv() {
            // 静音门槛是"值"而不是"开关"，单独一条格式：silence_sec:15
            if let Some(sec) = key.strip_prefix("silence_sec:") {
                let _ = side.send_set("silence_sec", serde_json::Value::String(sec.to_string()));
                continue;
            }
            let v = match key.as_str() {
                "punct_mode" => serde_json::Value::String(
                    if PUNCT_SPACE.load(std::sync::atomic::Ordering::Relaxed) {
                        "space".into()
                    } else {
                        "keep".into()
                    },
                ),
                "merge_terms" => serde_json::Value::Bool(
                    MERGE_TERMS.load(std::sync::atomic::Ordering::Relaxed),
                ),
                "fix_case" => {
                    serde_json::Value::Bool(FIX_CASE.load(std::sync::atomic::Ordering::Relaxed))
                }
                "ctx_bias" => {
                    serde_json::Value::Bool(CTX_BIAS.load(std::sync::atomic::Ordering::Relaxed))
                }
                _ => continue,
            };
            let _ = side.send_set(&key, v);
        }

        // 3) ASR 输出 → 上屏
        side.drain(&mut buf);
        for m in buf.drain(..) {
            // 空闲状态下迟到的东西一律丢掉（别把已经上屏的字擦掉、也别让
            // 模型把静音「听」成字打到屏幕上）
            if stage == Stage::Idle {
                continue;
            }
            let is_final_finish = matches!(&m, Msg::Final { reason, .. } if reason == "finish");
            let is_final_endpoint = matches!(&m, Msg::Final { reason, .. } if reason == "endpoint");
            let ui = Ui {
                ov: o.overlay.as_ref(),
                log: o.log.as_ref(),
                dry: false,
                max_bs: o.max_backspace,
                recording,
                t0,
            };
            if matches!(&m, Msg::Partial { .. }) {
                last_text_at = Instant::now();
            }
            // 上下文偏置：定稿时记下这句的**原文**（不是渲染后的文本 ——
            // 渲染层做过标点/空格整形，拿来抽术语反而更脏）。要在 handle_msg 之前取。
            if let Msg::Final { text, raw, .. } = &m {
                let rec = if raw.is_empty() { text.as_str() } else { raw.as_str() };
                history.push(rec);
            }
            handle_msg(m, &mut wr, &ui);
            // finish 的 final = 这一句彻底定稿；尾巴期间的 endpoint 也已经定稿
            if is_final_finish || (is_final_endpoint && stage != Stage::Recording) {
                stage = Stage::Idle;
                if defer_reset {
                    // 上一句的 final 终于到了：现在才轮到新一句重新起算
                    wr.reset();
                    defer_reset = false;
                }
            }
        }
    }
}

/// 尾巴参数（run 和 config 都用它，保证两边说的是一回事）。
fn tail_of(a: &Args) -> Result<TailCfg> {
    Ok(TailCfg {
        min_ms: a.u("tail_ms", "--tail-ms", 200)? as u32,
        max_ms: a.u("tail_max_ms", "--tail-max-ms", 1200)? as u32,
        quiet_ms: a.u("tail_quiet_ms", "--tail-quiet-ms", 250)? as u32,
        pad_ms: a.u("tail_pad_ms", "--tail-pad-ms", 480)? as u32,
        rms: a.f("tail_rms", "--tail-rms", 0.012)? as f32,
    })
}

fn cmd_run(_args: &[String], a: &Args) -> Result<()> {
    // 单实例：两个 sainput 同时跑会各挂一个全局钩子，说一句话屏幕上打两遍。
    // 后启动的接管：先让旧实例退出（优先托盘退出，退不掉才硬杀），再自己拿锁。
    // 这样每次重新构建后双击一次，跑的一定是最新二进制，也不会留下旧进程。
    let Some(_guard) = single::acquire_or_take_over() else {
        println!("[sainput] 旧实例退不掉（可能以管理员权限在跑）—— 先手动退出它再启动。");
        std::thread::sleep(Duration::from_secs(6));
        return Ok(());
    };
    let c = cfg(a)?;
    // 开关的**实时**状态：从配置初始化（不是硬编码默认），
    // 之后托盘菜单改一次就更新一次，worker 每句读它 → 改了立刻生效。
    let verbose_ctx_init = a.b("verbose_ctx", "--verbose-ctx", "--quiet-ctx", false);
    // 前置空格：默认开（用户明确要「光标前有字就插空格」）
    let leading_space_init = a.b("leading_space", "--leading-space", "--no-leading-space", true);
    // 悬浮条默认**关**：文字已经逐字上屏，再显示一遍是重复信息（用户指出）。
    // 悬浮显示方式：配置里 `overlay` 兼容老写法（true=细条、false=关），
    // 新增 `overlay_mode = bar|orb|edge|off` 表达四态。
    let overlay_mode_init: u32 = match a.s("overlay_mode", "--overlay-mode").as_deref() {
        Some("bar") => 1,
        Some("orb") => 2,
        Some("edge") => 3,
        Some("off") => 0,
        _ => {
            // 默认就是**屏幕边缘光**（用户选定的方案：不占中央、最不干扰）。
            // 老写法 overlay = false 仍然能关掉。
            if a.b("overlay", "--overlay", "--no-overlay", false) { 1 } else { 3 }
        }
    };
    let overlay_on = overlay_mode_init != 0;
    // 当前热键的配置写法（托盘打勾用）。命令行/conf 传什么就记什么；
    // 若它是候选表里的一项，就原样匹配，否则托盘里不打勾。
    let hk_arg = a.s("hotkey", "--hotkey").unwrap_or_else(|| "rightctrl".to_string());
    let cur_hotkey_conf: &'static str = tray::HOTKEY_CANDIDATES
        .iter()
        .find(|(conf, _)| *conf == hk_arg)
        .map(|(conf, _)| *conf)
        .unwrap_or("rightctrl");
    if let Ok(mut g) = CUR_HOTKEY.lock() {
        *g = cur_hotkey_conf;
    }
    {
        use std::sync::atomic::Ordering as O;
        PUNCT_SPACE.store(c.punct_mode == "space", O::Relaxed);
        MERGE_TERMS.store(c.merge_terms, O::Relaxed);
        CTX_BIAS.store(c.ctx_bias, O::Relaxed);
        VERBOSE_CTX.store(verbose_ctx_init, O::Relaxed);
        LEADING_SPACE.store(leading_space_init, O::Relaxed);
        FIX_CASE.store(c.fix_case, O::Relaxed);
        SILENCE_SEC.store(c.rule2.round() as u32, O::Relaxed);
        OVERLAY_MODE.store(overlay_mode_init, O::Relaxed);
        // 光色从配置读；没配就白
        let tint = EdgeTint::from_conf(
            &a.s("edge_tint", "--edge-tint").unwrap_or_else(|| "white".to_string()),
        );
        sainput_core::overlay::set_edge_tint(tint);
        tray::set_flags(tray::TrayFlags {
            punct_to_space: c.punct_mode == "space",
            merge_terms: c.merge_terms,
            ctx_bias: c.ctx_bias,
            ctx_verbose: verbose_ctx_init,
            leading_space: leading_space_init,
            fix_case: c.fix_case,
            silence_sec: c.rule2.round() as u32,
            overlay_mode: overlay_mode_init as usize,
            edge_tint: tint_code_of(),
            hotkey: cur_hotkey_conf,
            autostart: autostart::is_enabled(),
        });
    }
    let streaming = a.b("stream", "--stream", "--no-stream", true);
    let max_bs = a.u("max_backspace", "--max-backspace", 8)?;
    let device = a.s("device", "--device");
    let vk_name = a.s("hotkey", "--hotkey").unwrap_or_else(|| "rightctrl".to_string());
    // 热键可以是键盘键，也可以是**鼠标键**（侧键 X1/X2、中键）—— 见 hotkey::Trigger。
    let trigger = hotkey::parse_trigger(&vk_name)?;
    if let hotkey::Trigger::Key(_) = trigger {
        // 键盘键保持原样；鼠标键走另一条钩子，不需要这里校验
    }
    let toggle = a.b("toggle", "--toggle", "--hold", false);
    let lazy_mic = a.b("lazy_mic", "--lazy-mic", "--warm-mic", false);
    // 尾巴参数：实测默认值见 README「尾巴」一节（都是拿真机麦克风量出来的）
    let tail = tail_of(a)?;
    if tail.rms > 0.0 {
        println!(
            "[sainput] 尾巴: 松键后最少收 {}ms（上限 {}ms / 安静 {}ms 即收工）+ 补 {}ms 底噪（静音阈值 RMS {:.3}）",
            tail.min_ms, tail.max_ms, tail.quiet_ms, tail.pad_ms, tail.rms
        );
    }

    // 先把控制台标题改掉：默认标题是那串 exe 路径，任务栏里根本认不出是什么程序。
    // 控制台标题也带版本号 —— 用户开着黑窗口时一眼就知道跑的是哪版。
    tray::set_console_title(&format!(
        "sainput v{} —— 按住{}说话",
        env!("CARGO_PKG_VERSION"),
        trigger.label()
    ));

    // 横幅按显示宽度补齐：热键名长短不一，硬编码空格会让右边框歪掉。
    const BOX_W: usize = 73;
    let rule = "─".repeat(BOX_W);
    println!("┌{rule}┐");
    println!("│{}│", pad_display("  sainput 本地语音输入法 · 全离线", BOX_W));
    println!(
        "│{}│",
        pad_display(&format!("  按住【{}】说话，松开即上屏", trigger.label()), BOX_W)
    );
    println!("│{}│", pad_display("  光标放进要打字的窗口再说话", BOX_W));
    println!("└{rule}┘");

    c.describe();

    let overlay = if overlay_on {
        let ov = Overlay::start();
        match &ov {
            Some(o) => {
                o.show_bar("正在加载模型…");
                println!(
                    "[sainput] 悬浮显示: {}",
                    match overlay_mode_init {
                        2 => "悬浮球",
                        3 => "屏幕边缘光",
                        _ => "细条",
                    }
                );
            }
            None => eprintln!("[sainput] 悬浮窗起不来（不影响语音输入）"),
        }
        ov
    } else {
        None
    };

    ov_put(overlay.clone());
    let side = c.spawn()?;
    println!("[sainput] ASR 就绪: {} ({})", side.model, side.decoding);
    if let Some(ov) = &overlay {
        ov.show("就绪 · 按住热键说话", "");
        ov.hide_after(1500);
    }

    // 托盘图标：双击之后只留一个黑窗口太容易让人以为「没启动」。
    // 有了托盘，用户随时看得见它在不在跑，也能右键退出。
    // 菜单第一行带**版本号** —— 用户要看得出自己跑的是哪一版，不然改了也不知道生效没。
    let tray_on = a.b("tray", "--tray", "--no-tray", true);
    let tray_title = format!(
        "sainput v{} · 按住{}说话",
        env!("CARGO_PKG_VERSION"),
        trigger.label()
    );
    tray::set_title(&tray_title);
    let tray = if tray_on {
        Tray::start(&tray_title)
    } else {
        None
    };
    match &tray {
        Some(t) => {
            println!("[sainput] 托盘图标: 开（右键菜单可退出，也能直接开关键盘：标点/英文/偏置）");
            t.balloon(
                &format!("sainput v{} 已就绪", env!("CARGO_PKG_VERSION")),
                &format!(
                    "按住【{}】说话，松开上屏。右键托盘图标可以退出或改开关。",
                    trigger.label()
                ),
            );
        }
        None => {
            if tray_on {
                eprintln!("[sainput] 托盘图标没起来（不影响录音，可用 --no-tray 关掉这条提示）");
            }
        }
    }

    // 测试用：喂到第几秒音频自动松键（配合 --feed 做吞字回归）
    let feed_release = a.f("feed_release_at", "--feed-release-at", 0.0)?;
    let feed_release = if feed_release > 0.0 { Some(feed_release) } else { None };

    let feed = match a.s("feed", "--feed") {
        Some(p) => {
            let path = resolve_path(&find_root(), &p);
            let spath = path.display().to_string();
            let data = wav::read_as_pcm16_mono_16k(&spath)
                .with_context(|| format!("读取 --feed WAV 失败: {spath}"))?;
            println!("[sainput] feed 模式：{} ({:.2}s)", path.display(), data.len() as f64 / audio::TARGET_RATE as f64);
            Some(data)
        }
        None => None,
    };

    let log = if a.b("log", "--log", "--no-log", true) {
        let dir = resolve_path(
            &find_root(),
            &a.s("log_dir", "--log-dir").unwrap_or_else(|| "logs".to_string()),
        );
        let l = Logger::open(&dir);
        match &l {
            Some(x) => println!("[sainput] 会话日志: {}", x.path()),
            None => eprintln!("[sainput] 会话日志建不了（不影响使用）"),
        }
        l
    } else {
        None
    };
    if let Some(l) = &log {
        l.line(&format!(
            "=== sainput {} 启动 | 模型 {} | 节流 {}ms | 尾巴 {}/{}/{}ms+补{} rms={} | rule1={} rule2={} rule3={} ===",
            env!("CARGO_PKG_VERSION"),
            c.model_dir.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default(),
            c.throttle_ms,
            tail.min_ms, tail.max_ms, tail.quiet_ms, tail.pad_ms, tail.rms,
            c.rule1, c.rule2, c.rule3
        ));
    }

    // 日志文件路径：托盘「显示日志」要用。
    // 必须在 log 被 move 进 RunOpts **之前**取。
    let log_path: Option<std::path::PathBuf> =
        log.as_ref().map(|l| std::path::PathBuf::from(l.path()));

    let opts = RunOpts {
        feed,
        log,
        streaming,
        max_backspace: max_bs,
        device,
        toggle,
        lazy_mic,
        overlay,
        tail,
        feed_release,
        ctx_hotwords: if a.b("ctx_bias", "--ctx-bias", "--no-ctx-bias", true) {
            c.hotwords_extra.clone()
        } else {
            None
        },
        verbose_ctx: a.b("verbose_ctx", "--verbose-ctx", "--quiet-ctx", false),
        ime_wmchar: c.ime_wmchar.clone(),
        feed_restart: a.b("feed_restart", "--feed-restart", "--no-feed-restart", false),
    };
    // 托盘开关要送到 worker 手里的 sidecar（side 被 worker 独占，主线程够不着）。
    // 主线程点菜单 → 这条 channel → worker 每拍取走并 send_set 给 sidecar。
    let (side_ctl_tx, side_ctl_rx) = mpsc::channel::<String>();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Err(e) = worker(rx, side, opts, side_ctl_rx) {
            eprintln!("[sainput] worker 退出: {e}");
        }
    });

    Tray::note_main_thread();
    hotkey::install(trigger, tx)?;
    println!(
        "[sainput] ✅ 就绪。{} [{}] {}。",
        if toggle { "按一下" } else { "按住" },
        trigger.label(),
        if toggle { "开始/结束录音" } else { "说话，松开上屏" }
    );
    if tray.is_some() {
        println!("[sainput] 退出方式：右键托盘图标 → 退出 sainput");
    } else {
        println!("[sainput] 退出方式：在这个窗口按 Ctrl+C");
    }

    // 托盘起来了就把控制台收起来，省得一直占着屏幕；右键退出时会自动还原。
    let hide_console = a.b("hide_console", "--hide-console", "--show-console", false);
    if hide_console && tray.is_some() {
        std::thread::sleep(Duration::from_millis(900));
        tray::set_console_visible(false);
    }

    // 托盘菜单点出来的开关：主线程每拍收一次 —— 落盘（下次启动仍生效）+ 推给托盘画勾 +
    // 通知 sidecar 立刻改用新策略（不用重开程序）。
    let conf_path = find_root().join(CONF_NAME);
    let side_tx = side_ctl_tx;
    let apply_toggle = move |t: tray::TrayToggle| {
        use std::sync::atomic::Ordering as O;
        let (key, val, human) = match t {
            tray::TrayToggle::PunctToSpace => {
                let v = !PUNCT_SPACE.load(O::Relaxed);
                PUNCT_SPACE.store(v, O::Relaxed);
                ("punct_mode", if v { "space" } else { "keep" },
                 if v { "标点转空格：开（问号保留）" } else { "标点策略：原样" })
            }
            tray::TrayToggle::MergeTerms => {
                let v = !MERGE_TERMS.load(O::Relaxed);
                MERGE_TERMS.store(v, O::Relaxed);
                ("merge_terms", if v { "true" } else { "false" },
                 if v { "英文连写（R E A → REA）：开" } else { "英文连写：关" })
            }
            tray::TrayToggle::CtxBias => {
                let v = !CTX_BIAS.load(O::Relaxed);
                CTX_BIAS.store(v, O::Relaxed);
                ("ctx_bias", if v { "true" } else { "false" },
                 if v { "上下文偏置：开" } else { "上下文偏置：关" })
            }
            tray::TrayToggle::CtxVerbose => {
                let v = !VERBOSE_CTX.load(O::Relaxed);
                VERBOSE_CTX.store(v, O::Relaxed);
                ("verbose_ctx", if v { "true" } else { "false" },
                 if v { "偏置提示：开" } else { "偏置提示：关" })
            }
            tray::TrayToggle::SetSilence(sec) => {
                SILENCE_SEC.store(sec, O::Relaxed);
                // 静音断句门槛是 sidecar 的 live 值，这里只负责把新值送过去（下面的 side_tx）。
                // 0 = 不断句（只认松手）。
                let human: &'static str = match sec {
                    5 => "断句门槛：5 秒",
                    10 => "断句门槛：10 秒",
                    15 => "断句门槛：15 秒",
                    30 => "断句门槛：30 秒",
                    _ => "断句门槛：不断句（只认松手）",
                };
                println!("[sainput] 托盘开关 → {human}");
                // 单独发（不走下面统一的 key/val 落盘路径，因为它要存成 rule2）
                let _ = side_tx.send(format!("silence_sec:{sec}"));
                // 落盘到 rule2，下次启动仍生效
                if let Err(e) = config::save_key(&conf_path, "rule2", &sec.to_string()) {
                    eprintln!("[sainput] rule2 写回失败（本次仍生效）: {e}");
                }
                tray::set_flags(tray::TrayFlags {
                    punct_to_space: PUNCT_SPACE.load(O::Relaxed),
                    merge_terms: MERGE_TERMS.load(O::Relaxed),
                    ctx_bias: CTX_BIAS.load(O::Relaxed),
                    ctx_verbose: VERBOSE_CTX.load(O::Relaxed),
                    leading_space: LEADING_SPACE.load(O::Relaxed),
                    fix_case: FIX_CASE.load(O::Relaxed),
                    silence_sec: sec,
                    overlay_mode: overlay_mode() as usize,
                    edge_tint: tint_code_of(),
                    hotkey: CUR_HOTKEY.lock().map(|g| *g).unwrap_or("rightctrl"),
                    autostart: autostart::is_enabled(),
                });
                return;
            }
            tray::TrayToggle::FixCase => {
                let v = !FIX_CASE.load(O::Relaxed);
                FIX_CASE.store(v, O::Relaxed);
                ("fix_case", if v { "true" } else { "false" },
                 if v { "英文小写修复：开" } else { "英文小写修复：关" })
            }
            tray::TrayToggle::SetOverlayMode(m) => {
                let m = m as u32;
                OVERLAY_MODE.store(m, O::Relaxed);
                // 需要窗口时**现在**建起来 —— 窗口原本只在启动时按模式创建，
                // 运行时只改原子量等于什么都没发生，用户点了开关却看不到东西。
                if m != 0 {
                    if ov_get().is_none() {
                        match Overlay::ensure() {
                            Some(o) => {
                                ov_put(Some(o));
                                println!("[sainput] 悬浮窗已启动");
                            }
                            None => eprintln!("[sainput] 悬浮窗起不来（不影响语音输入）"),
                        }
                    }
                    // 立刻给一个可见的回执，让用户知道点的这一下生效了
                    if let Some(o) = ov_get() {
                        if m == 2 {
                            o.show_orb();
                            // 静音时波形是平的，先垫几个点让它看起来是"活的"
                            for i in 0..9 {
                                o.push_level(0.25 + 0.15 * ((i % 3) as f32));
                            }
                        } else if m == 3 {
                            // 边缘光：亮 2 秒再淡出，让用户看见效果
                            o.show_edge();
                            let ov2 = o.clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(Duration::from_millis(2000));
                                ov2.fade_edge();
                            });
                        } else {
                            o.show_bar("悬浮条已开 · 说话时显示文字");
                            o.hide_after(2500);
                        }
                    }
                } else {
                    if let Some(o) = ov_get() {
                        o.hide();
                    }
                }
                let human = match m {
                    1 => "悬浮显示：细条".to_string(),
                    2 => "悬浮显示：悬浮球".to_string(),
                    3 => "悬浮显示：屏幕边缘光".to_string(),
                    _ => "悬浮显示：不显示".to_string(),
                };
                ("overlay_mode",
                 match m { 1 => "bar", 2 => "orb", 3 => "edge", _ => "off" },
                 // 借用 String 的生命周期：下面统一按 &str 用，这里返回 'static 保证能活
                 Box::leak(human.into_boxed_str()) as &'static str)
            }
            tray::TrayToggle::LeadingSpace => {
                let v = !LEADING_SPACE.load(O::Relaxed);
                LEADING_SPACE.store(v, O::Relaxed);
                ("leading_space", if v { "true" } else { "false" },
                 if v { "前面自动加空格：开" } else { "前面自动加空格：关" })
            }
            tray::TrayToggle::ReleaseKeys => {
                // 键盘失灵时的自救：把可能卡在「按下」的修饰键全部补 keyup。
                //
                // 什么时候会卡：钩子吃掉了 keydown，而 keyup 没能送达
                // （进程被强杀 / 崩溃 / 拔电源）。系统就认为那个键一直按着。
                hotkey::panic_release();
                println!("[sainput] 托盘 → 已释放所有修饰键");
                return;
            }
            tray::TrayToggle::ShowConsole => {
                // 控制台已经 FreeConsole 释放了（Windows Terminal 承载下 ShowWindow
                // 藏不掉窗口，只能真正分离），不可恢复。
                // 这一项改成**打开日志文件** —— 用户点它本来就是为了看日志/报错。
                match log_path.clone() {
                    Some(p) if p.exists() => {
                        // 用系统默认关联程序打开（.log 通常关联记事本）
                        let _ = std::process::Command::new("cmd")
                            .args(["/c", "start", "", &p.to_string_lossy()])
                            .spawn();
                    }
                    _ => {
                        eprintln!("[sainput] 托盘 → 找不到日志文件");
                    }
                }
                return;
            }
            tray::TrayToggle::AutoStart => {
                let on = !autostart::is_enabled();
                let exe = std::env::current_exe().unwrap_or_default();
                let r = if on { autostart::enable(&exe) } else { autostart::disable() };
                match r {
                    Ok(()) => println!(
                        "[sainput] 托盘 → 开机自启：{}",
                        if on { "开" } else { "关" }
                    ),
                    Err(e) => eprintln!("[sainput] 开机自启设置失败: {e}"),
                }
                // 自启是注册表状态，不落 sainput.conf；重推一次勾选即可
                tray::set_flags(tray::TrayFlags {
                    punct_to_space: PUNCT_SPACE.load(O::Relaxed),
                    merge_terms: MERGE_TERMS.load(O::Relaxed),
                    ctx_bias: CTX_BIAS.load(O::Relaxed),
                    ctx_verbose: VERBOSE_CTX.load(O::Relaxed),
                    leading_space: LEADING_SPACE.load(O::Relaxed),
                    fix_case: FIX_CASE.load(O::Relaxed),
                    silence_sec: SILENCE_SEC.load(O::Relaxed),
                    overlay_mode: overlay_mode() as usize,
                    edge_tint: tint_code_of(),
                    hotkey: CUR_HOTKEY.lock().map(|g| *g).unwrap_or("rightctrl"),
                    autostart: autostart::is_enabled(),
                });
                return;
            }
            tray::TrayToggle::SetEdgeTint(i) => {
                let tints = [
                    EdgeTint::White,
                    EdgeTint::Rainbow,
                    EdgeTint::Amber,
                    EdgeTint::Cyan,
                    EdgeTint::Aurora,
                ];
                let Some(t) = tints.get(i).copied() else {
                    return;
                };
                sainput_core::overlay::set_edge_tint(t);
                // 立刻演示一下：切到边缘光并亮 1.5 秒
                if overlay_mode() != 3 {
                    OVERLAY_MODE.store(3, O::Relaxed);
                    if ov_get().is_none() {
                        if let Some(o) = Overlay::ensure() {
                            ov_put(Some(o));
                        }
                    }
                }
                if let Some(o) = ov_get() {
                    o.show_edge();
                    let ov2 = o.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(1500));
                        ov2.fade_edge();
                    });
                }
                println!("[sainput] 边缘光颜色 → {}", t.label());
                ("edge_tint", t.as_conf(),
                 Box::leak(format!("边缘光颜色：{}", t.label()).into_boxed_str()) as &'static str)
            }
            tray::TrayToggle::SetHotkey(i) => {
                let Some((conf, _)) = tray::HOTKEY_CANDIDATES.get(i) else {
                    return;
                };
                // 立刻换键（原子量，不用重装钩子、不用重启）+ 重画托盘标题
                match hotkey::parse_trigger(conf) {
                    Ok(t) => {
                        hotkey::set_trigger(t);
                        let new_title = format!(
                            "sainput v{} · 按住{}说话",
                            env!("CARGO_PKG_VERSION"),
                            t.label()
                        );
                        tray::set_title(&new_title);
                        tray::set_console_title(&format!(
                            "sainput v{} —— 按住{}说话",
                            env!("CARGO_PKG_VERSION"),
                            t.label()
                        ));
                        // 记下当前热键写法给托盘打勾用（候选表里是 &'static str，直接存）
                        if let Ok(mut g) = CUR_HOTKEY.lock() {
                            *g = conf;
                        }
                        println!("[sainput] 托盘换热键 → {}（已写入 sainput.conf，立刻生效）", t.label());
                        ("hotkey", *conf, "")
                    }
                    Err(e) => {
                        eprintln!("[sainput] 热键切换失败: {e}");
                        return;
                    }
                }
            }
        };
        // 落盘：失败不致命（本次仍生效，只是下次启动回到文件里的值）
        if let Err(e) = config::save_key(&conf_path, key, val) {
            eprintln!("[sainput] 开关写回 sainput.conf 失败（本次仍生效）: {e}");
        }
        if !human.is_empty() {
            println!("[sainput] 托盘开关 → {human}（已写入 sainput.conf）");
        }
        // 通知 sidecar 立刻改用新策略
        let _ = side_tx.send(key.to_string());
        // 推当前状态给托盘，让菜单里的勾跟上
        tray::set_flags(tray::TrayFlags {
            punct_to_space: PUNCT_SPACE.load(O::Relaxed),
            merge_terms: MERGE_TERMS.load(O::Relaxed),
            ctx_bias: CTX_BIAS.load(O::Relaxed),
            ctx_verbose: VERBOSE_CTX.load(O::Relaxed),
            leading_space: LEADING_SPACE.load(O::Relaxed),
            fix_case: FIX_CASE.load(O::Relaxed),
            silence_sec: SILENCE_SEC.load(O::Relaxed),
            overlay_mode: overlay_mode() as usize,
                    edge_tint: tint_code_of(),
            hotkey: CUR_HOTKEY.lock().map(|g| *g).unwrap_or("rightctrl"),
            autostart: autostart::is_enabled(),
        });
    };

    // 50ms 的超时只是"多久检查一次托盘开关"，**不是**鼠标事件的等待时间 ——
    // 见 run_message_loop_with_tick 的注释（用 sleep 等消息会让鼠标卡成幻灯片）。
    hotkey::run_message_loop_with_tick(50, || {
        if let Some(t) = tray::take_toggle() {
            apply_toggle(t);
        }
    });
    hotkey::uninstall();
    Ok(())
}


// ------------------------------------------------------ overlay-demo
/// 预览光标旁的状态条（调试/验收用，不影响正常录音）。
fn cmd_overlay_demo(a: &Args) -> Result<()> {
    let secs = a.u("seconds", "--seconds", 10)? as u64;
    let ov = Overlay::start().ok_or_else(|| anyhow::anyhow!("悬浮窗起不来"))?;
    let mode = a.s("mode", "--mode").unwrap_or_else(|| "bar".to_string());

    if mode == "edge" {
        // 屏幕边缘光：亮 2.5 秒 → 淡出，完整走一遍录音的生命周期
        if let Some(t) = a.s("tint", "--tint") {
            let tint = EdgeTint::from_conf(&t);
            sainput_core::overlay::set_edge_tint(tint);
            println!("[sainput] overlay-demo: 边缘光 · {}", tint.label());
        } else {
            println!("[sainput] overlay-demo: 屏幕边缘光（亮起 2.5s 后淡出）");
        }
        ov.show_edge();
        std::thread::sleep(Duration::from_millis(2500));
        ov.fade_edge();
        std::thread::sleep(Duration::from_millis(1200));
        ov.hide();
        println!("[sainput] overlay-demo 结束");
        return Ok(());
    }

    if mode == "orb" {
        // 悬浮球：喂一段模拟音量（由小到大再回落），看波形动不动。
        ov.show_orb();
        println!("[sainput] overlay-demo: 悬浮球（模拟音量，{secs}s）");
        let step = (secs * 1000 / 60).max(40);
        for i in 0..60u32 {
            // 用一个正弦包络模拟说话起伏，明显能看出波形在变
            let t = i as f32 / 60.0;
            let env = (t * std::f32::consts::PI * 3.0).sin().abs();
            // 叠一点随机，别看起来像机器
            let jitter = ((i * 7919) % 100) as f32 / 100.0 * 0.25;
            ov.push_level((env * 0.75 + jitter).min(1.0));
            std::thread::sleep(Duration::from_millis(step));
        }
        ov.hide();
        println!("[sainput] overlay-demo 结束");
        return Ok(());
    }

    // 细条：只显示文字，逐段变长
    let frames: [&str; 4] = [
        "今天下午",
        "今天下午三点十五分开会",
        "今天下午三点十五分开会，讨论一下这个方案",
        "今天下午3点15分开会，讨论一下这个方案，顺便把合同带过来。",
    ];
    let step = (secs * 1000 / frames.len() as u64).max(400);
    println!("[sainput] overlay-demo: 细条（只显示文字，{secs}s）");
    for tx in frames {
        ov.show_bar(tx);
        std::thread::sleep(Duration::from_millis(step));
    }
    ov.hide();
    println!("[sainput] overlay-demo 结束");
    Ok(())
}

// ---------------------------------------------------------------- selftest
fn cmd_selftest(args: &[String]) -> Result<()> {
    match args.first().map(|s| s.as_str()) {
        Some("inject") => {
            let text = args
                .get(1)
                .cloned()
                .unwrap_or_else(|| "sainput 上屏自检 ABC 123".to_string());
            println!("[selftest] 3 秒后把下面的文本打进**当前焦点窗口**：");
            println!("           {text}");
            for i in (1..=3).rev() {
                println!("           {i}…");
                std::thread::sleep(Duration::from_secs(1));
            }
            inject::type_text(&text)?;
            println!("[selftest] SendInput 已接受全部事件（目标窗口是否显示取决于它是否接受注入）");
            Ok(())
        }
        _ => {
            println!("用法: sainput selftest inject <text>");
            Ok(())
        }
    }
}

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (st, root) = args_of();
    let cmd = argv.first().map(|s| s.as_str()).unwrap_or("help");
    // 子命令之后的参数才参与「选项」解析
    let body: Vec<String> = if argv.is_empty() {
        Vec::new()
    } else {
        argv[1..].to_vec()
    };
    let a = Args { cli: &body, st: &st };

    match cmd {
        "help" | "--help" | "-h" => {
            usage();
            Ok(())
        }
        "devices" => cmd_devices(),
        "record" => cmd_record(&body, &a),
        "probe" => cmd_probe(&a),
        "replay" => cmd_replay(&body, &a),
        "render" => cmd_render(&body, &a),
        "config" => cmd_config(&a, &root),
        "overlay-demo" => cmd_overlay_demo(&a),
        "release-keys" => {
            // 救命命令：进程被强杀后，热键可能卡在「按下」状态，
            // 表现为空格打不出 / 滚轮变缩放 / 整个键盘失灵。
            // 这条命令给所有修饰键补发 keyup。
            hotkey::panic_release();
            println!("[sainput] 已释放所有修饰键（Ctrl/Alt/Shift/Win 等）");
            Ok(())
        }
        "run" => cmd_run(&body, &a),
        "selftest" => cmd_selftest(&body),
        other => {
            eprintln!("未知子命令: {other}\n");
            usage();
            std::process::exit(2);
        }
    }
}