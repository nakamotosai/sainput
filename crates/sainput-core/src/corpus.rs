//! 每句留一份真实语料:音频 + 识别文本 + 时延,落到 `<日志目录>/corpus/`。
//!
//! 为什么要有它:"换成某个模型到底是不是更准"这个问题,只能拿**用户真实说过的话**
//! 来判(拿合成音频比出来的结论会骗人 —— 2026-10-31 实测:同一个备选模型在自带样例上
//! 看着还行,在用户的真实音频上"验证码"听成"验马")。而这批数据只有在引擎里顺手留下
//! 才拿得到:让用户"专门录十句"是拿不到的 —— 他正忙着用工具干活,不会为调参停下来录音。
//!
//! 边界(必须写清楚,否则这东西就是个"偷偷录音"的黑盒):
//!   * 只留**说话那几秒**(一次按下~定稿之间的采集帧),不留环境音、不留常驻循环;
//!   * 只落在本机,不发往任何网络;
//!   * 有条数与体积上限,超了删**最旧的**(滚动),所以不会无限涨;
//!   * `SAINPUT_CORPUS=off` 一句话关掉(启动时会打一行日志告诉你它在哪、上限多少)。
//!
//! 一条语料 = 一个 wav + `index.jsonl` 里的一行(文本/时延/上限是否触发/帧是否丢过),
//! 于是"跑候选模型做盲测"就是拿 index.jsonl 里的 wav 批量过一遍、再和 `text` 逐句比。

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// 采集就是按这个采样率喂给模型的(`audio::TARGET_RATE`),wav 头照它写。
pub const RATE: u32 = 16_000;

/// 默认上限:200 句 / 300MB。按一句 3 秒 16k 单声道 16bit 算,一句约 96KB,
/// 200 句 ≈ 19MB —— 体积上限是给"偶尔说了很久"兜底的,先到哪个算哪个。
pub const DEFAULT_MAX_ITEMS: usize = 200;
pub const DEFAULT_MAX_MB: u64 = 300;

pub struct Corpus {
    dir: PathBuf,
    enabled: bool,
    max_items: usize,
    max_mb: u64,
    /// 本次会话的文件名前缀(进程启动时刻),和句序号拼成 `20261031-220423-0007.wav`,
    /// 所以按文件名排序就是**时间顺序** —— 滚动清理靠这个挑最旧的。
    stamp: String,
    seq: u64,
    /// 这一句的采集帧(只在按下~定稿之间攒)。
    cur: Vec<i16>,
    /// `begin()` 之后、`finish()` 之前才收帧:防止上一句的尾巴帧漏进下一句的文件。
    armed: bool,
}

