//! 上下文偏置的 **Rust 侧**：只负责「从一段文本里抽术语」和「把术语写成热词文件」。
//!
//! 为什么真正的 UIA 读取不在这里：
//!   读焦点输入框要靠 UI Automation 的 COM 接口，手写 vtable 偏移一旦对不上就是**静默内存错乱**
//!   （这正是本项目用「子进程 + JSON」隔离 sherpa-onnx 的同一个理由，见 asr.rs 文件头）。
//!   Python 侧已经装了 `uiautomation`，所以那边读、这里只做纯文本处理。
//!   分工：`tools/ctx_probe.py --print` 读出光标前文 → 本模块 `extract_terms` 抽词 →
//!         run 主循环写进 `.ctx_hotwords.txt` → sidecar 每句开始当热词喂给识别器。
//!
//! 与云端方案的差异（照实说）：云端方案把你的历史文本打包上传、由**云端模型**做偏置；
//! 我们只能喂**本地热词表**。方向一致（让识别贴合你的用词），强度不如云端。

use std::path::Path;

/// 抽取上限。太多会把热词表冲稀、还会拖慢 beam search（热词强制走 modified_beam_search）。
const MAX_TERMS: usize = 24;
/// 只看光标前这么多个字符。
const LOOKBACK_CHARS: usize = 600;

