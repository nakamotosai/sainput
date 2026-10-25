# -*- coding: utf-8 -*-
"""Local OpenAI-compatible ASR bridge for sainput.

The Metasequoia IME can send a recorded WAV to any OpenAI-compatible
``/v1/audio/transcriptions`` endpoint. This bridge accepts that request, feeds
the PCM to sainput's ``asr_sidecar.py`` (sherpa-onnx streaming ASR + punctuation
+ ITN), and returns ``{"text": "..."}``.

It is a prototype integration path: no IME rebuild is required. The sidecar is
kept warm, so only the first request waits for the model to load.

Usage:
  python local_asr_http.py --port 18080
  python local_asr_http.py --test-wav path\\to\\16k_mono.wav
"""
from __future__ import annotations

import argparse
import array
import base64
import io
import json
import os
import queue
import struct
import subprocess
import sys
import threading
import time
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Optional

CREATE_NO_WINDOW = 0x0800_0000


def s16le(samples: array.array) -> bytes:
    # Windows is little-endian; array('h') is already the right layout.
    return samples.tobytes()


def decode_wav(data: bytes) -> array.array:
    with wave.open(io.BytesIO(data), "rb") as wf:
        channels = wf.getnchannels()
        width = wf.getsampwidth()
        rate = wf.getframerate()
        frames = wf.readframes(wf.getnframes())

    if width == 1:
        vals = [(b - 128) / 128.0 for b in frames]
    elif width == 2:
        n = len(frames) // 2
        vals = [v / 32768.0 for v in struct.unpack("<%dh" % n, frames[: n * 2])]
    elif width == 4:
        n = len(frames) // 4
        vals = [v / 2147483648.0 for v in struct.unpack("<%di" % n, frames[: n * 4])]
    else:
        raise ValueError("unsupported WAV sample width: %d" % width)

    if channels > 1:
        mono = []
        for i in range(0, len(vals) - channels + 1, channels):
            mono.append(sum(vals[i : i + channels]) / channels)
        vals = mono

    if rate != 16000:
        # Linear resample. The IME already records at 16 kHz, so this is only a
        # safety net for manually supplied test files.
        ratio = 16000.0 / rate
        out_len = max(1, int(len(vals) * ratio))
        out = []
        last = len(vals) - 1
        for i in range(out_len):
            pos = i / ratio
            a = int(pos)
            frac = pos - a
            b = min(a + 1, last)
            out.append(vals[a] * (1.0 - frac) + vals[b] * frac)
        vals = out

    pcm = array.array("h")
    for v in vals:
        if v > 1.0:
            v = 1.0
        elif v < -1.0:
            v = -1.0
        pcm.append(int(round(v * 32767.0)))
    return pcm


def fmt_code(code) -> str:
    """Readable exit code: decimal, plus hex for Windows exception codes.

    A real crash shows up as -1073741819 (0xC0000005) rather than as
    18446744073709551615, which is what the OS reports for a kill.
    """
    if code is None:
        return "?"
    if code >= 0x80000000:
        return "%d (0x%08X)" % (code - 0x100000000, code)
    return str(code)


