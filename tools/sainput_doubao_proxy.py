# -*- coding: utf-8 -*-
"""Doubao-compatible streaming WebSocket front end for sainput's local ASR sidecar.

The water-fir IME has a native Doubao streaming client (partial preedit + final
commit).  This process speaks enough of that protocol for the client to stream
16 kHz PCM here, while the actual recognition stays on sainput's sherpa-onnx
sidecar.  It is a local-only bridge; no audio leaves the machine.
"""
from __future__ import annotations

import argparse
import base64
import ctypes
import ctypes.wintypes as wintypes
import glob
import gzip
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

from websockets.exceptions import ConnectionClosed
from websockets.sync.server import serve

from local_asr_http import Sidecar

LOG_PATH = os.environ.get("SAINPUT_PROXY_LOG") or os.path.join(
    os.environ.get("TEMP", "."), "sainput_doubao_proxy.log")
SIDECAR_LOG_PATH = os.environ.get("SAINPUT_SIDECAR_LOG") or os.path.join(
    os.environ.get("TEMP", "."), "sainput_sidecar.log")


def mutex_name(port: int) -> str:
    """One proxy per port. The supervisor watches the default port's name."""
    return "Local\\SainputDoubaoProxy" if port == 19000 else "Local\\SainputDoubaoProxy-%d" % port


def acquire_single_instance(name: str):
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateMutexW.argtypes = [wintypes.LPVOID, wintypes.BOOL, wintypes.LPCWSTR]
    kernel32.CreateMutexW.restype = wintypes.HANDLE
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel32.CreateMutexW(None, False, name)
    if not handle:
        return None
    if ctypes.get_last_error() == 183:  # ERROR_ALREADY_EXISTS
        kernel32.CloseHandle(handle)
        return None
    return handle


def open_sidecar_log(max_bytes: int = 2 << 20):
    """Give the recognizer a place to say why it died.

    Its stderr used to go to DEVNULL: a sidecar that exited left nothing behind
    but a proxy that kept accepting audio and returned no text, which is
    indistinguishable from a slow model. Rotated by size so it cannot grow.
    """
    try:
        if os.path.exists(SIDECAR_LOG_PATH) and os.path.getsize(SIDECAR_LOG_PATH) > max_bytes:
            os.remove(SIDECAR_LOG_PATH)
        return open(SIDECAR_LOG_PATH, "ab", buffering=0)
    except Exception as exc:
        log("sidecar log unusable:", SIDECAR_LOG_PATH, repr(exc))
        return subprocess.DEVNULL


def log(*args) -> None:
    now = time.time()
    stamp = time.strftime("%H:%M:%S", time.localtime(now)) + (".%03d" % int((now % 1.0) * 1000))
    line = "[doubao-proxy] " + stamp + " " + " ".join(str(a) for a in args)
    try:
        with open(LOG_PATH, "a", encoding="utf-8") as f:
            f.write(line + "\n")
    except Exception:
        pass
    try:
        stream = sys.stderr or sys.stdout
        if stream is not None:
            print(line, file=stream, flush=True)
    except Exception:
        pass


def parse_packet(message: bytes):
    """Return (message_type, flags, sequence, payload) or None."""
    if len(message) < 4:
        return None
    header_size = (message[0] & 0x0F) * 4
    if header_size > len(message):
        return None
    message_type = message[1] >> 4
    flags = message[1] & 0x0F
    serialization = message[2] >> 4
    compression = message[2] & 0x0F
    offset = header_size
    sequence = None
    if flags & 0x01:
        if offset + 4 > len(message):
            return None
        sequence = struct.unpack(">i", message[offset : offset + 4])[0]
        offset += 4
    if flags & 0x04:
        offset += 4
    if message_type == 0x0F:
        if offset + 8 > len(message):
            return None
        code = struct.unpack(">i", message[offset : offset + 4])[0]
        size = struct.unpack(">I", message[offset + 4 : offset + 8])[0]
        offset += 8
    else:
        if offset + 4 > len(message):
            return None
        size = struct.unpack(">I", message[offset : offset + 4])[0]
        offset += 4
    payload = message[offset : offset + min(size, len(message) - offset)]
    if compression == 0x01 and payload:
        try:
            payload = gzip.decompress(payload)
        except Exception:
            return None
    if serialization not in (0x01, 0x00):
        pass
    return message_type, flags, sequence, payload


