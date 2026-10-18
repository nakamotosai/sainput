# -*- coding: utf-8 -*-
"""M0b: 流式 ASR 打表 —— 首字延迟 / 每块解码耗时 / RTF。

用法: python bench_stream.py <model_dir> [wav ...]
"""
import sys, time, statistics, os
sys.stdout.reconfigure(encoding="utf-8")

import numpy as np
import soundfile as sf
import sherpa_onnx

FRAME_MS = 32
RATE = 16000
FRAME = RATE * FRAME_MS // 1000   # 512


def build(md, threads=2, greedy=True):
    return sherpa_onnx.OnlineRecognizer.from_transducer(
        tokens=os.path.join(md, "tokens.txt"),
        encoder=os.path.join(md, "encoder-epoch-99-avg-1.int8.onnx"),
        decoder=os.path.join(md, "decoder-epoch-99-avg-1.int8.onnx"),
        joiner=os.path.join(md, "joiner-epoch-99-avg-1.int8.onnx"),
        num_threads=threads,
        sample_rate=RATE,
        feature_dim=80,
        decoding_method="greedy_search" if greedy else "modified_beam_search",
        enable_endpoint_detection=True,
        rule1_min_trailing_silence=2.4,
        rule2_min_trailing_silence=1.2,
        rule3_min_utterance_length=20,
        provider="cpu",
    )


def bench(rec, wav, label):
    samples, sr = sf.read(wav, dtype="float32", always_2d=False)
    if samples.ndim > 1:
        samples = samples.mean(axis=1)
    if sr != RATE:
        idx = (np.arange(int(len(samples) * RATE / sr)) * sr / RATE).astype(np.int64)
        samples = samples[np.clip(idx, 0, len(samples) - 1)]
    dur = len(samples) / RATE

    stream = rec.create_stream()
    per_chunk = []
    first_text_ms = None
    first_partial_ms = None
    n_partial = 0
    t_start = time.perf_counter()
    last = ""
    for i in range(0, len(samples), FRAME):
        chunk = samples[i:i + FRAME]
        fed_ms = (i + len(chunk)) / RATE * 1000.0
        t0 = time.perf_counter()
        stream.accept_waveform(RATE, chunk)
        while rec.is_ready(stream):
            rec.decode_stream(stream)
        t1 = time.perf_counter()
        per_chunk.append((t1 - t0) * 1000.0)
        txt = rec.get_result(stream)
        if txt and txt != last:
            n_partial += 1
            if first_partial_ms is None:
                first_partial_ms = fed_ms
            last = txt
    stream.input_finished()
    while rec.is_ready(stream):
        rec.decode_stream(stream)
    final = rec.get_result(stream)
    t_end = time.perf_counter()

    s = [x for x in per_chunk]
    s_sorted = sorted(s)
    p = lambda q: s_sorted[min(int((len(s_sorted) - 1) * q), len(s_sorted) - 1)]
    print(f"\n===== {label} =====")
    print(f"  音频 {dur:.2f}s / {len(s)} 块 ({FRAME_MS}ms)")
    print(f"  总解码 {t_end - t_start:.3f}s  → RTF {(t_end - t_start) / dur:.3f}")
    print(f"  每块耗时(ms)  p50={p(0.5):.2f}  p95={p(0.95):.2f}  max={max(s):.2f}  mean={statistics.mean(s):.2f}")
    print(f"  部分结果次数 = {n_partial}")
    print(f"  首个部分结果出现在喂入 {first_partial_ms:.0f}ms 音频时" if first_partial_ms else "  没有部分结果")
    print(f"  final: {final}")


if __name__ == "__main__":
    md = sys.argv[1]
    wavs = sys.argv[2:] or [os.path.join(md, "test_wavs", "0.wav")]
    rec = build(md)
    print(f"model: {md}")
    for i, w in enumerate(wavs):
        bench(rec, w, os.path.basename(w))
