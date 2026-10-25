# -*- coding: utf-8 -*-
"""Smoke-test the local Doubao-compatible streaming proxy."""
from __future__ import annotations

import gzip
import json
import os
import ssl
import struct
import sys
import time

from websockets.sync.client import connect

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from local_asr_http import decode_wav

HERE = os.path.dirname(os.path.abspath(__file__))
WAV = r"F:\projects\sainput\models\sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20\test_wavs\0.wav"


def packet(mtype: int, flags: int, sequence: int, payload: bytes) -> bytes:
    compressed = gzip.compress(payload)
    return (
        bytes([0x11, (mtype << 4) | flags, 0x11, 0x00])
        + struct.pack(">i", sequence)
        + struct.pack(">I", len(compressed))
        + compressed
    )


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
    size = struct.unpack(">I", message[offset : offset + 4])[0]
    offset += 4
    payload = message[offset : offset + size]
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


def main() -> int:
    with open(WAV, "rb") as f:
        pcm = decode_wav(f.read())
    raw = pcm.tobytes()
    print("pcm bytes", len(raw))

    ctx = ssl.create_default_context(cafile=os.path.join(HERE, "ca.pem"))
    with connect(
        "wss://127.0.0.1:19000/api/v3/sauc/bigmodel_async",
        ssl=ctx,
        max_size=None,
        open_timeout=10,
        close_timeout=3,
    ) as ws:
        request = {
            "user": {"uid": "test"},
            "audio": {"format": "pcm", "codec": "raw", "rate": 16000, "bits": 16, "channel": 1},
            "request": {"model_name": "bigmodel"},
        }
        ws.send(packet(0x01, 0x01, 1, json.dumps(request).encode("utf-8")))
        seq = 2
        chunk = 6400
        for i in range(0, len(raw), chunk):
            piece = raw[i : i + chunk]
            flags = 0x01
            last = i + chunk >= len(raw)
            if last:
                flags = 0x03
            ws.send(packet(0x02, flags, -seq if last else seq, piece))
            seq += 1
            time.sleep(0.02)
            while True:
                try:
                    msg = ws.recv(timeout=0.001)
                except TimeoutError:
                    break
                parsed = parse_server(msg)
                if parsed:
                    mtype, flags, text, is_last = parsed
                    print("recv", mtype, hex(flags), repr(text), "last" if is_last else "")
                    if is_last:
                        print("FINAL:", repr(text))
                        return 0
        # Drain until the final response.
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            try:
                msg = ws.recv(timeout=0.5)
            except TimeoutError:
                continue
            parsed = parse_server(msg)
            if parsed:
                mtype, flags, text, is_last = parsed
                print("recv", mtype, hex(flags), repr(text), "last" if is_last else "")
                if is_last:
                    print("FINAL:", repr(text))
                    return 0
        print("no final response")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