def server_packet(text: str, last: bool) -> bytes:
    body = json.dumps({"result": {"text": text}, "code": 0}, ensure_ascii=False).encode("utf-8")
    compressed = gzip.compress(body)
    flags = 0x02 if last else 0x00
    header = bytes([0x11, (0x09 << 4) | flags, 0x11, 0x00])
    return header + struct.pack(">I", len(compressed)) + compressed


class Proxy:
    def __init__(self, sidecar: Sidecar, dump_dir: str = ""):
        self.sidecar = sidecar
        self.lock = threading.Lock()
        self.dump_dir = dump_dir
        if dump_dir:
            try:
                os.makedirs(dump_dir, exist_ok=True)
            except Exception as exc:
                log("dump dir unusable:", dump_dir, repr(exc))
                self.dump_dir = ""
        # The recognizer can die outside any session (crash, something killing
        # it, a device event). Serving audio into a dead one used to mean
        # "waveform moves, no text ever again" with nothing in any log. Watch it
        # and put a fresh one in place instead.
        threading.Thread(target=self._sidecar_watchdog, daemon=True).start()

    def ensure_sidecar(self, timeout: float = 150.0) -> None:
        """Wait for a live recognizer, respawning one that died.

        Called at the start of a session, so a dictation that lands while the
        recognizer is being replaced still gets its text. `Sidecar.restart`
        serialises with the watchdog, so only one child is ever spawned.
        """
        if self.sidecar.alive():
            return
        t0 = time.monotonic()
        log("session waiting for recognizer")
        self.sidecar.restart(timeout)
        log("recognizer back (pid=%s, %.1fs)" % (self.sidecar.pid, time.monotonic() - t0))

    def _sidecar_watchdog(self) -> None:
        failures = 0
        while True:
            time.sleep(2.0)
            if self.sidecar.alive():
                failures = 0
                continue
            try:
                self.sidecar.restart()
            except Exception as exc:
                failures += 1
                log("sidecar watchdog: respawn failed (%d): %r" % (failures, exc))
                time.sleep(min(60.0, 5.0 * failures))

    def dump_session(self, pcm: bytearray, text: str, audio_ms: int) -> None:
        """Write one session's raw audio + its transcript next to it.

        Off unless --dump-dir is given. This is the only way to answer "was the
        tail ever recorded?": replay the exact audio offline and compare.

        The name carries what the reader needs to avoid a false verdict:
          <HHMMSS.mmm>-live<audio_ms>ms-dump<dump_ms>ms-t<len(text)>.wav/.txt
        `dump < live` means the capture was cut short (60 s cap) and the replay
        is NOT comparable; `t0` means the live path returned no text at all.
        """
        if not self.dump_dir or not pcm:
            return
        try:
            dump_ms = len(pcm) // 32
            stamp = time.strftime("%H%M%S") + (".%03d" % int((time.time() % 1.0) * 1000))
            base = os.path.join(self.dump_dir,
                                "%s-live%dms-dump%dms-t%d" % (stamp, audio_ms, dump_ms, len(text)))
            with wave.open(base + ".wav", "wb") as wf:
                wf.setnchannels(1)
                wf.setsampwidth(2)
                wf.setframerate(16000)
                wf.writeframes(bytes(pcm))
            with open(base + ".txt", "w", encoding="utf-8") as f:
                f.write(text)
            files = sorted(glob.glob(os.path.join(self.dump_dir, "*.wav")), key=os.path.getmtime)
            for old in files[:-20]:
                for path in (old, old[:-4] + ".txt"):
                    try:
                        os.remove(path)
                    except Exception:
                        pass
        except Exception as exc:
            log("dump failed:", repr(exc))

    def wait_for(self, kind: str, timeout: float, allow_partial: bool = False):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                msg = self.sidecar.q.get(timeout=min(0.5, max(0.05, deadline - time.monotonic())))
            except queue.Empty:
                continue
            t = msg.get("type")
            if t == "error":
                raise RuntimeError("sidecar: " + str(msg.get("msg")))
            if t == "partial" and allow_partial:
                return msg
            if t == kind:
                return msg
        raise TimeoutError("sidecar timed out waiting for %s" % kind)

    def drain(self, send_partial) -> str | None:
        """Forward queued partials; return final text when a final event arrives."""
        final_text = None
        while True:
            try:
                msg = self.sidecar.q.get_nowait()
            except queue.Empty:
                break
            t = msg.get("type")
            if t == "error":
                raise RuntimeError("sidecar: " + str(msg.get("msg")))
            if t == "partial":
                text = str(msg.get("text") or "")
                if text:
                    send_partial(text)
            elif t == "final":
                final_text = str(msg.get("text") or "")
        return final_text

    def handle(self, ws) -> None:
        if not self.lock.acquire(blocking=False):
            ws.close(1013, "another session is active")
            return
        got_audio = False
        last_text = ""
        audio_bytes = 0
        audio_pkts = 0
        tail_bytes = 0
        t_connect = time.monotonic()
        t_first_audio = 0.0
        t_last_audio = 0.0
        session_pcm = bytearray()
        final_text = ""
        tail_chunks = 0
        try:
            # A dictation that lands while the recognizer is being replaced still
            # gets its text instead of failing on a socket error. Deaths are
            # logged by the watchdog thread.
            self.ensure_sidecar()
            # Clear any leftovers from a previous session, and make the sidecar
            # end utterances only when we ask (the IME commits on key release).
            self.sidecar.send({"cmd": "set", "key": "silence_sec", "value": 0})
            self.sidecar.send({"cmd": "reset"})
            self.wait_for("set_ack", 5.0)
            self.wait_for("reset", 5.0)
            log("client connected")

            while True:
                try:
                    raw = ws.recv(timeout=60)
                except TimeoutError:
                    log("idle timeout")
                    break
                except ConnectionClosed:
                    break
                if not isinstance(raw, (bytes, bytearray)):
                    continue
                parsed = parse_packet(bytes(raw))
                if not parsed:
                    continue
                message_type, flags, sequence, payload = parsed
                if message_type == 0x01:
                    try:
                        request = json.loads(payload.decode("utf-8"))
                        audio = request.get("audio", {})
                        log("handshake", audio.get("rate"), audio.get("format"), audio.get("channel"))
                    except Exception:
                        pass
                    continue
                if message_type != 0x02:
                    continue

                if message_type == 0x02 and payload:
                    audio_bytes += len(payload)
                    audio_pkts += 1
                    # Dumps stop at 60 s; the file name records it so a short dump
                    # can never be mistaken for the whole session.
                    if len(session_pcm) + len(payload) <= 16000 * 2 * 60:
                        session_pcm.extend(payload)
                    if not t_first_audio:
                        t_first_audio = time.monotonic()
                    t_last_audio = time.monotonic()
                    if flags & 0x02:
                        tail_bytes = len(payload)

                if payload:
                    got_audio = True
                    self.sidecar.send({
                        "cmd": "audio",
                        "pcm": base64.b64encode(payload).decode("ascii"),
                        "sample_rate": 16000,
                    })
                    # The sidecar emits at most one partial per audio command.
                    deadline = time.monotonic() + 0.8
                    while time.monotonic() < deadline:
                        try:
                            msg = self.sidecar.q.get(timeout=0.05)
                        except queue.Empty:
                            break
                        if msg.get("type") == "error":
                            raise RuntimeError("sidecar: " + str(msg.get("msg")))
                        if msg.get("type") == "partial":
                            text = str(msg.get("text") or "")
                            if text and text != last_text:
                                last_text = text
                                ws.send(server_packet(text, last=False))
                        elif msg.get("type") == "final":
                            # VAD is disabled, so a final here is unexpected.
                            pass

                if flags & 0x02:
                    # Client's negative-sequence final packet.
                    if payload:
                        pass
                    if not got_audio:
                        ws.send(server_packet("", last=True))
                        break
                    # Give the model a little room noise so the last syllable
                    # clears its right context (sainput's frontend does the same).
                    t_release = time.monotonic()
                    self.sidecar.send({"cmd": "pad", "ms": 600, "rms": 0.003})
                    try:
                        self.wait_for("padded", 5.0)
                    except TimeoutError:
                        pass
                    t_padded = time.monotonic()
                    self.sidecar.send({"cmd": "finish"})
                    final_text = ""
                    deadline = time.monotonic() + 20.0
                    while time.monotonic() < deadline:
                        try:
                            msg = self.sidecar.q.get(timeout=0.5)
                        except queue.Empty:
                            continue
                        t = msg.get("type")
                        if t == "error":
                            raise RuntimeError("sidecar: " + str(msg.get("msg")))
                        if t == "partial":
                            text = str(msg.get("text") or "")
                            if text and text != last_text:
                                last_text = text
                                ws.send(server_packet(text, last=False))
                        elif t == "final":
                            final_text = str(msg.get("text") or "")
                            tail_chunks = int(msg.get("tail_chunks") or 0)
                            break
                    if final_text and final_text != last_text:
                        ws.send(server_packet(final_text, last=False))
                    ws.send(server_packet(final_text, last=True))
                    log("final", repr(final_text))
                    # One summary line per session: audio_ms is the speech actually
                    # delivered to the recognizer, so it can be compared against the
                    # real hold time (sainput_x1_ralt.log) to see whether the tail
                    # never left the IME or was lost here.
                    log("session audio_ms=%d pkts=%d tail_bytes=%d tail_chunks=%d first_audio=+%dms speech_span=%dms "
                        "pad=%dms finish=%dms listen=%dms dump_ms=%d"
                        % (audio_bytes // 32, audio_pkts, tail_bytes, tail_chunks,
                           int((t_first_audio - t_connect) * 1000) if t_first_audio else -1,
                           int((t_last_audio - t_first_audio) * 1000) if t_first_audio else -1,
                           int((t_padded - t_release) * 1000),
                           int((time.monotonic() - t_padded) * 1000),
                           int((time.monotonic() - t_connect) * 1000),
                           len(session_pcm) // 32))
                    break
        except Exception as exc:
            log("session error:", repr(exc))
            try:
                ws.close(1011, "proxy error")
            except Exception:
                pass
        finally:
            try:
                self.sidecar.send({"cmd": "reset"})
            except Exception:
                pass
            if session_pcm:
                # Keep the audio even when no text came back: that is exactly the
                # "pipeline returned nothing" case worth replaying. The t0 marker
                # in the file name stops the oracle from calling it a tail loss.
                self.dump_session(session_pcm, final_text, audio_ms=audio_bytes // 32)
            self.lock.release()
            log("client disconnected")


def main() -> int:
    here = os.path.dirname(os.path.abspath(__file__))
    repo = os.path.dirname(here)
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=19000)
    ap.add_argument("--cert", default=os.path.join(here, "server.pem"))
    ap.add_argument("--key", default=os.path.join(here, "server.key"))
    ap.add_argument("--python", default=os.path.join(repo, "dist", "sainput-0.2.2-win64", "runtime", "python.exe"))
    ap.add_argument("--script", default=os.path.join(here, "asr_sidecar.py"))
    ap.add_argument("--model-dir", default=os.path.join(
        repo, "models", "sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05"))
    ap.add_argument("--punct-dir", default=os.path.join(
        repo, "models", "sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8"))
    ap.add_argument("--hotwords", default=os.path.join(repo, "hotwords.txt"))
    ap.add_argument("--threads", type=int, default=2)
    ap.add_argument("--dump-dir", default="",
                    help="把每次会话的原始音频+最终文本写到这个目录(默认关;排查丢尾巴用)")
    args = ap.parse_args()

    mutex = acquire_single_instance(mutex_name(args.port))
    if mutex is None:
        log("already running; exit")
        return 0
    log("starting pid=%d port=%d" % (os.getpid(), args.port))

    sidecar = Sidecar(args.python, args.script, args.model_dir, args.punct_dir, args.hotwords, args.threads,
                      stderr=open_sidecar_log(), log=log)
    log("loading model...")
    try:
        sidecar.wait_ready()
    except Exception as exc:
        # A recognizer that will not come up must say so here, or every
        # dictation after it turns into a silent "no text".
        log("model load failed: %r" % (exc,))
        sidecar.close()
        return 1
    log("model ready: pid=%d %s" % (sidecar.pid, sidecar.model))

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(certfile=args.cert, keyfile=args.key)
    proxy = Proxy(sidecar, args.dump_dir)
    with serve(proxy.handle, args.host, args.port, ssl=context, max_size=None) as server:
        log("listening on wss://%s:%d/api/v3/sauc/bigmodel_async" % (args.host, args.port))
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass
    sidecar.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
