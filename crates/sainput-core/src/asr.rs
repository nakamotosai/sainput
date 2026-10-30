//! ASR sidecar 客户端。
//!
//! 为什么走 sidecar 而不是 Rust FFI：sherpa-onnx 的 C 结构体布局跨版本会变，
//! 手写绑定一旦对不上就是静默内存错乱。子进程 + JSON 协议把这份风险降到零，
//! 代价只有约 1ms 的管道往返。
//!
//! 协议字段（与 tools/asr_sidecar.py 对齐）：
//!   ready    {model, decoding, punct, itn, hotwords}
//!   partial  {text, raw, ms}      text 是 ITN+标点+润色之后的，raw 是识别器原文
//!   final    {text, raw, reason, tail_chunks}
//!            reason = "endpoint"(VAD 自动断句)| "finish"
//!            tail_chunks = 定稿时补解了多少帧(收尾不限 4 帧/轮),诊断用的是额外字段,客户端可忽略
//!   rendered {text, ms}
//!   reset    {ok}
//!   padded   {ms}                 静音补偿完成（模型右上下文已喂饱）
//!   error    {msg}

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::Engine as _;

/// sidecar 发回来的一条消息。
#[derive(Debug, Clone)]
pub enum Msg {
    Ready {
        model: String,
        decoding: String,
        punct: bool,
        itn: bool,
        hotwords: usize,
    },
    Partial {
        text: String,
        raw: String,
        ms: f64,
    },
    Final {
        text: String,
        raw: String,
        reason: String,
    },
    Rendered {
        text: String,
        ms: f64,
    },
    ResetAck,
    Error {
        msg: String,
    },
}

fn parse(line: &str) -> Option<Msg> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let k = v.get("type")?.as_str()?;
    let s = |f: &str| v.get(f).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let n = |f: &str| v.get(f).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let u = |f: &str| v.get(f).and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    let b = |f: &str| v.get(f).and_then(|x| x.as_bool()).unwrap_or(false);
    Some(match k {
        "ready" => Msg::Ready {
            model: s("model"),
            decoding: s("decoding"),
            punct: b("punct"),
            itn: b("itn"),
            hotwords: u("hotwords"),
        },
        "partial" => Msg::Partial {
            text: s("text"),
            raw: s("raw"),
            ms: n("ms"),
        },
        "final" => Msg::Final {
            text: s("text"),
            raw: s("raw"),
            reason: s("reason"),
        },
        "rendered" => Msg::Rendered {
            text: s("text"),
            ms: n("ms"),
        },
        "reset" => Msg::ResetAck,
        "error" => Msg::Error { msg: s("msg") },
        _ => return None,
    })
}

/// 一个正在运行的 ASR sidecar。
pub struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    pub rx: Receiver<Msg>,
    pub model: String,
    pub decoding: String,
    pub punct: bool,
    pub itn: bool,
    pub hotwords: usize,
}