impl Corpus {
    pub fn new(dir: PathBuf, stamp: String) -> Corpus {
        let enabled = !matches!(
            std::env::var("SAINPUT_CORPUS")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "off" | "0" | "no" | "false" | "disable" | "disabled"
        );
        let max_items = env_num("SAINPUT_CORPUS_MAX", DEFAULT_MAX_ITEMS as u64).max(1) as usize;
        let max_mb = env_num("SAINPUT_CORPUS_MB", DEFAULT_MAX_MB).max(1);
        Corpus {
            dir,
            enabled,
            max_items,
            max_mb,
            stamp,
            seq: 0,
            cur: Vec::new(),
            armed: false,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 明确关掉。给"用户关了会话日志"这种情况用:没有日志就不该偷偷留音频。
    pub fn off() -> Corpus {
        let mut c = Corpus::new(PathBuf::new(), String::new());
        c.enabled = false;
        c
    }

    pub fn path(&self) -> String {
        self.dir.display().to_string()
    }

    pub fn limits(&self) -> String {
        format!("上限 {} 句 / {}MB", self.max_items, self.max_mb)
    }

    /// 现在目录里有多少句语料(给日志用:数的是**实际文件**,不是自增计数 ——
    /// 清理删掉的不会算进来,对不上时一眼能看出来)。
    pub fn written(&self) -> usize {
        fs::read_dir(&self.dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".wav"))
                    .count()
            })
            .unwrap_or(0)
    }

    /// 新的一句开始采集。
    pub fn begin(&mut self) {
        if !self.enabled {
            return;
        }
        self.cur.clear();
        self.armed = true;
    }

    /// 采集帧(按下~定稿之间)。第 1 句的尾巴帧也走这里,和真音频一起留在同一个文件里。
    pub fn push(&mut self, pcm: &[i16]) {
        if self.enabled && self.armed {
            self.cur.extend_from_slice(pcm);
        }
    }

    /// 这一句定稿:写 wav + 追加 index 一行 + 滚动清理。
    ///
    /// `row` 由调用方给(识别文本/时延这些它手上才有的东西),这里补上音频字段。
    /// 返回落盘的 wav 路径(没写就成了 None:没开、没音频、或者早写过了)。
    pub fn finish(&mut self, mut row: serde_json::Value) -> Option<String> {
        if !self.enabled || !self.armed || self.cur.is_empty() {
            self.armed = false;
            self.cur.clear();
            return None;
        }
        self.armed = false;
        self.seq += 1;
        let name = format!("{}-{:04}.wav", self.stamp, self.seq);
        let path = self.dir.join(&name);
        if fs::create_dir_all(&self.dir).is_err() {
            self.cur.clear();
            return None;
        }
        // 用**既有**的 wav 写出器(crates/sainput-core/src/wav.rs),不另写一份头 ——
        // 两份 WAV 实现迟早会分叉,而且那份已经有 `--replay`/`devices` 在用。
        if let Err(e) = crate::wav::write_pcm16_mono(&path.display().to_string(), RATE, &self.cur) {
            eprintln!("[sainput] 语料写盘失败({name}): {e}");
            self.cur.clear();
            return None;
        }
        let dur_ms = self.cur.len() as f64 * 1000.0 / RATE as f64;
        if let Some(o) = row.as_object_mut() {
            o.insert("wav".into(), serde_json::Value::String(name.clone()));
            o.insert("samples".into(), serde_json::Value::from(self.cur.len()));
            o.insert("dur_ms".into(), serde_json::Value::from(dur_ms.round() as u64));
            o.insert("rate".into(), serde_json::Value::from(RATE));
        }
        self.cur.clear();
        append_index(&self.dir, &row);
        self.prune();
        Some(path.display().to_string())
    }

    /// 滚动清理:按文件名(=时间)删最旧的,直到条数与体积都在上限内;
    /// 顺手把 index.jsonl 重写成"只留还活着的 wav",不让它和目录对不上。
    fn prune(&self) {
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut wavs: Vec<(String, u64)> = rd
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                if !n.ends_with(".wav") {
                    return None;
                }
                let sz = e.metadata().ok()?.len();
                Some((n, sz))
            })
            .collect();
        wavs.sort();
        let mut total: u64 = wavs.iter().map(|(_, s)| *s).sum();
        let mut keep_from = 0usize;
        while keep_from + 1 < wavs.len()
            && (wavs.len() - keep_from > self.max_items || total > self.max_mb * 1024 * 1024)
        {
            total -= wavs[keep_from].1;
            keep_from += 1;
        }
        if keep_from == 0 {
            return;
        }
        for (n, _) in &wavs[..keep_from] {
            let _ = fs::remove_file(self.dir.join(n));
        }
        rewrite_index(&self.dir, &wavs[keep_from..]);
    }
}

