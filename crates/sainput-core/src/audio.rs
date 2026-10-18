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
    acc: Vec<f32>,
    tx: SyncSender<Vec<i16>>,
    stats: Arc<CaptureStats>,
    channels: usize,
}

impl CaptureState {
    fn new(
        src_rate: u32,
        channels: u16,
        tx: SyncSender<Vec<i16>>,
        stats: Arc<CaptureStats>,
    ) -> Self {
        Self {
            rs: LinearResampler::new(src_rate, TARGET_RATE),
            mono: Vec::with_capacity(1024),
            acc: Vec::with_capacity(FRAME_SAMPLES * 4),
            tx,
            stats,
            channels: (channels.max(1)) as usize,
        }
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

        // 2) 重采样到 16 kHz
        let mut resampled: Vec<f32> = Vec::with_capacity(self.mono.len() + 8);
        self.rs.process(&self.mono, &mut resampled);

        // 3) 攒定长帧并送出
        self.acc.extend_from_slice(&resampled);
        while self.acc.len() >= FRAME_SAMPLES {
            let frame: Vec<i16> = self.acc[..FRAME_SAMPLES]
                .iter()
                .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect();
            self.acc.drain(..FRAME_SAMPLES);
            self.stats.frames_out.fetch_add(1, Ordering::Relaxed);
            if self.tx.try_send(frame).is_err() {
                self.stats.frames_dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// 一个正在运行的采集会话。drop 它会停止采集。
pub struct Capture {
    _stream: cpal::Stream,
    pub rx: Receiver<Vec<i16>>,
    pub stats: Arc<CaptureStats>,
    pub src_sample_rate: u32,
    pub src_channels: u16,
    pub device_name: String,
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
    let stats = Arc::new(CaptureStats::default());
    let err_stats = Arc::clone(&stats);
    let err_fn = move |e: cpal::StreamError| {
        err_stats.stream_errors.fetch_add(1, Ordering::Relaxed);
        eprintln!("[audio] stream error: {e}");
    };

    let stream = match sample_format {
        SampleFormat::F32 => {
            let mut st = CaptureState::new(src_sample_rate, src_channels, tx, Arc::clone(&stats));
            dev.build_input_stream(&config, move |d: &[f32], _| st.feed(d, |x| x), err_fn, None)?
        }
        SampleFormat::I16 => {
            let mut st = CaptureState::new(src_sample_rate, src_channels, tx, Arc::clone(&stats));
            dev.build_input_stream(
                &config,
                move |d: &[i16], _| st.feed(d, |x| x as f32 / 32768.0),
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let mut st = CaptureState::new(src_sample_rate, src_channels, tx, Arc::clone(&stats));
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
        stats,
        src_sample_rate,
        src_channels,
        device_name: name,
    })
}