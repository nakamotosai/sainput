//! 极简 WAV 读写（16-bit PCM 单声道 16 kHz），刻意不引依赖。

use std::io::Write;

/// 目标采样率：语音前端统一到 16 kHz。
use crate::audio::TARGET_RATE;

/// 写出 16 kHz / 单声道 / 16 bit 的 WAV。
pub fn write_pcm16_mono(path: &str, sample_rate: u32, samples: &[i16]) -> std::io::Result<()> {
    let data_bytes = (samples.len() * 2) as u32;
    let byte_rate = sample_rate * 2;
    let mut f = std::fs::File::create(path)?;

    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_bytes).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_bytes.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// 读任意常见 WAV（PCM16 / float32，任意声道与采样率），统一成 16 kHz 单声道 i16。
///
/// 线性重采样只是够用，正式录制链路用的是音频回调里那个连续重采样器。
pub fn read_as_pcm16_mono_16k(path: &str) -> anyhow::Result<Vec<i16>> {
    let b = std::fs::read(path)?;
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        anyhow::bail!("不是合法的 RIFF/WAVE: {path}");
    }
    let mut pos = 12usize;
    let (mut channels, mut rate, mut bits, mut fmt_tag) = (1u16, 16000u32, 16u16, 1u16);
    let mut data: Option<(usize, usize)> = None;

    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = u32le(&b, pos + 4) as usize;
        let body = pos + 8;
        if id == b"fmt " && body + 16 <= b.len() {
            fmt_tag = u16le(&b, body);
            channels = u16le(&b, body + 2).max(1);
            rate = u32le(&b, body + 4).max(1);
            bits = u16le(&b, body + 14);
        } else if id == b"data" {
            data = Some((body, size.min(b.len().saturating_sub(body))));
        }
        pos = body + size + (size & 1);
    }

    let (off, len) = data.ok_or_else(|| anyhow::anyhow!("WAV 里没有 data 块"))?;
    let bytes_per_sample = (bits / 8) as usize;
    if bytes_per_sample == 0 {
        anyhow::bail!("非法位深 {bits}");
    }
    let total = len / (bytes_per_sample * channels as usize);

    // 1) 解成 f32 单声道
    let mut mono: Vec<f32> = Vec::with_capacity(total);
    for i in 0..total {
        let mut acc = 0.0f32;
        for c in 0..channels as usize {
            let o = off + (i * channels as usize + c) * bytes_per_sample;
            if o + bytes_per_sample > b.len() {
                break;
            }
            let v = match (fmt_tag, bits) {
                (3, 32) => f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]),
                (1, 16) => i16::from_le_bytes([b[o], b[o + 1]]) as f32 / 32768.0,
                (1, 8) => (b[o] as f32 - 128.0) / 128.0,
                (1, 32) => i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as f32 / 2147483648.0,
                _ => anyhow::bail!("暂不支持的 WAV 格式: tag={fmt_tag} bits={bits}"),
            };
            acc += v;
        }
        mono.push(acc / channels as f32);
    }

    // 2) 线性重采样到 16k
    let out_len = ((mono.len() as f64) * TARGET_RATE as f64 / rate as f64).round() as usize;
    let mut out: Vec<i16> = Vec::with_capacity(out_len);
    let step = rate as f64 / TARGET_RATE as f64;
    for i in 0..out_len {
        let p = i as f64 * step;
        let idx = p as usize;
        let frac = (p - idx as f64) as f32;
        let a = *mono.get(idx).unwrap_or(&0.0);
        let bnext = *mono.get(idx + 1).unwrap_or(&a);
        let s = a + (bnext - a) * frac;
        out.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
    }
    Ok(out)
}