class Sidecar:
    """The recognizer child process and its JSON protocol.

    A missing recognizer must not look like a working one. When the child exits,
    `alive()` turns false, everyone waiting for a reply is woken with an error
    instead of sitting out their timeout, and `restart()` starts a fresh child
    with the same arguments. Without that, a dead sidecar leaves the Doubao proxy
    accepting audio and returning no text at all -- the "waveform moves, nothing
    is typed" failure, which used to be indistinguishable from a slow model.
    """

    def __init__(self, python: str, script: str, model_dir: str, punct_dir: str,
                 hotwords: Optional[str], threads: int, stderr=None, log=None):
        args = [
            python,
            script,
            "--model-dir", model_dir,
            "--threads", str(threads),
            "--rule2", "0",          # batch mode: only finish() ends a sentence
            "--punct-mode", "keep",
        ]
        if punct_dir:
            args += ["--punct-dir", punct_dir]
        if hotwords and os.path.isfile(hotwords):
            args += ["--hotwords", hotwords]

        self.q: "queue.Queue[dict]" = queue.Queue()
        self.lock = threading.Lock()          # one request at a time
        self.spawn_lock = threading.Lock()    # one respawn at a time
        self.dead = threading.Event()
        self.ready = False
        self.model = ""
        self.pid = 0
        self.exit_code: Optional[int] = None
        self._argv = args
        self._stderr = stderr
        self.log = log or (lambda *a: None)
        self._spawn()

    def _spawn(self) -> None:
        self.proc = subprocess.Popen(
            self._argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self._stderr,
            bufsize=0,
            creationflags=CREATE_NO_WINDOW,
        )
        self.pid = self.proc.pid
        self.ready = False
        self.exit_code = None
        self.dead.clear()
        while True:   # whatever the previous child said is stale now
            try:
                self.q.get_nowait()
            except queue.Empty:
                break
        threading.Thread(target=self._reader, args=(self.proc,), daemon=True).start()

    def alive(self) -> bool:
        return not self.dead.is_set() and self.proc.poll() is None

    def restart(self, timeout: float = 120.0) -> None:
        """Replace the child if it is gone; return at once when it is fine."""
        with self.spawn_lock:
            if self.alive():
                return
            code = self.exit_code if self.exit_code is not None else self.proc.poll()
            self.log("sidecar died (pid=%s exit=%s); respawning" % (self.pid, fmt_code(code)))
            started = time.monotonic()
            self._spawn()
            self.wait_ready(timeout)
            self.log("sidecar ready (pid=%s model=%s, %.1fs)" % (
                self.pid, self.model, time.monotonic() - started))

    def _reader(self, proc) -> None:
        try:
            for raw in proc.stdout:
                line = raw.decode("utf-8", "replace").strip()
                if not line:
                    continue
                try:
                    msg = json.loads(line)
                except Exception:
                    continue
                self.q.put(msg)
        except Exception as exc:
            self.log("sidecar stdout reader failed: %r" % (exc,))
        finally:
            if proc is self.proc:
                # EOF and process exit are not atomic: wait briefly for the real
                # exit code, it is the first thing to read when asking why.
                try:
                    self.exit_code = proc.wait(timeout=2.0)
                except Exception:
                    self.exit_code = proc.poll()
                self.ready = False
                self.dead.set()
                self.q.put({"type": "error", "msg": "exited (code=%s)" % fmt_code(self.exit_code)})

    def send(self, obj: dict) -> None:
        if self.dead.is_set():
            raise RuntimeError("sidecar exited (code=%s)" % fmt_code(self.exit_code))
        assert self.proc.stdin is not None
        try:
            self.proc.stdin.write(json.dumps(obj, ensure_ascii=False).encode("utf-8") + b"\n")
            self.proc.stdin.flush()
        except OSError as exc:
            # Windows reports a write to a pipe whose reader is gone as
            # OSError(22, 'Invalid argument'), which tells nobody anything.
            raise RuntimeError("sidecar gone (write failed: %r)" % (exc,)) from exc

    def wait_ready(self, timeout: float = 120.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                msg = self.q.get(timeout=min(1.0, max(0.1, deadline - time.monotonic())))
            except queue.Empty:
                continue
            if msg.get("type") == "ready":
                self.model = msg.get("model", "")
                self.ready = True
                return
            if msg.get("type") == "error":
                raise RuntimeError("sidecar: " + str(msg.get("msg")))
        raise TimeoutError("sidecar did not become ready")

    def _wait_for(self, kind: str, timeout: float) -> dict:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                msg = self.q.get(timeout=min(0.5, max(0.05, deadline - time.monotonic())))
            except queue.Empty:
                continue
            t = msg.get("type")
            if t == "error":
                raise RuntimeError("sidecar: " + str(msg.get("msg")))
            if t == kind:
                return msg
        raise TimeoutError("sidecar timed out waiting for %s" % kind)

    def transcribe(self, pcm: array.array) -> str:
        if not self.ready:
            raise RuntimeError("sidecar not ready")
        with self.lock:
            # Drop stale partials from a previous request.
            while True:
                try:
                    self.q.get_nowait()
                except queue.Empty:
                    break

            self.send({"cmd": "reset"})
            self._wait_for("reset", 10.0)

            chunk = 16000 // 3  # ~333 ms, two model chunks
            for i in range(0, len(pcm), chunk):
                piece = pcm[i : i + chunk]
                self.send({
                    "cmd": "audio",
                    "pcm": base64.b64encode(s16le(piece)).decode("ascii"),
                })
                # Sync barrier: render("") is handled after the audio command,
                # so the sidecar has finished pumping this chunk before we send
                # the next one. Without it a long WAV piles the whole file into
                # the sidecar's stdin in one go, and every pump still decodes at
                # most 4 frames while the audio command sits at the head of the
                # queue -- the transcription then lags the feed by the whole file.
                # (收尾的 finish 已经不再限量,所以这里不再有"尾巴被截"的风险。)
                self.send({"cmd": "render", "text": ""})
                self._wait_for("rendered", 60.0)

            # Feed right-context silence so the last character is not cut off.
            self.send({"cmd": "pad", "ms": 600})
            self._wait_for("padded", 30.0)

            self.send({"cmd": "finish"})
            while True:
                msg = self._wait_for("final", 60.0)
                if msg.get("reason") == "finish":
                    return str(msg.get("text") or "")

    def close(self) -> None:
        try:
            self.send({"cmd": "quit"})
        except Exception:
            pass
        try:
            self.proc.terminate()
        except Exception:
            pass


class Handler(BaseHTTPRequestHandler):
    server_version = "sainput-local-asr/0.1"

    def log_message(self, fmt: str, *args) -> None:
        sys.stderr.write("[bridge] " + (fmt % args) + "\n")
        sys.stderr.flush()

    def _json(self, code: int, obj: dict) -> None:
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        if self.path == "/health":
            self._json(200, {"ok": True, "model": self.server.sidecar.model})
        else:
            self._json(404, {"error": "not found"})

    def do_POST(self) -> None:
        if self.path != "/v1/audio/transcriptions":
            self._json(404, {"error": "not found"})
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            body = self.rfile.read(length)
            wav = extract_file_part(self.headers.get("Content-Type", ""), body)
            try:
                with open(os.path.join(os.environ.get("TEMP", "."), "sainput-last.wav"), "wb") as f:
                    f.write(wav)
            except Exception:
                pass
            pcm = decode_wav(wav)
            text = self.server.sidecar.transcribe(pcm)
            sys.stderr.write("[bridge] wav=%d pcm=%d text=%r\n" % (len(wav), len(pcm), text))
            sys.stderr.flush()
            self._json(200, {"text": text})
        except Exception as e:
            self._json(500, {"error": str(e)})


def extract_file_part(content_type: str, body: bytes) -> bytes:
    marker = "boundary="
    pos = content_type.find(marker)
    if pos < 0:
        # Allow a raw audio/wav body for manual tests.
        return body
    boundary = content_type[pos + len(marker):].strip().strip('"')
    delim = b"--" + boundary.encode("ascii")
    parts = body.split(delim)
    for part in parts:
        part = part.strip(b"\r\n")
        if not part or part == b"--":
            continue
        head_end = part.find(b"\r\n\r\n")
        if head_end < 0:
            continue
        headers = part[:head_end].decode("utf-8", "replace")
        if 'name="file"' not in headers:
            continue
        payload = part[head_end + 4:]
        if payload.endswith(b"\r\n"):
            payload = payload[:-2]
        return payload
    raise ValueError("multipart body has no file part")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=18080)
    ap.add_argument("--python", default=os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "dist", "sainput-0.2.2-win64", "runtime", "python.exe"))
    ap.add_argument("--script", default=os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "asr_sidecar.py"))
    ap.add_argument("--model-dir", default=os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "models", "sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05"))
    ap.add_argument("--punct-dir", default=os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "models", "sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8"))
    ap.add_argument("--hotwords", default=os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "hotwords.txt"))
    ap.add_argument("--threads", type=int, default=2)
    ap.add_argument("--test-wav", default=None)
    a = ap.parse_args()

    sidecar = Sidecar(a.python, a.script, a.model_dir, a.punct_dir, a.hotwords, a.threads)
    print("[bridge] loading model...", file=sys.stderr, flush=True)
    sidecar.wait_ready()
    print("[bridge] model ready: " + sidecar.model, file=sys.stderr, flush=True)

    if a.test_wav:
        with open(a.test_wav, "rb") as f:
            pcm = decode_wav(f.read())
        t0 = time.monotonic()
        text = sidecar.transcribe(pcm)
        print(json.dumps({"text": text, "ms": round((time.monotonic() - t0) * 1000, 1)},
                         ensure_ascii=False))
        sidecar.close()
        return 0

    server = ThreadingHTTPServer((a.host, a.port), Handler)
    server.daemon_threads = True
    server.sidecar = sidecar
    print("[bridge] listening on http://%s:%d/v1/audio/transcriptions" % (a.host, a.port),
          file=sys.stderr, flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
        sidecar.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