impl Sidecar {
    /// `args` 直接透传给 asr_sidecar.py（--model-dir / --punct-dir / --hotwords ...）。
    pub fn spawn(python: &Path, script: &Path, args: &[String]) -> Result<Self> {
        let mut cmd = Command::new(python);
        cmd.arg(script).args(args);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            // 线程/进程优先级(体检报告 §5.2 最后一行):sidecar 每帧要解码
            // 26~39ms,是整条链路的实时瓶颈,而它和 overlay(录音时整屏重绘)、
            // worker 抢同一批核 —— 争用一上来先掉的就是音频帧。
            // 用 CreateProcess 的优先级**类**标志比事后 OpenProcess 再
            // SetPriorityClass 干净:没有「刚起来还落在 NORMAL 里」的窗口期。
            const ABOVE_NORMAL_PRIORITY_CLASS: u32 = 0x0000_8000;
            cmd.creation_flags(CREATE_NO_WINDOW | ABOVE_NORMAL_PRIORITY_CLASS);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("无法启动 ASR sidecar: {}", python.display()))?;
        let stdin = child.stdin.take().context("sidecar stdin 不可用")?;
        let stdout = child.stdout.take().context("sidecar stdout 不可用")?;

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                if let Some(m) = parse(&line) {
                    if tx.send(m).is_err() {
                        break;
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            rx,
            model: String::new(),
            decoding: String::new(),
            punct: false,
            itn: false,
            hotwords: 0,
        })
    }

    /// 等待握手完成（首次加载模型可能要几十秒）。
    pub fn wait_ready(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Msg::Ready {
                    model,
                    decoding,
                    punct,
                    itn,
                    hotwords,
                }) => {
                    self.model = model;
                    self.decoding = decoding;
                    self.punct = punct;
                    self.itn = itn;
                    self.hotwords = hotwords;
                    return Ok(());
                }
                Ok(Msg::Error { msg }) => anyhow::bail!("sidecar 启动报错: {msg}"),
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("sidecar 提前退出（检查模型路径与 Python 依赖）")
                }
            }
        }
        anyhow::bail!("等待 sidecar 就绪超时 ({timeout:?})")
    }

    /// 子进程还活着吗?(看门狗用 —— 体检报告 §2)
    ///
    /// 为什么必须由调用方定期问:`Child` 不会主动通知「对面没了」,stdout 读线程
    /// 碰到 EOF 只是安静地退出,之后往 stdin 写就是 EPIPE。以前那些写错误全被
    /// `let _ =` 吞掉,于是 sidecar 一挂,用户看到的是「按住热键、边缘光亮着、
    /// 说话,屏幕上一个字都不出」—— 没有任何提示。
    /// `try_wait` 会顺手把僵尸进程回收掉,所以判死之后 `Drop` 里的等待不会白等 2 秒。
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn send(&mut self, v: serde_json::Value) -> Result<()> {
        serde_json::to_writer(&mut self.stdin, &v)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    /// 送一帧 16 kHz 单声道 i16 音频。
    pub fn send_audio(&mut self, pcm: &[i16]) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        let mut raw = Vec::with_capacity(pcm.len() * 2);
        for s in pcm {
            raw.extend_from_slice(&s.to_le_bytes());
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        self.send(serde_json::json!({"cmd": "audio", "pcm": b64}))
    }

    /// 一次性灌入 `ms` 毫秒「房间底噪」当静音。
    ///
    /// 为什么需要：这个流式模型有右上下文，最后一个字要等它「听到」后面
    /// 约 480ms 的音频才肯吐出来。实测真等它就得再喂 640ms 实时音频；
    /// 直接由 sidecar 一次性灌 480ms 底噪，70ms 内就出字，而且因为是底噪
    /// 而不是纯 0，模型不会多吐虚词（纯 0 实测会多出一个「的」）。
    pub fn pad_silence(&mut self, ms: u32, rms: f32) -> Result<()> {
        self.send(serde_json::json!({ "cmd": "pad", "ms": ms, "rms": rms }))
    }

    /// 结束当前话音，要求出 final。
    pub fn finish(&mut self) -> Result<()> {
        self.send(serde_json::json!({ "cmd": "finish" }))
    }

    pub fn reset(&mut self) -> Result<()> {
        self.send(serde_json::json!({ "cmd": "reset" }))
    }

    /// 只跑文本后处理（ITN + 标点 + 润色），用来单测渲染链。
    pub fn send_render(&mut self, text: &str) -> Result<()> {
        self.send(serde_json::json!({ "cmd": "render", "text": text }))
    }

    /// 运行中改策略（托盘开关）。`key` 用 conf 里的键名，`value` 是字符串或布尔。
    pub fn send_set(&mut self, key: &str, value: serde_json::Value) -> Result<()> {
        self.send(serde_json::json!({ "cmd": "set", "key": key, "value": value }))
    }

    /// 等一条 Rendered（超时返回 None）。期间其它消息直接丢掉。
    pub fn wait_rendered(&mut self, timeout: Duration) -> Option<Msg> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(200)) {
                Ok(m @ Msg::Rendered { .. }) => return Some(m),
                Ok(Msg::Error { msg }) => {
                    eprintln!("[asr] 错误: {msg}");
                    return None;
                }
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
        None
    }

    /// 把当前已到达的消息全部取走。
    pub fn drain(&mut self, out: &mut Vec<Msg>) {
        while let Ok(m) = self.rx.try_recv() {
            out.push(m);
        }
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self.send(serde_json::json!({ "cmd": "quit" }));
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(50));
                }
                _ => {
                    let _ = self.child.kill();
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的解释器:优先用户指定的,否则裸 `python`(测试里图省事,
    /// 生产路径走的是 main.rs 的绝对路径探测)。
    fn test_python() -> String {
        std::env::var("SAINPUT_PYTHON")
            .ok()
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| "python".to_string())
    }

    /// 看门狗的那条判据:子进程退出之后 `is_alive()` 必须变成 false。
    ///
    /// 这是「sidecar 死了没人知道」那个 bug(体检报告 §2)的回归测试:没有它,
    /// 只能靠读码相信 worker 每拍真的能发现对面已经没了。
    #[test]
    fn is_alive_turns_false_after_child_exit() {
        // 故意给一个不存在的脚本:python 立刻报错退出(退出码 2)。
        // 这正是「sidecar 崩了」的形态 —— 这里只验 is_alive(),不验协议。
        let Ok(mut s) = Sidecar::spawn(
            Path::new(&test_python()),
            Path::new("__no_such_sidecar_script__.py"),
            &[],
        ) else {
            return; // 这台机器没有 python:跳过
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if !s.is_alive() {
                return; // 通过:确实发现了
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("子进程早就退出了,is_alive() 还是 true —— 看门狗会瞎");
    }

    /// 反向验证:还在跑的子进程**不能**被判死。
    /// 假阳性比漏报更糟 —— 那会让每一句都被掐断、还每次都去重启 sidecar。
    #[test]
    fn is_alive_true_for_running_child() {
        let Ok(mut s) = Sidecar::spawn(
            Path::new(&test_python()),
            Path::new("-c"),
            &["import time; time.sleep(3)".to_string()],
        ) else {
            return; // 没有 python:跳过
        };
        assert!(s.is_alive(), "刚起来的子进程就被判死 —— 看门狗会误杀每一条会话");
    }
}