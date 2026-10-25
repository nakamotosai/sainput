# -*- coding: utf-8 -*-
"""Tail-loss probe: where does the last syllable go?

Modes:
  direct : spawn asr_sidecar.py exactly like sainput_doubao_proxy.py does, feed
           audio straight into it (no websocket, no IME).
  proxy  : speak the Doubao protocol to a running sainput_doubao_proxy.py.

Pacing:
  --paced 0 : dump the whole wav at once (batch / offline truth)
  --paced 1 : 200 ms chunks every 200 ms, like the IME's live capture

Every mode ends with the same release sequence the IME produces: final audio
packet -> (proxy) pad 600 ms room noise -> finish.
"""
from __future__ import annotations

import argparse
import array
import base64
import gzip
import io
import json
import os
import queue
import ssl
import struct
import subprocess
import sys
import threading
import time
import wave

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
MODEL_DIR = os.path.join(REPO, "models",
                         "sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05")
PUNCT_DIR = os.path.join(REPO, "models",
                         "sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8")
PYTHON = os.path.join(REPO, "dist", "sainput-0.2.2-win64", "runtime", "python.exe")
SCRIPT = os.path.join(HERE, "asr_sidecar.py")
HOTWORDS = os.path.join(REPO, "hotwords.txt")
RATE = 16000


def read_wav(path: str) -> "array.array":
    with open(path, "rb") as f:
        data = f.read()
    with wave.open(io.BytesIO(data), "rb") as wf:
        assert wf.getnchannels() == 1 and wf.getsampwidth() == 2 and wf.getframerate() == RATE, \
            "need 16 kHz mono 16-bit wav"
        raw = wf.readframes(wf.getnframes())
    return array.array("h", raw)


class Sidecar:
    def __init__(self, script: str = SCRIPT) -> None:
        args = [PYTHON, script, "--model-dir", MODEL_DIR, "--threads", "2",
                "--rule2", "0", "--punct-mode", "keep"]
        if os.path.isdir(PUNCT_DIR):
            args += ["--punct-dir", PUNCT_DIR]
        if os.path.isfile(HOTWORDS):
            args += ["--hotwords", HOTWORDS]
        self.proc = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     bufsize=0, env={**os.environ, "SAINPUT_PUMP_DEBUG": "1"})
        self.q: "queue.Queue[dict]" = queue.Queue()
        self.spill: list = []
        threading.Thread(target=self._reader, daemon=True).start()

    def _reader(self) -> None:
        for raw in self.proc.stdout:  # type: ignore[union-attr]
            line = raw.decode("utf-8", "replace").strip()
            if not line:
                continue
            try:
                self.q.put(json.loads(line))
            except Exception:
                pass

    def send(self, obj: dict) -> None:
        self.proc.stdin.write((json.dumps(obj) + "\n").encode("utf-8"))  # type: ignore[union-attr]
        self.proc.stdin.flush()  # type: ignore[union-attr]

    def wait_for(self, kind: str, timeout: float) -> dict | None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                msg = self.q.get(timeout=0.2)
            except queue.Empty:
                continue
            if msg.get("type") == kind:
                return msg
            self.spill.append(msg)
        return None

    def close(self) -> None:
        try:
            self.proc.kill()
        except Exception:
            pass


def packet(mtype: int, flags: int, sequence: int, payload: bytes) -> bytes:
    compressed = gzip.compress(payload)
    return (bytes([0x11, (mtype << 4) | flags, 0x11, 0x00])
            + struct.pack(">i", sequence)
            + struct.pack(">I", len(compressed))
            + compressed)


