# -*- coding: utf-8 -*-
"""M0b: 多模型流式打表。首字延迟按「语音起点」归一化，不按文件开头。"""
import sys, os, glob, time, statistics
sys.stdout.reconfigure(encoding="utf-8")
import numpy as np, soundfile as sf, sherpa_onnx

RATE, FRAME_MS = 16000, 32
FRAME = RATE * FRAME_MS // 1000

def find(md, kind):
    g = sorted(glob.glob(os.path.join(md, f"{kind}*.int8.onnx")))
    if not g: g = sorted(glob.glob(os.path.join(md, f"{kind}*.onnx")))
    return g[0]

def onset_ms(x, sr=RATE, win_ms=20, rel=0.08):
    win = int(win_ms*sr/1000)
    if len(x) < win: return 0
    e = np.array([np.sqrt(np.mean(x[i:i+win]**2)) for i in range(0, len(x)-win, win)])
    if e.size == 0 or e.max() <= 0: return 0
    thr = e.max()*rel
    idx = int(np.argmax(e > thr))
    return idx*win_ms

def build(md, threads=2, beam=False):
    return sherpa_onnx.OnlineRecognizer.from_transducer(
        tokens=os.path.join(md,"tokens.txt"), encoder=find(md,"encoder"),
        decoder=find(md,"decoder"), joiner=find(md,"joiner"),
        num_threads=threads, sample_rate=RATE, feature_dim=80,
        decoding_method="modified_beam_search" if beam else "greedy_search",
        enable_endpoint_detection=True,
        rule1_min_trailing_silence=2.4, rule2_min_trailing_silence=1.2,
        rule3_min_utterance_length=20, provider="cpu")

def run(rec, wav):
    x, sr = sf.read(wav, dtype="float32")
    if x.ndim>1: x = x.mean(axis=1)
    if sr != RATE:
        idx=(np.arange(int(len(x)*RATE/sr))*sr/RATE).astype(np.int64)
        x = x[np.clip(idx,0,len(x)-1)]
    on = onset_ms(x)
    st = rec.create_stream(); times=[]; first=None; last=""; n=0
    t0=time.perf_counter()
    for i in range(0,len(x),FRAME):
        fed=(i+FRAME)/RATE*1000
        a=time.perf_counter(); st.accept_waveform(RATE, x[i:i+FRAME])
        while rec.is_ready(st): rec.decode_stream(st)
        times.append((time.perf_counter()-a)*1000)
        txt=rec.get_result(st)
        if txt and txt!=last:
            n+=1
            if first is None: first=fed
            last=txt
    st.input_finished()
    while rec.is_ready(st): rec.decode_stream(st)
    fin=rec.get_result(st); el=time.perf_counter()-t0
    s=sorted(times); p=lambda q: s[min(int((len(s)-1)*q), len(s)-1)]
    return dict(dur=len(x)/RATE, rtf=el/(len(x)/RATE), p50=p(.5), p95=p(.95),
                mx=max(s), n=n, first_abs=first, on=on,
                first_rel=(first-on) if first else None, final=fin)

models = sys.argv[1:]
for md in models:
    name = os.path.basename(md.rstrip("\\/"))
    try: rec = build(md)
    except Exception as e: print(f"[{name}] 构建失败: {e}"); continue
    wavs = sorted(glob.glob(os.path.join(md,"test_wavs","*.wav")))
    wavs = [w for w in wavs if "8k" not in os.path.basename(w)][:3]
    print(f"\n########## {name}")
    for w in wavs:
        r = run(rec, w)
        fr = f"{r['first_rel']:.0f}ms" if r['first_rel'] is not None else "n/a"
        print(f"  {os.path.basename(w):8s} dur={r['dur']:5.2f}s RTF={r['rtf']:.3f} "
              f"chunk p50={r['p50']:.2f}/p95={r['p95']:.2f}/max={r['mx']:.1f}ms  "
              f"partials={r['n']:3d} 起说后首字={fr}")
        print(f"           final: {r['final']}")
