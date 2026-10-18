//! 延迟打表。
//!
//! 口径（本地语音输入的常见埋点划分）：input_ms / commit_ms / ui_ms / total_ms。
//! 没有这套表就无法优化 —— 先把测量做对，再谈调优。

/// 一组延迟样本的分布摘要。单位由调用方决定（本模块统一按毫秒使用）。
#[derive(Debug, Clone, Copy)]
pub struct Summary {
    pub n: usize,
    pub min: f64,
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub max: f64,
    pub mean: f64,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "n={} min={:.1} p50={:.1} p90={:.1} p95={:.1} max={:.1} mean={:.1}",
            self.n, self.min, self.p50, self.p90, self.p95, self.max, self.mean
        )
    }
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// 汇总一组样本。空输入返回 None（而不是张冠李戴的 0）。
pub fn summarize(mut v: Vec<f64>) -> Option<Summary> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    let mean = v.iter().sum::<f64>() / n as f64;
    Some(Summary {
        n,
        min: v[0],
        p50: percentile(&v, 0.50),
        p90: percentile(&v, 0.90),
        p95: percentile(&v, 0.95),
        max: v[n - 1],
        mean,
    })
}