def parse_server(message: bytes):
    if len(message) < 4:
        return None
    header_size = (message[0] & 0x0F) * 4
    mtype = message[1] >> 4
    flags = message[1] & 0x0F
    compression = message[2] & 0x0F
    offset = header_size
    if flags & 0x01:
        offset += 4
    if flags & 0x04:
        offset += 4
    if offset + 4 > len(message):
        return None
    size = struct.unpack(">I", message[offset:offset + 4])[0]
    offset += 4
    payload = message[offset:offset + size]
    if compression == 0x01 and payload:
        payload = gzip.decompress(payload)
    text = ""
    if payload:
        try:
            body = json.loads(payload.decode("utf-8"))
            result = body.get("result", {})
            if isinstance(result, dict):
                text = result.get("text", "")
        except Exception:
            pass
    return mtype, flags, text, bool(flags & 0x02)


def chunks_of(pcm: "array.array", chunk_bytes: int = 6400):
    raw = pcm.tobytes()
    return [raw[i:i + chunk_bytes] for i in range(0, len(raw), chunk_bytes)]


def run_direct(pcm, paced: bool, pad_ms: int, tail_ms: int, script: str, verbose: bool, speed: float = 1.0,
               chunk_bytes: int = 6400, silence_sec: float = 0.0) -> dict:
    sc = Sidecar(script)
    try:
        if sc.wait_for("ready", 60) is None:
            return {"error": "sidecar not ready"}
        sc.send({"cmd": "set", "key": "silence_sec", "value": silence_sec})
        sc.wait_for("set_ack", 5)
        sc.send({"cmd": "reset"})
        sc.wait_for("reset", 5)

        pieces = chunks_of(pcm, chunk_bytes)
        # The IME keeps the final partial chunk back for the negative-sequence packet.
        dbg: list = []

        def collect(phase: str) -> None:
            while True:
                try:
                    msg = sc.q.get_nowait()
                except queue.Empty:
                    break
                if msg.get("type") == "dbg":
                    msg["phase"] = phase
                    dbg.append(msg)
            for msg in sc.spill:
                if msg.get("type") == "dbg":
                    msg["phase"] = phase
                    dbg.append(msg)
            sc.spill.clear()

        pit = time.perf_counter()
        for piece in pieces:
            sc.send({"cmd": "audio", "pcm": base64.b64encode(piece).decode("ascii"),
                     "sample_rate": RATE})
            collect("audio")
            if paced:
                target = 0.2 / max(speed, 0.01) - (time.perf_counter() - pit)
                if target > 0:
                    time.sleep(target)
                pit = time.perf_counter()
        if tail_ms:
            sc.send({"cmd": "pad", "ms": tail_ms, "rms": 0.003})
            sc.wait_for("padded", 5)
            collect("pad-tail")
        if pad_ms:
            sc.send({"cmd": "pad", "ms": pad_ms, "rms": 0.003})
            sc.wait_for("padded", 5)
            collect("pad")
        sc.send({"cmd": "finish"})
        final = sc.wait_for("final", 30)
        collect("finish")
        # The sidecar only emits `dbg` when SAINPUT_PUMP_DEBUG=1 (this probe sets it).
        # No dbg at all -> report None, never a fake 0 that reads like "no backlog".
        measured = bool(dbg)
        # 流式阶段(收尾那一轮 unlimited,不算)的每轮解码帧数:这才是"4 帧限量有没有顶住"。
        streaming = [m["n"] for m in dbg if m["phase"] != "finish"]
        out = {"final": (final or {}).get("text", ""), "raw": (final or {}).get("raw", ""),
               "tail_chunks": (final or {}).get("tail_chunks"),
               "pumps": len(dbg) if measured else None,
               "max_decode_per_pump": max(streaming, default=0) if measured else None,
               "pumps_at_cap": sum(1 for n in streaming if n >= 4) if measured else None,
               # 哨兵:修好后收尾一定把流解空,所以这里恒为 false。
               "still_ready_after_finish": dbg[-1]["still_ready"] if measured else None}
        if verbose:
            out["dbg"] = dbg
        return out
    finally:
        sc.close()


