# -*- coding: utf-8 -*-
r"""逐字发射延迟实测：按真实 32ms 节拍喂帧，记录每次文本变化发生的时刻。
回答「几百毫秒才一个字」到底是多少、以及是不是模型分块大小造成的。

用法：
  python tools/probe_latency.py                      # 写进 tmp\latency.txt（不碰冻结产物）
  python tools/probe_latency.py --out D:/x.txt       # 自己指定输出路径

注意：默认**不**写仓库根 `latency.txt` —— 那是 FROZEN.md 第 8 节
按 SHA-256 冻结的产物，被覆写就等于悄悄作废了冻结快照。
"""
import sys, os, glob, io, wave, array, time, argparse
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sherpa_onnx
import numpy as np

MODELS = [
    ("bilingual-2023", r"models\sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20"),
    ("x-asr-160ms",    r"models\sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05"),
    ("x-asr-480ms",    r"models\sherpa-onnx-x-asr-480ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05"),
    ("x-asr-960ms",    r"models\sherpa-onnx-x-asr-960ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05"),
]
WAV = r"models\sherpa-onnx-streaming-zipformer-zh-14M-2023-02-23\test_wavs\0.wav"


def pick(md, kind):
    for pat in (f"{kind}*.int8.onnx", f"{kind}*.onnx"):
        g = sorted(glob.glob(os.path.join(md, pat)))
        if g:
            return g[0]
    raise SystemExit("missing " + kind)


def load(md):
    return sherpa_onnx.OnlineRecognizer.from_transducer(
        tokens=os.path.join(md, "tokens.txt"),
        encoder=pick(md, "encoder"), decoder=pick(md, "decoder"), joiner=pick(md, "joiner"),
        num_threads=2, sample_rate=16000, feature_dim=80,
        decoding_method="greedy_search",
        enable_endpoint_detection=True,
        rule1_min_trailing_silence=2.4, rule2_min_trailing_silence=1.2,
        rule3_min_utterance_length=20, provider="cpu")


def read_wav16k(path):
    wf = wave.open(path, "rb")
    ch, sr, n = wf.getnchannels(), wf.getframerate(), wf.getnframes()
    a = array.array("h"); a.frombytes(wf.readframes(n)); wf.close()
    if ch == 2:
        a = array.array("h", [(a[i] + a[i+1]) // 2 for i in range(0, len(a)-1, 2)])
    if sr != 16000:
        r = 16000.0 / sr
        a = array.array("h", [a[min(len(a)-1, int(i*r))] for i in range(int(len(a)*r))])
    return np.frombuffer(a.tobytes(), dtype="<i2").astype("float32") / 32768.0


def default_out():
    """默认写到 tmp\\latency.txt（相对本文件所在项目根），绝不覆写冻结产物。"""
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    return os.path.join(root, "tmp", "latency.txt")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=default_out(),
                    help="输出路径（默认 tmp\\latency.txt，不碰冻结的 latency.txt）")
    ap.add_argument("--wav", default=WAV, help="喂入的 WAV（默认 14M 档的 0.wav）")
    a = ap.parse_args()

    out = io.StringIO()
    x = read_wav16k(a.wav)
    STEP = 512
    FRAME_MS = 32.0

    for label, md in MODELS:
        rec = load(md)
        s = rec.create_stream()
        marks = []      # (音频时间ms, 文本)
        last = ""
        t_wall0 = time.perf_counter()
        audio_ms = 0.0
        for i in range(0, len(x), STEP):
            chunk = x[i:i+STEP]
            if chunk.size == 0:
                break
            s.accept_waveform(16000, chunk)
            audio_ms += len(chunk) / 16000.0 * 1000.0
            while rec.is_ready(s):
                rec.decode_stream(s)
            t = rec.get_result(s)
            if t != last:
                marks.append((audio_ms, t))
                last = t
        t_total = time.perf_counter() - t_wall0
        out.write(f"\n### {label}   整段处理耗时 {t_total:.2f}s（音频 {audio_ms/1000:.1f}s，RTF {t_total/(audio_ms/1000):.3f}）\n")
        if not marks:
            out.write("  （没有任何输出）\n")
            continue
        gaps = []
        prev = 0.0
        for ms, t in marks[:24]:
            if prev:
                gaps.append(ms - prev)
            prev = ms
        out.write(f"  首个字在音频 {marks[0][0]:.0f}ms 处出来\n")
        if gaps:
            gaps_sorted = sorted(gaps)
            out.write("  后续每次文本更新的间隔(ms): p50=%.0f p90=%.0f max=%.0f  共 %d 次更新\n" % (
                gaps_sorted[len(gaps_sorted)//2], gaps_sorted[int(len(gaps_sorted)*0.9)], max(gaps), len(marks)))
        out.write("  前 8 次更新：\n")
        for ms, t in marks[:8]:
            out.write(f"    @{ms:6.0f}ms  {t}\n")

    dest = os.path.abspath(a.out)
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    io.open(dest, "w", encoding="utf-8").write(out.getvalue())
    print("ok ->", dest)


if __name__ == "__main__":
    main()