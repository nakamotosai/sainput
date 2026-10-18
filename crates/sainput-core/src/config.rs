//! 极简配置：仓库根下 `sainput.conf`，一行一个 `key = value`。
//!
//! 为什么要它：用户不该为了换个热键去改 .bat。命令行参数永远优先于配置文件，
//! 所以「配置文件写日常偏好，命令行做临时覆盖」这条链是通的。
//!
//! 支持的行形态：
//!   hotkey = f9
//!   model_dir = models/sherpa-onnx-streaming-zipformer-zh-14M-2023-02-23
//!   punct = false
//!   # 这是注释；`;` 开头的也算注释

use std::collections::HashMap;
use std::path::Path;

#[derive(Default, Debug, Clone)]
pub struct Settings {
    map: HashMap<String, String>,
    pub source: Option<String>,
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let mut map = HashMap::new();
        let mut source = None;
        if let Ok(txt) = std::fs::read_to_string(path) {
            source = Some(path.display().to_string());
            for line in txt.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    let k = k.trim().to_ascii_lowercase().replace('-', "_");
                    let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
                    if !k.is_empty() && !v.is_empty() {
                        map.insert(k, v);
                    }
                }
            }
        }
        Self { map, source }
    }

    pub fn s(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }

    pub fn b(&self, key: &str) -> Option<bool> {
        self.s(key).map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on" | "y" | "开" | "是"
            )
        })
    }

    pub fn us(&self, key: &str) -> Option<usize> {
        self.s(key).and_then(|v| v.trim().parse::<usize>().ok())
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn keys(&self) -> Vec<String> {
        let mut v: Vec<String> = self.map.keys().cloned().collect();
        v.sort();
        v
    }
}

/// 把单个键写回配置文件（托盘开关用）。
///
/// 为什么不直接重写整个文件：`sainput.conf` 里全是**带解释的注释**，那才是这份文件的
/// 价值所在。重写会把注释全丢掉，用户下次打开就只剩一堆光秃秃的键。
/// 所以策略是「就地改一行」：
///   · 文件里有 `key = ...` 的**生效行** → 改它（**只改第一处，其余重复的删掉**）；
///   · 没有生效行、但有 `# key = ...` 的**注释行** → 在该注释行下方插入一行生效值；
///   · 都没有 → 追加到文件末尾。
/// 关键：**绝不删注释**。失败也不致命（开关本次仍生效，只是下次启动会回到文件里的值）。
///
/// 踩过的坑：早先的实现「插入时把生效值放到已 push 的内容之后」，
/// 导致第二次调用找不到刚插进去的那行 → **每次都追加一行**，连点几次就堆一串重复键。
/// 现在改成两遍扫描：先决定「替哪一行」，再一次性拼出来。
pub fn save_key(path: &Path, key: &str, value: &str) -> std::io::Result<()> {
    let orig = std::fs::read_to_string(path).unwrap_or_default();
    let key_lc = key.to_ascii_lowercase().replace('-', "_");
    let lines: Vec<&str> = orig.lines().collect();

    // 解析一行：返回 (是不是注释, 键名)
    let parse = |line: &str| -> (bool, Option<String>) {
        let trimmed = line.trim();
        let is_comment = trimmed.starts_with('#') || trimmed.starts_with(';');
        let body = if is_comment {
            trimmed.trim_start_matches(['#', ';']).trim()
        } else {
            trimmed
        };
        let k = body
            .split_once('=')
            .map(|(k, _)| k.trim().to_ascii_lowercase().replace('-', "_"));
        (is_comment, k)
    };

    // 第一遍：找出所有「生效行」的下标，和第一条「注释行」的下标
    let mut active: Vec<usize> = Vec::new();
    let mut commented: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let (is_comment, k) = parse(line);
        if k.as_deref() == Some(key_lc.as_str()) {
            if is_comment {
                if commented.is_none() {
                    commented = Some(i);
                }
            } else {
                active.push(i);
            }
        }
    }

    let new_line = format!("{key} = {value}");
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 2);
    if let Some(&first) = active.first() {
        // 有生效行：第一处换成新值，后面重复的生效行丢掉（去重）
        for (i, line) in lines.iter().enumerate() {
            if i == first {
                out.push(new_line.clone());
            } else if active.contains(&i) {
                // 重复的生效行：跳过，不再写出去
            } else {
                out.push((*line).to_string());
            }
        }
    } else if let Some(ci) = commented {
        // 没有生效行，但有注释行：在注释行**下面**插一行生效值（保留注释做说明）
        for (i, line) in lines.iter().enumerate() {
            out.push((*line).to_string());
            if i == ci {
                out.push(new_line.clone());
            }
        }
    } else {
        // 全新键：追加到末尾（前面补一个空行，读起来清爽）
        out.extend(lines.iter().map(|s| (*s).to_string()));
        if !out.is_empty() && !out.last().map(|s| s.trim().is_empty()).unwrap_or(true) {
            out.push(String::new());
        }
        out.push(new_line.clone());
    }

    let mut text = out.join("\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }

    // 原子写：先写临时文件再改名，避免写一半断电留下残缺配置
    let tmp = path.with_extension("conf.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}