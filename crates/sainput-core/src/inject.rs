//! 上屏：SendInput + 流式改写。
//!
//! 手写 Win32 绑定而不是用 windows crate —— 这几个结构体的 ABI 十几年没变，
//! 手工声明比跟 crate 版本较劲更可靠。

use anyhow::Result;

const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const KEYEVENTF_UNICODE: u32 = 0x0004;
const VK_BACK: u16 = 0x08;

#[repr(C)]
#[derive(Clone, Copy)]
struct KEYBDINPUT {
    w_vk: u16,
    w_scan: u16,
    dw_flags: u32,
    time: u32,
    dw_extra_info: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MOUSEINPUT {
    dx: i32,
    dy: i32,
    mouse_data: u32,
    dw_flags: u32,
    time: u32,
    dw_extra_info: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HARDWAREINPUT {
    u_msg: u32,
    w_param_l: u16,
    w_param_h: u16,
}

#[repr(C)]
union InputUnion {
    mi: MOUSEINPUT,
    ki: KEYBDINPUT,
    hi: HARDWAREINPUT,
}

#[repr(C)]
struct Input {
    type_: u32,
    u: InputUnion,
}

#[link(name = "user32")]
extern "system" {
    fn SendInput(c_inputs: u32, p_inputs: *const Input, cb_size: i32) -> u32;
}

fn unicode_event(unit: u16, keyup: bool) -> Input {
    Input {
        type_: INPUT_KEYBOARD,
        u: InputUnion {
            ki: KEYBDINPUT {
                w_vk: 0,
                w_scan: unit,
                dw_flags: KEYEVENTF_UNICODE | if keyup { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dw_extra_info: 0,
            },
        },
    }
}

fn vk_event(vk: u16, keyup: bool) -> Input {
    Input {
        type_: INPUT_KEYBOARD,
        u: InputUnion {
            ki: KEYBDINPUT {
                w_vk: vk,
                w_scan: 0,
                dw_flags: if keyup { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dw_extra_info: 0,
            },
        },
    }
}

fn send(inputs: &[Input]) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let n = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<Input>() as i32) };
    if n as usize != inputs.len() {
        anyhow::bail!("SendInput 只接受了 {n}/{} 个事件（目标窗口可能拒绝注入）", inputs.len());
    }
    Ok(())
}

/// 用 KEYEVENTF_UNICODE 打一段文本（支持中文）。
pub fn type_text(s: &str) -> Result<()> {
    let mut v: Vec<Input> = Vec::with_capacity(s.len() * 2);
    for u in s.encode_utf16() {
        v.push(unicode_event(u, false));
        v.push(unicode_event(u, true));
    }
    send(&v)
}

/// 退格 n 次。
pub fn backspace(n: usize) -> Result<()> {
    let mut v: Vec<Input> = Vec::with_capacity(n * 2);
    for _ in 0..n {
        v.push(vk_event(VK_BACK, false));
        v.push(vk_event(VK_BACK, true));
    }
    send(&v)
}

fn common_prefix_chars(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 前缀（前置空格）必须进入和正文同一条算术。
    /// 这里不真的敲键盘（type_text 会 SendInput），只检查「该显示什么」的推导。
    #[test]
    fn want_includes_prefix() {
        let mut w = StreamWriter::new(8, true);
        assert_eq!(w.want("你好"), "你好");
        w.set_prefix(" ");
        assert_eq!(w.want("你好"), " 你好");
        w.set_prefix("");
        assert_eq!(w.want("你好"), "你好");
    }

    /// reset 要同时清掉前缀 —— 前缀是"这一句"的决策，换句要重新判断。
    #[test]
    fn reset_clears_prefix() {
        let mut w = StreamWriter::new(8, true);
        w.set_prefix(" ");
        w.reset();
        assert_eq!(w.want("下一句"), "下一句");
    }

    /// 前缀变化不能影响"公共前缀"的正确性:
    /// 从" 你好"到" 你好啊" → 公共前缀 3 个字(空格+你好),只需补"啊"。
    #[test]
    fn prefix_participates_in_common_prefix() {
        let mut w = StreamWriter::new(8, true);
        w.set_prefix(" ");
        let a = w.want("你好");
        let b = w.want("你好啊");
        assert_eq!(common_prefix_chars(&a, &b), 3);
        assert_eq!(a.chars().count() - common_prefix_chars(&a, &b), 0); // 不用退格
    }

    /// 安全阀放弃修正时,**记账必须留在屏幕上那一版**。
    ///
    /// 两条路径都不许把 `injected` 推到屏幕前面 —— 一旦对不上,后面每次退格都是
    /// 照「屏幕上有这些字」算的,算出来的退格数会删掉用户真正的文字。
    ///
    /// 这两次调用的差值都**故意超过**安全阀(final 32 / 流式 `max_backspace` 8),
    /// 所以都走「放弃」分支 —— 不会真的 SendInput,测试里可以放心跑。
    #[test]
    fn giveup_keeps_bookkeeping_on_screen() {
        let mut w = StreamWriter::new(8, true);
        // 屏幕上已经有一句(测试里直接摆好状态,不真的敲键盘)。
        // 必须比 final 的 32 字上限长,否则就会走进「真的退格重打」那条路。
        w.injected =
            "已经上屏的这一句 hello world 后面还有一大截没说完还在继续说下去".to_string();
        assert!(w.injected.chars().count() > 32);
        let screen = w.injected.clone();

        // final 超限(> 32):整笔放弃,记账不动
        w.finalize("收口", 32).unwrap();
        assert_eq!(w.injected, screen, "final 放弃后 injected 必须还是屏幕上那一版");

        // 流式超限(> max_backspace 8):同样是放弃且记账不动
        w.update("收口").unwrap();
        assert_eq!(w.injected, screen, "流式放弃后 injected 必须还是屏幕上那一版");
    }
}

/// 流式改写器：把「当前应显示的文本」增量地落到目标窗口。
///
/// 策略：算公共前缀 → 退格掉差异尾巴 → 补打新增部分。
/// `max_backspace` 是安全阀：改动超过这个字数就放弃本次修正，
/// 宁可等下一帧，也不冒删掉用户自己文字的风险。
pub struct StreamWriter {
    injected: String,
    max_backspace: usize,
    enabled: bool,
    /// 这一句要不要在前面补一个空格（用户要的"光标前有字就插空格"）。
    ///
    /// 为什么放在 writer 里而不是在 typing 前拼一下：
    /// 流式改写靠「公共前缀 → 退格差异 → 补打新增」这套算术，**屏幕上的内容必须和
    /// `injected` 完全一致**。如果只是打的时候多敲一个空格、而 `injected` 里没有，
    /// 下一帧算前缀就会错位，退格数算错 → 删到用户自己的字。
    /// 所以空格作为 `prefix` 进入同一条算术：`injected` 始终等于 prefix + 逻辑文本。
    prefix: String,
    pub typed_chars: usize,
    pub backspaces: usize,
}

impl StreamWriter {
    pub fn new(max_backspace: usize, enabled: bool) -> Self {
        Self {
            injected: String::new(),
            max_backspace,
            enabled,
            prefix: String::new(),
            typed_chars: 0,
            backspaces: 0,
        }
    }