def run_proxy(pcm, paced: bool, pad_ms: int, tail_ms: int, port: int, chunk_bytes: int = 6400) -> dict:
    from websockets.sync.client import connect
    ctx = ssl.create_default_context(cafile=os.path.join(HERE, "ca.pem"))
    last_text = ""
    with connect("wss://127.0.0.1:%d/api/v3/sauc/bigmodel_async" % port, ssl=ctx,
                 max_size=None, open_timeout=10, close_timeout=3) as ws:
        request = {"user": {"uid": "tailprobe"},
                   "audio": {"format": "pcm", "codec": "raw", "rate": RATE, "bits": 16, "channel": 1},
                   "request": {"model_name": "bigmodel"}}
        ws.send(packet(0x01, 0x01, 1, json.dumps(request).encode("utf-8")))
        pieces = chunks_of(pcm, chunk_bytes)
        seq = 2
        pit = time.perf_counter()
        for i, piece in enumerate(pieces):
            is_last = i == len(pieces) - 1
            ws.send(packet(0x02, 0x03 if is_last else 0x01, -seq if is_last else seq, piece))
            seq += 1
            while True:
                try:
                    msg = ws.recv(timeout=0.001)
                except TimeoutError:
                    break
                parsed = parse_server(msg)
                if parsed and parsed[2]:
                    last_text = parsed[2]
            if paced and not is_last:
                target = 0.2 - (time.perf_counter() - pit)
                if target > 0:
                    time.sleep(target)
                pit = time.perf_counter()
        deadline = time.monotonic() + 30
        final = ""
        while time.monotonic() < deadline:
            try:
                msg = ws.recv(timeout=0.5)
            except TimeoutError:
                continue
            parsed = parse_server(msg)
            if not parsed:
                continue
            _, _, text, is_last = parsed
            if text:
                last_text = text
            if is_last:
                final = text
                break
    return {"final": final, "last_partial": last_text}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", default="proxy", choices=["direct", "proxy"])
    ap.add_argument("--wav", default=os.path.join(MODEL_DIR, "test_wavs", "2.wav"))
    ap.add_argument("--paced", type=int, default=0)
    ap.add_argument("--pad-ms", type=int, default=600)
    ap.add_argument("--tail-ms", type=int, default=0,
                    help="extra silence sent BEFORE the pad step (direct mode only)")
    ap.add_argument("--port", type=int, default=19000)
    ap.add_argument("--script", default=SCRIPT)
    ap.add_argument("--verbose", type=int, default=0)
    ap.add_argument("--repeat", type=int, default=1, help="repeat the wav N times to build a long session")
    ap.add_argument("--speed", type=float, default=1.0, help="feed rate multiplier (1.0 = real time)")
    ap.add_argument("--chunk-bytes", type=int, default=6400, help="bytes per audio command (6400 = 200 ms)")
    ap.add_argument("--silence-sec", type=float, default=0.0,
                    help="sidecar silence_sec (0 = never endpoint; >0 exercises the VAD final path)")
    ap.add_argument("--trail-silence-ms", type=int, default=0,
                    help="append this much noise after the wav (lets a VAD endpoint fire)")
    a = ap.parse_args()
    pcm = read_wav(a.wav)
    if a.repeat > 1:
        pcm = array.array("h", pcm.tolist() * a.repeat)
    if a.trail_silence_ms > 0:
        pcm = array.array("h", pcm.tolist() + [0] * (RATE * a.trail_silence_ms // 1000))
    dur = len(pcm) / float(RATE)
    t0 = time.perf_counter()
    if a.mode == "direct":
        out = run_direct(pcm, bool(a.paced), a.pad_ms, a.tail_ms, a.script, bool(a.verbose), a.speed,
                         a.chunk_bytes, a.silence_sec)
    else:
        out = run_proxy(pcm, bool(a.paced), a.pad_ms, a.tail_ms, a.port, a.chunk_bytes)
    out["audio_sec"] = round(dur, 3)
    out["wall_sec"] = round(time.perf_counter() - t0, 3)
    print(json.dumps(out, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
