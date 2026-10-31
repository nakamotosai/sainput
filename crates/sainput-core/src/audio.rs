//! 麦克风采集 → 16 kHz / 单声道 / i16 定长帧。
//!
//! 参数按实测结论固定：16 kHz、单声道、16 bit。
//! 喂帧粒度 32 ms（512 样本），与 sherpa-onnx 官方示例一致。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

/// 目标采样率：语音前端统一到 16 kHz。
pub const TARGET_RATE: u32 = 16_000;
/// 每帧时长（毫秒）。
pub const FRAME_MS: u32 = 32;
/// 每帧样本数 = 512。
pub const FRAME_SAMPLES: usize = (TARGET_RATE * FRAME_MS / 1000) as usize;

/// 输入设备基本信息。
#[derive(Debug, Clone)]
pub struct MicInfo {
    pub name: String,
    pub is_default: bool,
    pub channels: u16,
    pub sample_rate: u32,
    pub sample_format: String,
}

/// 采集统计（原子计数，可在采集线程外读取）。
#[derive(Debug, Default)]
pub struct CaptureStats {
    /// 已产出的 16 kHz 定长帧数。
    pub frames_out: AtomicU64,
    /// 从驱动收到的原始样本总数（降混前）。
    pub samples_in: AtomicU64,
    /// 下游未及时取走而被丢弃的帧数 —— 这是采集路径拥塞的直接指标。
    pub frames_dropped: AtomicU64,
    /// **回调里被迫新开缓冲区**的次数。
    ///
    /// 这是 T6.3「回调内零分配」的可机检判据:下游把用完的帧缓冲还回来(见
    /// `Capture::recycle`),稳态下这个数**不再增长**。它涨了 = 帧池不够用,
    /// 那时 Collection 回调就又变成会分配的地方了(实时线程最忌讳的)。
    pub pool_misses: AtomicU64,
    /// 采集线程**从回收通道拿到**缓冲的次数(命中)—— 和 `pool_misses` 相加应等于出帧数。
    pub pool_reuse: AtomicU64,
    /// 想还但没还进去的数量(回收通道满了/容量不够)—— 不为 0 说明回收这条路有梗。
    pub recycle_failed: AtomicU64,
    /// cpal 回调报错次数。
    pub stream_errors: AtomicU64,
}

/// 枚举本机所有输入设备。
pub fn list_mics() -> anyhow::Result<Vec<MicInfo>> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());
    let mut out = Vec::new();
    for dev in host.input_devices()? {
        let name = dev.name().unwrap_or_else(|_| "<unnamed>".to_string());
        let (channels, sample_rate, sample_format) = match dev.default_input_config() {
            Ok(c) => (c.channels(), c.sample_rate().0, format!("{:?}", c.sample_format())),
            Err(e) => (0, 0, format!("unavailable: {e}")),
        };
        out.push(MicInfo {
            is_default: default_name.as_deref() == Some(name.as_str()),
            name,
            channels,
            sample_rate,
            sample_format,
        });
    }
    Ok(out)
}

/// 跨回调块连续工作的线性重采样器。
///
/// M0 用线性插值足够打表；M1 会换成 rubato / speexdsp。
struct LinearResampler {
    step: f64,
    pos: f64,
    last: f32,
    primed: bool,
}