    /// 设定这一句的前缀（`" "` 或 `""`）。**每句开始时**由调用方按光标上下文决定一次。
    pub fn set_prefix(&mut self, p: &str) {
        self.prefix = p.to_string();
    }

    pub fn reset(&mut self) {
        self.injected.clear();
        // 前缀属于"这一句"的决策，换句要重新决定（光标位置可能已经变了）
        self.prefix.clear();
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn injected(&self) -> &str {
        &self.injected
    }

    /// 这一句最终应该显示成什么（含前缀）。
    fn want(&self, target: &str) -> String {
        if self.prefix.is_empty() {
            target.to_string()
        } else {
            format!("{}{}", self.prefix, target)
        }
    }

    /// 用 partial 结果更新屏幕。失败不致命，交给下一帧。
    pub fn update(&mut self, target: &str) -> Result<()> {
        if !self.enabled || target.is_empty() {
            // 空结果一律当成「这次没听清」，绝不把已经上屏的字擦掉。
            // final 走 finalize()，那条路径才允许改写/清空。
            return Ok(());
        }
        let want = self.want(target);
        if want == self.injected {
            return Ok(());
        }
        let common = common_prefix_chars(&self.injected, &want);
        let bs = self.injected.chars().count().saturating_sub(common);
        if bs > self.max_backspace {
            return Ok(()); // 改动太大，等 final
        }
        if bs > 0 {
            backspace(bs)?;
            self.backspaces += bs;
        }
        let tail: String = want.chars().skip(common).collect();
        if !tail.is_empty() {
            type_text(&tail)?;
            self.typed_chars += tail.chars().count();
        }
        self.injected = want;
        Ok(())
    }

    /// final 结果：允许更大的回退（final 通常只是把尾巴改得更准）。
    ///
    /// 走和 `update` **完全相同**的前缀算术（含 `prefix`），否则屏幕与 `injected` 会错位。
    pub fn finalize(&mut self, target: &str, max_bs: usize) -> Result<()> {
        let want = self.want(target);
        if !self.enabled {
            self.injected = want;
            return Ok(());
        }
        if want == self.injected {
            return Ok(());
        }
        let common = common_prefix_chars(&self.injected, &want);
        let bs = self.injected.chars().count().saturating_sub(common);
        if bs > max_bs {
            // 超限:整笔放弃,**屏幕和记账都不动**。
            //
            // 这里原来会 `self.injected = want` —— 于是记账跑到了屏幕前面
            // (记账说屏幕上是新文本,屏幕上其实还是旧文本)。之后每一次退格都是
            // 照着「屏幕上有这些字」算的,算出来的退格数就会删掉用户真正的文字;
            // 真机上表现为「说英文的那一句整段不见了」。
            // 宁可这一笔不生效(屏幕留着上一版假设,反正下一句会 reset),
            // 也不能让记账和屏幕对不上。
            return Ok(());
        }
        if bs > 0 {
            backspace(bs)?;
            self.backspaces += bs;
        }
        let tail: String = want.chars().skip(common).collect();
        if !tail.is_empty() {
            type_text(&tail)?;
            self.typed_chars += tail.chars().count();
        }
        self.injected = want;
        Ok(())
    }
}