/// 从一段文本里抽出候选偏置词。
///
/// 只抽**含 ASCII 字母或数字**的串（英文词/缩写/版本号/路径/域名）。
/// 为什么不要纯汉字串：热词表里塞进常用中文词只会干扰识别（`hotwords.txt` 的注释也这么说），
/// 而且中文热词还要 bpe 分词才有意义 —— 收益低、风险高。术语（技术词、人名拼写）才是它的目标。
pub fn extract_terms(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();

    fn flush(cur: &mut String, out: &mut Vec<String>) {
        let w = cur.trim();
        if w.chars().count() >= 2 && w.chars().any(|c| c.is_ascii_alphanumeric()) {
            let s = w.to_string();
            if !out.iter().any(|x| x == &s) {
                out.push(s);
            }
        }
        cur.clear();
    }

    for ch in text.chars() {
        // 术语里允许的字符：字母数字 + 常见的词内连接符（sherpa-onnx / 0.2.1 / F:\path / a.com）
        let keep = ch.is_ascii_alphanumeric()
            || matches!(ch, '_' | '-' | '.' | '/' | ':' | '+' | '#' | '@' | '\\');
        if keep {
            cur.push(ch);
        } else {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);

    // 去掉只有符号的残留（例如 ".."、"--"）
    out.retain(|w| w.chars().any(|c| c.is_ascii_alphanumeric()));
    // 长词信息量大，优先保留
    out.sort_by(|a, b| b.len().cmp(&a.len()));
    out.truncate(MAX_TERMS);
    out
}

/// 取末 `LOOKBACK_CHARS` 个字符（光标前文才和这一句相关）。
pub fn tail_chars(text: &str) -> String {
    let n = text.chars().count();
    if n > LOOKBACK_CHARS {
        text.chars().skip(n - LOOKBACK_CHARS).collect()
    } else {
        text.to_string()
    }
}

/// 把术语写成 sidecar 认的热词文件（一行一个词，`#` 开头是注释）。
pub fn write_hotwords(path: &Path, terms: &[String]) -> std::io::Result<()> {
    let mut s = String::from(
        "# 自动生成：由 sainput 根据光标前文提取的上下文偏置词。\n\
         # 手动加词请写 hotwords.txt；本文件每次采集会被覆盖。\n",
    );
    for t in terms {
        s.push_str(t);
        s.push('\n');
    }
    std::fs::write(path, s)
}

/// 已上屏文本的滚动历史 —— 上下文偏置的**主来源**。
///
/// 为什么用它而不是读输入框：云端方案喂的也是它自己刚发出去的文本
/// （本项目对应 `SendHistory`，日志 `[SendHistory][Append]` / `[committed]`）。
/// 我们已经知道每句打了什么，**不需要**去读目标窗口 —— 这条在本机永远可用，
/// 也不碰 UIA（UIA 在这台机器上因为 comtypes 生成目录只读而不可用，见 tools/ctx_probe.py 注释）。
#[derive(Default)]
pub struct SendHistory {
    /// 最近若干句的**原文**（不是渲染后文本 —— 偏置要的是"用户用了哪些词"，
    /// 渲染层做过标点/空格整形，反而会污染术语抽取）。
    recent: Vec<String>,
}

/// 保留最近几句。太少偏置不足，太多会把热词表冲稀（这次的词才是最有用的）。
const HISTORY_UTTERANCES: usize = 6;

impl SendHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一句已定稿的原文。
    pub fn push(&mut self, raw: &str) {
        let t = raw.trim();
        if t.is_empty() {
            return;
        }
        self.recent.push(t.to_string());
        while self.recent.len() > HISTORY_UTTERANCES {
            self.recent.remove(0);
        }
    }

    /// 连续喂了 `n` 句空结果 → 说明用户在别的窗口干活，历史已经过时，清掉。
    pub fn clear(&mut self) {
        self.recent.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.recent.is_empty()
    }

    /// 记了几句。
    pub fn len(&self) -> usize {
        self.recent.len()
    }

    /// 从历史里抽出偏置词（最近的句子权重高：放在前面，`extract_terms` 会去重且按长度排）。
    pub fn terms(&self) -> Vec<String> {
        let joined = self.recent.join(" ");
        extract_terms(&joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_ascii_terms_not_plain_chinese() {
        let t = extract_terms("我在用 sainput 和 sherpa-onnx，路径是 C:\\work\\demo，版本 0.2.1");
        assert!(t.iter().any(|x| x == "sainput"), "got {t:?}");
        assert!(t.iter().any(|x| x == "sherpa-onnx"), "got {t:?}");
        assert!(t.iter().any(|x| x == "0.2.1"), "got {t:?}");
        assert!(!t.iter().any(|x| x == "我在用"), "纯中文不该进热词表: {t:?}");
    }

    #[test]
    fn caps_term_count() {
        let many: String = (0..200).map(|i| format!("term{i} ")).collect();
        assert!(extract_terms(&many).len() <= MAX_TERMS);
    }

    #[test]
    fn dedups() {
        let t = extract_terms("sainput sainput sainput");
        assert_eq!(t.iter().filter(|x| *x == "sainput").count(), 1);
    }

    #[test]
    fn empty_and_junk_safe() {
        assert!(extract_terms("").is_empty());
        assert!(extract_terms("。。。，，，").is_empty());
        assert!(extract_terms("... ---").is_empty());
    }

    #[test]
    fn tail_limits_chars() {
        let long: String = std::iter::repeat('x').take(2000).collect();
        assert_eq!(tail_chars(&long).chars().count(), LOOKBACK_CHARS);
    }

    #[test]
    fn history_keeps_only_recent() {
        let mut h = SendHistory::new();
        for i in 0..20 {
            h.push(&format!("sentence{i} word{i}"));
        }
        // 最早的应被挤掉，最近的还在
        let t = h.terms();
        assert!(t.iter().any(|x| x == "sentence19"), "got {t:?}");
        assert!(!t.iter().any(|x| x == "sentence0"), "不该留最早那句: {t:?}");
    }

    #[test]
    fn history_ignores_empty() {
        let mut h = SendHistory::new();
        h.push("");
        h.push("   ");
        assert!(h.is_empty());
        assert!(h.terms().is_empty());
    }

    #[test]
    fn history_terms_find_technical_words() {
        let mut h = SendHistory::new();
        h.push("我在调 sherpa-onnx 的 0.2.1");
        let t = h.terms();
        assert!(t.iter().any(|x| x == "sherpa-onnx"), "got {t:?}");
        assert!(t.iter().any(|x| x == "0.2.1"), "got {t:?}");
    }
}
