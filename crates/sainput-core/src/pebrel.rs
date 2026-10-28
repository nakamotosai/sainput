//! Pebrel 控制面客户端 —— 用它的**组合态缓冲层**上屏。
//!
//! 为什么不是注入按键:听写的一句话在说完之前一直在被改写(真机语料里约 5% 的帧会
//! 回头改前面的词)。按键那条路只能「逐字上屏 + 退格补打」,中间态是真的落进别人窗口
//! 的按键副作用,还要跟输入法抢顺序。Pebrel 的控制面提供 `pane.compose`:文本先只进
//! 它自己的组合串(preedit,可无限次改写),`commit` 时才一次性写进 PTY —— 屏幕上不会
//! 出现半句被改残的文本,也不会产生一个按键。
//!
//! 协议面(照它 `docs/runtime-control-api.md` 的契约实现,没有抄它的 CLI):
//!   * 一行一个 JSON 请求/一行一个响应;`protocol = "nebula.runtime"`、`version = 1`,
//!     `id` 必须是**字符串**;无 token 的连接被静默丢弃;
//!   * 发现文件 `<数据目录>/runtime.port`,内容 `port token version`;
//!     数据目录 = `%APPDATA%\Pebrel`(`PEBREL_CONFIG_DIR` 优先,便携/测试用)。

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

const PROTOCOL: &str = "nebula.runtime";
const VERSION: u16 = 1;
/// 控制面是本机回环,超时给短一点:卡住的请求不该把听写堵在那里。
const IO_TIMEOUT: Duration = Duration::from_millis(400);

/// 「上屏」这件事的两条调用。抽成 trait 只为一件事:让 `StreamWriter` 在测试里
/// 带一个替身 —— 真实现会去连 Pebrel 的控制套接字,测试不该需要它开着。
pub trait Composer: Send {
    /// 把文本放进组合态缓冲层(完全不碰 PTY);空串 = 取消组合。
    fn set(&mut self, text: &str) -> Result<()>;
    /// 提交:清掉缓冲层,把整段作为**一次**写入交给 PTY。
    fn commit(&mut self, text: &str) -> Result<()>;
}

/// 真实现:发现端点 → 解析当前活动 pane → 发一条 `pane.compose`。
pub struct PebrelComposer {
    target: Option<Target>,
    seq: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    /// Pebrel 进程 pid:用来确认「眼前这个前台窗口就是 Pebrel」。
    pid: u32,
    window_id: u64,
    pane_id: u64,
}

/// Pebrel 的数据目录。`%APPDATA%\Pebrel`;`PEBREL_CONFIG_DIR` 优先(与它自己的
/// 路径解析一致:便携模式与自动化测试靠这个环境变量)。
fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PEBREL_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    std::env::var_os("APPDATA")
        .map(|p| PathBuf::from(p).join("Pebrel"))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 读发现文件。**每次调用都读**:驻留实例重启会换端口和 token,缓存下来的旧地址
/// 只会在下一次请求上失败(它的 CLI 也是每次重新发现)。
fn endpoint() -> Result<(u16, String)> {
    let path = data_dir().join("runtime.port");
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("读不到 Pebrel 发现文件 {}", path.display()))?;
    let mut parts = data.split_whitespace();
    let port: u16 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .context("runtime.port 里的端口不合法")?;
    let token = parts.next().unwrap_or("").to_string();
    if port == 0 || token.len() != 32 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("runtime.port 内容不是 `port token version`");
    }
    Ok((port, token))
}

/// 一条请求 = 一条连接(与它的 CLI 同款),读回一行 JSON。
fn call(
    port: u16,
    token: &str,
    id: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, IO_TIMEOUT)
        .with_context(|| format!("连 Pebrel 控制面 127.0.0.1:{port} 失败"))?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok();
    let request = serde_json::json!({
        "protocol": PROTOCOL,
        "version": VERSION,
        "id": id,
        "token": token,
        "method": method,
        "params": params,
    });
    stream.write_all(request.to_string().as_bytes())?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let response: serde_json::Value =
        serde_json::from_str(&line).context("Pebrel 响应不是 JSON")?;
    if response.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let error = response.get("error").cloned().unwrap_or(serde_json::Value::Null);
        return Err(refusal(method, &error));
    }
    Ok(response.get("result").cloned().unwrap_or(serde_json::Value::Null))
}

