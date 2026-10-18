# -*- coding: utf-8 -*-
"""流式时间线探针：把一段 WAV 按真实节奏喂给 asr_sidecar，记录

  * 音频位置 → 识别文本变化的时刻（= 分块刷新间隔、首个字延迟）
  * 在指定位置「停喂」（模拟松键），可选继续喂 --tail-ms 的尾巴，
    再看 final 里多出来什么字（尾巴到底救回了哪些字）

用法：
  python tools/probe_stream.py --wav x.wav                       # 只看时间线
  python tools/probe_stream.py --wav x.wav --stop-at 3800        # 3.8s 处停喂（旧行为）
  python tools/probe_stream.py --wav x.wav --stop-at 3800 --tail-ms 300   # 新行为
"""
import argparse, base64, json, os, queue, subprocess, sys, threading, time, wave

import numpy as np

RATE = 16000
FRAME = 512          # 32ms @ 16k


def say(*a):
    print(*a, flush=True)


def read_wav16k(path):
    wf = wave.open(path, "rb")
    sr, ch, sw = wf.getframerate(), wf.getnchannels(), wf.getsampwidth()
    raw = wf.readframes(wf.getnframes())
    wf.close()
    if sw != 2:
        raise SystemExit("只支持 16bit WAV: %s" % path)
    x = np.frombuffer(raw, dtype="<i2").astype(np.float32)
    if ch > 1:
        x = x.reshape(-1, ch).mean(axis=1)
    if sr != RATE:
        n = int(round(len(x) * RATE / sr))
        x = np.interp(np.linspace(0, len(x) - 1, n), np.arange(len(x)), x)
    return np.clip(x, -32768, 32767).astype(np.int16)