fn env_num(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// 单声道 16bit PCM wav 由 `crate::wav::write_pcm16_mono` 负责(不在这里再写一份)。

fn append_index(dir: &Path, row: &serde_json::Value) {
    let path = dir.join("index.jsonl");
    let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(f, "{row}");
}

/// 清理之后把 index 对齐:只保留 wav 还在的那些行(其余行指向的文件已删)。
fn rewrite_index(dir: &Path, alive: &[(String, u64)]) {
    let path = dir.join("index.jsonl");
    let Ok(txt) = fs::read_to_string(&path) else {
        return;
    };
    let mut out = String::new();
    for line in txt.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(w) = v.get("wav").and_then(|x| x.as_str()) else {
            continue;
        };
        if alive.iter().any(|(n, _)| n == w) {
            let _ = writeln!(out, "{v}");
        }
    }
    let _ = fs::write(&path, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sainput-corpus-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// 落盘的 wav 必须能被**读侧**读回来(读侧是 `--replay`/`devices` 在用的那条,
    /// 也是我拿去批量跑候选模型的那条) —— 只查自己写的常量不算数。
    ///
    /// 允许 **±1 LSB**:读侧是 `i16/32768.0` 进来、`f32*32767.0` 出去(见 wav.rs),
    /// 对负数会在最后一位**向零截断**一次(-50 → -49)。这是那条既有读侧的行为,
    /// 归零到 1/32768 ≈ 0.003% 的量级,对识别和试听都没有意义 —— 但"完全相等"是
    /// 错的断言,这里就把它钉成真实不变式,免得下次有人以为坏了。
    #[test]
    fn written_wav_round_trips_through_the_reader() {
        let d = tmp("wav");
        let mut c = Corpus::new(d.clone(), "20260101-000000".into());
        let samples: Vec<i16> = (0..1600).map(|i| (i % 100) as i16 - 50).collect();
        c.begin();
        c.push(&samples);
        let p = c.finish(serde_json::json!({ "text": "甲" })).unwrap();
        let back = crate::wav::read_as_pcm16_mono_16k(&p).unwrap();
        assert_eq!(back.len(), samples.len(), "样本数必须一致(不能多也不能少)");
        let worst = samples
            .iter()
            .zip(&back)
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        assert!(worst <= 1, "每个样本的偏差都得 ≤1 LSB,实际最大 {worst}");
    }

    /// 滚动清理:超上限删**最旧的**,且 index 里不能留下指向已删文件的行。
    #[test]
    fn prune_drops_oldest_and_keeps_index_consistent() {
        let d = tmp("prune");
        let mut c = Corpus {
            dir: d.clone(),
            enabled: true,
            max_items: 2,
            max_mb: 1,
            stamp: "20260101-000000".into(),
            seq: 0,
            cur: Vec::new(),
            armed: false,
        };
        for i in 1..=4u64 {
            c.begin();
            c.push(&vec![i as i16; 160]);
            c.finish(serde_json::json!({ "text": format!("第{i}句") }))
                .unwrap();
        }
        let mut wavs: Vec<String> = fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".wav"))
            .collect();
        wavs.sort();
        assert_eq!(wavs.len(), 2, "上限 2 句:目录里只该剩 2 个 wav,实际 {wavs:?}");
        assert!(wavs[0].ends_with("-0003.wav"), "该删最旧的,剩下的是 {wavs:?}");
        assert!(wavs[1].ends_with("-0004.wav"), "剩下的是 {wavs:?}");
        let idx = fs::read_to_string(d.join("index.jsonl")).unwrap();
        let lines: Vec<&str> = idx.lines().collect();
        assert_eq!(lines.len(), 2, "index 行数必须和活着的 wav 对上:\n{idx}");
        assert!(lines.iter().all(|l| l.contains("-0003.wav") || l.contains("-0004.wav")));
    }

    /// `armed` 的边界:没 begin 就 push 不许进文件(否则上一句的帧会漏进下一句)。
    #[test]
    fn frames_outside_a_sentence_are_not_recorded() {
        let d = tmp("armed");
        let mut c = Corpus::new(d.clone(), "20260101-000000".into());
        c.push(&vec![7i16; 100]); // 还没 begin
        assert!(c.finish(serde_json::json!({})).is_none(), "没有句子的帧不该落盘");
        c.begin();
        c.push(&vec![9i16; 100]);
        assert!(c.finish(serde_json::json!({ "text": "甲" })).is_some());
        // 写完之后继续来帧(下一句的尾巴)不许再被收集
        c.push(&vec![11i16; 100]);
        assert!(c.finish(serde_json::json!({})).is_none(), "写完了就不该再有第二份");
    }
}