/// 把控制面的错误信封翻成一句用户能照做的话(纯函数,好测)。
///
/// 最可预见的一种是「这台 Pebrel 还没有 `pane.compose`」(旧版本):用户看到的应该
/// 是「换一版 Pebrel」,而不是一句协议错误 —— 后者会让人以为是自己配错了。
fn refusal(method: &str, error: &serde_json::Value) -> anyhow::Error {
    let code = error.get("code").and_then(|c| c.as_str()).unwrap_or("unknown");
    let message = error.get("message").and_then(|m| m.as_str()).unwrap_or("");
    if code == "method_not_found" && method == "pane.compose" {
        return anyhow::anyhow!(
            "这台 Pebrel 还没有组合态缓冲层(pane.compose):换一个带它的构建,或把配置改回 `ime_wmchar = on:pebrel.exe`"
        );
    }
    anyhow::anyhow!("Pebrel 拒绝了 {method}:{code} {message}")
}

/// 从快照里挑出「这一句该写进哪个 pane」。纯函数 —— 截下真实 `runtime.snapshot`
/// 的 JSON 就能测(`tests/fixtures/pebrel-snapshot.json`)。
fn pick_target(snapshot: &serde_json::Value) -> Result<Target> {
    let pid = snapshot.get("process_id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let windows = snapshot
        .get("windows")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    // 只有一个窗口时不必要求它 focused:用户可能刚切走又切回来,
    // 而听写的目标从来就是「Pebrel 的那个窗口」。
    let window = windows
        .iter()
        .find(|w| w.get("focused").and_then(|v| v.as_bool()) == Some(true))
        .or_else(|| (windows.len() == 1).then(|| &windows[0]))
        .context("Pebrel 有多个窗口且当前没有聚焦的那个:请把焦点放在目标窗口")?;
    let window_id = window.get("id").and_then(|v| v.as_u64()).context("快照里没有 window.id")?;
    let active_tab = window
        .get("tabs")
        .and_then(|t| t.as_array())
        .and_then(|tabs| tabs.iter().find(|t| t.get("active").and_then(|v| v.as_bool()) == Some(true)));
    let pane_id = window
        .get("focused_pane_id")
        .and_then(|v| v.as_u64())
        .or_else(|| active_tab.and_then(|t| t.get("focused_pane_id")).and_then(|v| v.as_u64()))
        .context("快照里找不到活动 pane")?;
    Ok(Target { pid, window_id, pane_id })
}

/// 端点在这台机上是全局的,但用户的眼睛只在一个地方:前端应用不是 Pebrel 时
/// 绝不能把组合串塞进后台窗口。**每次发送都查**,不跟着 target 一起缓存
/// (前台随时会变,缓存一次就把后台写放进去了)。
fn ensure_foreground(target: Target, foreground_pid: u32) -> Result<()> {
    if target.pid != 0 && foreground_pid != 0 && foreground_pid != target.pid {
        bail!(
            "前台不是 Pebrel(前台 pid {foreground_pid} / Pebrel pid {}):不往后台窗口写组合串",
            target.pid
        );
    }
    Ok(())
}

impl PebrelComposer {
    pub fn new() -> Self {
        Self { target: None, seq: 0 }
    }

    fn next_id(&mut self) -> String {
        self.seq += 1;
        format!("sainput-{}", self.seq)
    }

    /// 「这一句该写进哪个 pane」。缓存;任何一次失败都会把缓存丢掉重来 ——
    /// 目标可能在用户说话期间换了 Tab 或分屏。
    fn resolve(&mut self, port: u16, token: &str) -> Result<Target> {
        if let Some(target) = self.target {
            return Ok(target);
        }
        let id = self.next_id();
        let snapshot = call(port, token, &id, "runtime.snapshot", serde_json::Value::Null)?;
        let target = pick_target(&snapshot)?;
        self.target = Some(target);
        Ok(target)
    }

    fn send(&mut self, text: &str, commit: bool) -> Result<()> {
        let (port, token) = endpoint()?;
        let target = self.resolve(port, &token)?;
        ensure_foreground(target, crate::inject::focused_pid())?;
        let id = self.next_id();
        let params = serde_json::json!({
            "window_id": target.window_id,
            "pane_id": target.pane_id,
            "text": text,
            "commit": commit,
        });
        if let Err(e) = call(port, &token, &id, "pane.compose", params) {
            // 重新发现端点/目标再试是**下一次**的事:这里不自动重发
            // (重发一次「设置组合串」是幂等的,但把失败当成功更危险)。
            self.target = None;
            return Err(e);
        }
        Ok(())
    }
}

impl Default for PebrelComposer {
    fn default() -> Self {
        Self::new()
    }
}

impl Composer for PebrelComposer {
    fn set(&mut self, text: &str) -> Result<()> {
        self.send(text, false)
    }

    fn commit(&mut self, text: &str) -> Result<()> {
        self.send(text, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_explains_the_version_gap_instead_of_a_protocol_error() {
        let old = serde_json::json!({
            "code": "method_not_found",
            "message": "runtime API method \"pane.compose\" does not exist"
        });
        let text = refusal("pane.compose", &old).to_string();
        assert!(text.contains("还没有组合态缓冲层"), "{text}");
        assert!(text.contains("ime_wmchar"), "要告诉用户怎么退回去:{text}");

        let other = serde_json::json!({ "code": "target_not_found", "message": "pane 9 does not exist" });
        let text = refusal("pane.compose", &other).to_string();
        assert!(text.contains("target_not_found") && text.contains("pane 9 does not exist"), "{text}");

        // 别的哪个方法报 method_not_found,都不该套用这条专用提示
        let text = refusal("pane.read", &old).to_string();
        assert!(!text.contains("还没有组合态缓冲层"), "{text}");
    }

    /// 真机截下来的 `runtime.snapshot` 原样存进 fixtures —— 目标是「写进哪个 pane」
    /// 这种错了会很难看的地方,拿真数据测才算数。
    fn real_snapshot() -> serde_json::Value {
        serde_json::from_str(include_str!("../tests/fixtures/pebrel-snapshot.json")).expect("fixture")
    }

    #[test]
    fn picks_the_active_pane_from_a_real_snapshot() {
        let snap = real_snapshot();
        let pid = snap["process_id"].as_u64().expect("快照里有 process_id") as u32;
        assert_eq!(pick_target(&snap).expect("该挑得出目标"), Target { pid, window_id: 1, pane_id: 1 });
    }

    #[test]
    fn a_single_window_is_the_target_even_when_it_is_not_focused() {
        // 真机快照里窗口正是 focused=false(焦点在别的程序上)—— 只有一个窗口时
        // 该写的还是它,这是「切走又切回来」的常见形态。
        let snap = real_snapshot();
        assert_eq!(snap["windows"][0]["focused"], serde_json::json!(false));
        assert!(pick_target(&snap).is_ok());
    }

    #[test]
    fn background_pebrel_is_refused_on_every_send() {
        let snap = real_snapshot();
        let target = pick_target(&snap).unwrap();
        let err = ensure_foreground(target, target.pid + 15732).unwrap_err().to_string();
        assert!(err.contains("前台不是 Pebrel"), "{err}");
        // 前台就是它自己(以及拿不到前台 pid 时)才放行
        assert!(ensure_foreground(target, target.pid).is_ok());
        assert!(ensure_foreground(target, 0).is_ok());
    }

    #[test]
    fn several_windows_without_a_focused_one_is_an_error() {
        let mut snap = real_snapshot();
        let window = snap["windows"][0].clone();
        snap["windows"] = serde_json::json!([window.clone(), window]);
        assert!(pick_target(&snap).unwrap_err().to_string().contains("多个窗口"));
    }
}