class Sidecar:
    def __init__(self, python, script, args):
        self.p = subprocess.Popen(
            [python, "-u", script] + args,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            cwd=os.path.dirname(os.path.abspath(script)),
        )
        self.q = queue.Queue()
        threading.Thread(target=self._reader, daemon=True).start()

    def _reader(self):
        for line in self.p.stdout:
            line = line.decode("utf-8", "replace").strip()
            if not line:
                continue
            try:
                self.q.put((time.monotonic(), json.loads(line)))
            except Exception:
                self.q.put((time.monotonic(), {"type": "junk", "text": line}))

    def send(self, obj):
        self.p.stdin.write((json.dumps(obj) + "\n").encode("utf-8"))
        self.p.stdin.flush()

    def audio(self, frame):
        self.send({"cmd": "audio", "pcm": base64.b64encode(frame.tobytes()).decode("ascii")})

    def finish(self):
        self.send({"cmd": "finish"})

    def drain(self):
        out = []
        while True:
            try:
                out.append(self.q.get_nowait())
            except queue.Empty:
                return out

    def wait_ready(self, timeout=300):
        t0 = time.monotonic()
        while time.monotonic() - t0 < timeout:
            for _, m in self.drain():
                if m.get("type") == "ready":
                    return m
            time.sleep(0.05)
        raise SystemExit("sidecar 没就绪")

    def close(self):
        try:
            self.send({"cmd": "quit"})
        except Exception:
            pass
        time.sleep(0.2)
        try:
            self.p.kill()
        except Exception:
            pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--wav", required=True)
    ap.add_argument("--model-dir", default=None)
    ap.add_argument("--punct-dir", default=None)
    ap.add_argument("--no-punct", action="store_true")
    ap.add_argument("--throttle-ms", type=int, default=80)
    ap.add_argument("--threads", type=int, default=2)
    ap.add_argument("--stop-at", type=int, default=0, help="喂到多少毫秒就停（模拟松键）")
    ap.add_argument("--tail-ms", type=int, default=0, help="停喂之后继续喂多少毫秒")
    ap.add_argument("--pad-kind", default="zero", choices=["zero", "noise", "tone"],
                    help="静音补偿用什么填：zero=纯 0，noise=底噪白噪，tone=循环停喂点之后的真实静音")
    ap.add_argument("--pad-ms", type=int, default=0,
                    help="停喂之后一次性灌多少毫秒静音（模型的右上下文补偿，瞬间完成）")
    ap.add_argument("--pace", default="real", choices=["real", "fast"])
    ap.add_argument("--python", default=sys.executable)
    ap.add_argument("--timeout", type=float, default=6.0)
    ap.add_argument("--ready-timeout", type=float, default=300.0, help="等模型加载的时间")
    a = ap.parse_args()

    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    script = os.path.join(root, "tools", "asr_sidecar.py")
    a.model_dir = os.path.abspath(a.model_dir) if a.model_dir else ""
    if a.punct_dir:
        a.punct_dir = os.path.abspath(a.punct_dir)
    model = a.model_dir or os.path.join(
        root, "models",
        "sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05")
    pargs = ["--model-dir", model, "--threads", str(a.threads),
             "--throttle-ms", str(a.throttle_ms)]
    if not a.no_punct:
        punct = a.punct_dir or os.path.join(
            root, "models", "sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8")
        if os.path.isdir(punct):
            pargs += ["--punct-dir", punct]

    x = read_wav16k(a.wav)
    dur_ms = len(x) * 1000.0 / RATE
    say("wav=%s  %.2fs  %d 样本" % (os.path.basename(a.wav), dur_ms / 1000.0, len(x)))
    sc = Sidecar(a.python, script, pargs)
    r = sc.wait_ready(a.ready_timeout)
    say("就绪: model=%s decoding=%s" % (r.get("model"), r.get("decoding")))
    say("节流=%dms  停喂位置=%s ms  尾巴=%d ms  静音补偿=%d ms"
        % (a.throttle_ms, a.stop_at or "无", a.tail_ms, a.pad_ms))
    say("-" * 92)
    say("%9s %8s  %s" % ("音频位置", "墙钟", "事件"))

    t0 = time.monotonic()
    updates = []          # (pos_ms, wall_ms, 文本)
    last_text = ""
    nframes = len(x) // FRAME
    stop_frame = nframes
    if a.stop_at > 0:
        stop_frame = min(nframes, int(a.stop_at / (FRAME * 1000.0 / RATE)))
    tail_frames = int(a.tail_ms / (FRAME * 1000.0 / RATE))

    def handle(msgs, pos_ms):
        nonlocal last_text
        for _, m in msgs:
            w = (time.monotonic() - t0) * 1000.0
            if m.get("type") == "partial":
                txt = m.get("text") or ""
                if txt != last_text:
                    add = txt[len(last_text):] if txt.startswith(last_text) else "(重写)"
                    updates.append((pos_ms, w, txt))
                    last_text = txt
                    say("%9d %8.0f  P  %s   ← 新增 %s" % (pos_ms, w, txt, add))
            elif m.get("type") == "final":
                say("%9d %8.0f  F  (%s)  %s" % (pos_ms, w, m.get("reason"), m.get("text")))
                say("%9s %8s     raw: %s" % ("", "", m.get("raw")))
            elif m.get("type") == "error":
                say("%9d %8.0f  !  %s" % (pos_ms, w, m.get("msg")))

    def feed(i, pos_ms):
        sc.audio(x[i * FRAME:(i + 1) * FRAME])
        handle(sc.drain(), pos_ms)
        if a.pace == "real":
            nxt = t0 + (pos_ms + 1000.0 * FRAME / RATE) / 1000.0
            d = nxt - time.monotonic()
            if d > 0:
                time.sleep(d)

    for i in range(stop_frame):
        feed(i, int(i * FRAME * 1000.0 / RATE))
    pos_stop = int(stop_frame * FRAME * 1000.0 / RATE)
    if a.stop_at > 0:
        say("%9d %8.0f  ▓ 停喂（= 松手）" % (pos_stop, (time.monotonic() - t0) * 1000))
    for k in range(tail_frames):
        i = stop_frame + k
        if i >= nframes:
            break
        feed(i, int(i * FRAME * 1000.0 / RATE))
    pos_end = int(min(stop_frame + tail_frames, nframes) * FRAME * 1000.0 / RATE)
    if a.tail_ms:
        say("%9d %8.0f  ◆ 尾巴喂完（共多喂 %dms）" % (pos_end, (time.monotonic() - t0) * 1000,
                                                pos_end - pos_stop))
    if a.pad_ms:
        burst = int(a.pad_ms / (FRAME * 1000.0 / RATE))
        t_pad = time.monotonic()
        rng = np.random.default_rng(7)
        for kk in range(burst):
            if a.pad_kind == "zero":
                pad = np.zeros(FRAME, dtype=np.int16)
            elif a.pad_kind == "noise":
                pad = np.clip(rng.normal(0, 91.0, FRAME), -3000, 3000).astype(np.int16)
            else:
                st = stop_frame * FRAME + (kk % 5) * FRAME
                pad = x[st:st + FRAME]
                if len(pad) < FRAME:
                    pad = np.zeros(FRAME, dtype=np.int16)
            sc.audio(pad)
        handle(sc.drain(), pos_end)
        say("%9s %8.0f  ◇ 一次性灌入 %dms 静音（耗时 %.0fms）"
            % ("", (time.monotonic() - t0) * 1000, a.pad_ms,
               (time.monotonic() - t_pad) * 1000))
    sc.finish()
    deadline = time.monotonic() + a.timeout
    while time.monotonic() < deadline:
        handle(sc.drain(), pos_end)
        if any(True for _ in []):
            pass
        time.sleep(0.01)
        if not sc.q.empty():
            continue
    handle(sc.drain(), pos_end)
    say("-" * 92)
    if updates:
        gaps = [b[0] - a_[0] for a_, b in zip(updates, updates[1:])]
        gapsw = [b[1] - a_[1] for a_, b in zip(updates, updates[1:])]
        gaps_sorted = sorted(gaps)
        say("文本更新次数=%d  首次出字@%dms  更新间隔(按音频) p50=%dms max=%dms"
            % (len(updates), updates[0][0], gaps_sorted[len(gaps_sorted) // 2] if gaps else 0,
               max(gaps) if gaps else 0))
        if gapsw:
            sw = sorted(gapsw)
            say("更新间隔(按墙钟) p50=%.0fms max=%.0fms" % (sw[len(sw) // 2], max(sw)))
        say("每次新增字数: %s" % " ".join(str(len(t) - len(p)) for (_, _, p), (_, _, t)
                                          in zip(updates, updates[1:])))
    sc.close()


if __name__ == "__main__":
    main()