impl LinearResampler {
    fn new(src_rate: u32, dst_rate: u32) -> Self {
        Self {
            step: src_rate as f64 / dst_rate as f64,
            pos: 0.0,
            last: 0.0,
            primed: false,
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        if !self.primed {
            self.last = input[0];
            self.primed = true;
        }
        while (self.pos as usize) < input.len() {
            let idx = self.pos as usize;
            let frac = (self.pos - idx as f64) as f32;
            let a = if idx == 0 { self.last } else { input[idx - 1] };
            let b = input.get(idx).copied().unwrap_or(a);
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= input.len() as f64;
        if self.pos < 0.0 {
            self.pos = 0.0;
        }
        self.last = *input.last().unwrap();
    }
}

/// 采集回调持有的状态：降混 → 重采样 → 组帧 → 送通道。
struct CaptureState {
    rs: LinearResampler,
    mono: Vec<f32>,
    /// 重采样 scratch:**复用**(以前每次回调 `Vec::with_capacity` 新开一个)。
    resampled: Vec<f32>,
    acc: Vec<f32>,
    /// 手边备用的帧缓冲(下游还没还回来时用)。
    pool: Vec<Vec<i16>>,
    /// 下游还回来的帧缓冲 —— 实时回调靠它做到零分配。
    free_rx: Receiver<Vec<i16>>,
    tx: SyncSender<Vec<i16>>,
    stats: Arc<CaptureStats>,
    channels: usize,
}

impl CaptureState {
    fn new(
        src_rate: u32,
        channels: u16,
        tx: SyncSender<Vec<i16>>,
        free_rx: Receiver<Vec<i16>>,
        stats: Arc<CaptureStats>,
    ) -> Self {
        Self {
            rs: LinearResampler::new(src_rate, TARGET_RATE),
            mono: Vec::with_capacity(1024),
            resampled: Vec::with_capacity(1024),
            acc: Vec::with_capacity(FRAME_SAMPLES * 4),
            pool: Vec::new(),
            free_rx,
            tx,
            stats,
            channels: (channels.max(1)) as usize,
        }
    }

    /// 拿一个帧缓冲:先要下游还回来的,再要手边备用的,**都没有才新开一个**
    /// (并记一次 `pool_misses`)。稳态下走不到最后那步。
    fn take_frame_buf(&mut self) -> Vec<i16> {
        if let Ok(mut v) = self.free_rx.try_recv() {
            v.clear();
            self.stats.pool_reuse.fetch_add(1, Ordering::Relaxed);
            return v;
        }
        if let Some(mut v) = self.pool.pop() {
            v.clear();
            return v;
        }
        self.stats.pool_misses.fetch_add(1, Ordering::Relaxed);
        Vec::with_capacity(FRAME_SAMPLES)
    }

    fn feed<T: Copy, F: Fn(T) -> f32>(&mut self, data: &[T], conv: F) {
        self.stats
            .samples_in
            .fetch_add(data.len() as u64, Ordering::Relaxed);

        // 1) 降混到单声道
        self.mono.clear();
        for frame in data.chunks_exact(self.channels) {
            let sum: f32 = frame.iter().map(|s| conv(*s)).sum();
            self.mono.push(sum / self.channels as f32);
        }

        // 2) 重采样到 16 kHz(scratch 复用,不再每次回调新开 Vec)
        self.resampled.clear();
        self.rs.process(&self.mono, &mut self.resampled);

        // 3) 攒定长帧并送出
        self.acc.extend_from_slice(&self.resampled);
        while self.acc.len() >= FRAME_SAMPLES {
            let mut frame = self.take_frame_buf();
            frame.extend(
                self.acc[..FRAME_SAMPLES]
                    .iter()
                    .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16),
            );
            // `drain(..n)` 每次都要把剩下的往前挪一遍;`copy_within` + truncate 一样是原地挪,
            // 但少一层迭代器/边界检查(这条路径在每个音频回调里都跑)。
            let rest = self.acc.len() - FRAME_SAMPLES;
            self.acc.copy_within(FRAME_SAMPLES.., 0);
            self.acc.truncate(rest);
            self.stats.frames_out.fetch_add(1, Ordering::Relaxed);
            if self.tx.try_send(frame).is_err() {
                self.stats.frames_dropped.fetch_add(1, Ordering::Relaxed);
            } else {
                // 没发出去的那份缓冲还能用,留着别丢
            }
        }
    }
}

/// 一个正在运行的采集会话。drop 它会停止采集。
pub struct Capture {
    _stream: cpal::Stream,
    pub rx: Receiver<Vec<i16>>,
    free_tx: SyncSender<Vec<i16>>,
    pub stats: Arc<CaptureStats>,
    pub src_sample_rate: u32,
    pub src_channels: u16,
    pub device_name: String,
}

impl Capture {
    /// 把用完的帧缓冲还回去。**采集方靠这个做到回调内零分配** ——
    /// 下游每丢一个 `Vec` 就是让实时线程以后多分配一次,所以用完就还。
    /// 通道满了(mpsc 的有界队列,极罕见)就让它自然析构,不算错误。
    pub fn recycle(&self, buf: Vec<i16>) {
        if buf.capacity() < FRAME_SAMPLES {
            self.stats.recycle_failed.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.free_tx.try_send(buf).is_err() {
            self.stats.recycle_failed.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 启动采集。`device_name` 为 None 时用系统默认输入设备。
pub fn start(device_name: Option<&str>) -> anyhow::Result<Capture> {
    let host = cpal::default_host();
    let dev = match device_name {
        Some(want) => host
            .input_devices()?
            .find(|d| d.name().map(|n| n == want).unwrap_or(false))
            .ok_or_else(|| anyhow::anyhow!("找不到麦克风: {want}"))?,
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("系统没有默认输入设备"))?,
    };

    let name = dev.name().unwrap_or_else(|_| "<unnamed>".to_string());
    let supported = dev.default_input_config()?;
    let src_sample_rate = supported.sample_rate().0;
    let src_channels = supported.channels();
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    let (tx, rx) = mpsc::sync_channel::<Vec<i16>>(64);
    // 回收通道:下游用完把缓冲还回来,采集线程复用(容量给足,别让"还"变成阻塞)
    let (free_tx, free_rx) = mpsc::sync_channel::<Vec<i16>>(64);
    let stats = Arc::new(CaptureStats::default());
    let err_stats = Arc::clone(&stats);
    let err_fn = move |e: cpal::StreamError| {
        err_stats.stream_errors.fetch_add(1, Ordering::Relaxed);
        eprintln!("[audio] stream error: {e}");
    };

    let stream = match sample_format {
        SampleFormat::F32 => {
            let mut st = CaptureState::new(src_sample_rate, src_channels, tx, free_rx, Arc::clone(&stats));
            dev.build_input_stream(&config, move |d: &[f32], _| st.feed(d, |x| x), err_fn, None)?
        }
        SampleFormat::I16 => {
            let mut st = CaptureState::new(src_sample_rate, src_channels, tx, free_rx, Arc::clone(&stats));
            dev.build_input_stream(
                &config,
                move |d: &[i16], _| st.feed(d, |x| x as f32 / 32768.0),
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let mut st = CaptureState::new(src_sample_rate, src_channels, tx, free_rx, Arc::clone(&stats));
            dev.build_input_stream(
                &config,
                move |d: &[u16], _| st.feed(d, |x| (x as f32 - 32768.0) / 32768.0),
                err_fn,
                None,
            )?
        }
        other => anyhow::bail!("暂不支持采样格式 {other:?}（当前只处理 F32/I16/U16）"),
    };
    stream.play()?;

    Ok(Capture {
        _stream: stream,
        rx,
        free_tx,
        stats,
        src_sample_rate,
        src_channels,
        device_name: name,
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    /// T6.3 的判据:回调**稳态零分配** —— 也就是帧缓冲靠下游回收循环起来。
    ///
    /// 为什么必须做成测试而不是"看代码觉得对":`pool_misses` 是运行时计数,
    /// 只有真跑一遍才知道池子够不够。这里直接驱动 `CaptureState::feed`
    /// (回调里跑的就是这个方法),第一轮收帧、还帧,第二轮不该再新开缓冲。
    #[test]
    fn 帧池自给自足_稳态不再分配() {
        let (tx, rx) = mpsc::sync_channel::<Vec<i16>>(64);
        let (free_tx, free_rx) = mpsc::sync_channel::<Vec<i16>>(64);
        let stats = Arc::new(CaptureStats::default());
        // 源采样率 == 目标采样率:pure 直通,便于核"没有丢样本/没有重复样本"
        let mut st = CaptureState::new(TARGET_RATE, 1, tx, free_rx, Arc::clone(&stats));

        let input: Vec<f32> = (0..FRAME_SAMPLES * 4)
            .map(|i| ((i % 97) as f32 / 97.0) - 0.5)
            .collect();

        // 参照物:同一段输入过一个**独立的**重采样器,它吐出来的序列就是帧该拼成的东西。
        let mut oracle = LinearResampler::new(TARGET_RATE, TARGET_RATE);
        let mut want = Vec::new();
        oracle.process(&input, &mut want);

        st.feed(&input, |x| x);
        let mut got: Vec<Vec<i16>> = Vec::new();
        while let Ok(f) = rx.try_recv() {
            got.push(f);
        }
        assert_eq!(got.len(), want.len() / FRAME_SAMPLES, "整帧数要对");
        assert!(
            got.iter().all(|f| f.len() == FRAME_SAMPLES),
            "每帧必须是定长 {FRAME_SAMPLES}"
        );
        let flat: Vec<i16> = got.iter().flatten().copied().collect();
        let want_i16: Vec<i16> = want
            .iter()
            .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .collect();
        assert_eq!(flat, want_i16, "组帧不能丢样本/重复样本");

        // 还回去 → 第二轮不该再分配任何帧缓冲
        for f in got {
            free_tx.try_send(f).expect("回收通道不该满");
        }
        let before = stats.pool_misses.load(Ordering::Relaxed);
        st.feed(&input, |x| x);
        let mut n = 0;
        while let Ok(f) = rx.try_recv() {
            n += 1;
            free_tx.try_send(f).expect("回收通道不该满");
        }
        assert_eq!(n, want.len() / FRAME_SAMPLES);
        assert_eq!(
            stats.pool_misses.load(Ordering::Relaxed),
            before,
            "稳态下回调不该再新开缓冲区(T6.3)"
        );
    }

    /// 降混:多声道要平均,不能只取第一路(取一路会让定位/音量都不对)。
    #[test]
    fn 多声道降混是平均() {
        let (tx, rx) = mpsc::sync_channel::<Vec<i16>>(8);
        let (_free_tx, free_rx) = mpsc::sync_channel::<Vec<i16>>(8);
        let stats = Arc::new(CaptureStats::default());
        let mut st = CaptureState::new(TARGET_RATE, 2, tx, free_rx, stats);
        // 喂够一整帧:左 1.0 / 右 0.0 交替(len 按帧长的两倍,保证出得来一帧)
        let input: Vec<f32> = (0..FRAME_SAMPLES * 2)
            .map(|i| if i % 2 == 0 { 1.0 } else { 0.0 })
            .collect();
        st.feed(&input, |x| x);
        let mut first = None;
        while let Ok(f) = rx.try_recv() {
            first.get_or_insert(f);
        }
        let f = first.expect("该出一帧");
        assert_eq!(f[0], (0.5f32 * 32767.0) as i16, "两声道 1.0/0.0 → 0.5");
    }
}
