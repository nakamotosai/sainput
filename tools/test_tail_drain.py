# -*- coding: utf-8 -*-
"""回归测试:定稿时解码落后,最后一个字不能丢。

背景 —— 丢尾巴的真实机制(2026-10-25 复现并修好):
  sidecar 的 `pump()` 每轮最多解 4 帧,那是为了**流式画面**不被解码突刺卡住。
  但这个限量曾经也套在收尾(finish / VAD endpoint)上:只要解码落后过
  (长句、机器忙、模型慢),定稿时 `get_result()` 拿到的就是**还没喂完**的音频,
  最后一个词甚至最后半句直接消失 —— 用户看到的就是「松手之后尾巴被丢掉」。

跑法:
  python tools/test_tail_drain.py
  断言 2.wav 的尾巴(「FREQUENTLY 频繁」)必须在定稿文本里。
  故意一次灌 1.9s 音频制造积压:限量版会丢,修好后不丢。
"""
from __future__ import annotations

import argparse
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe_tail import MODEL_DIR, Sidecar, read_wav  # noqa: E402

WAV = os.path.join(MODEL_DIR, "test_wavs", "2.wav")
TAIL = "频繁"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--script", default=os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                                    "asr_sidecar.py"))
    ap.add_argument("--chunk-bytes", type=int, default=60000,
                    help="每次 audio 命令灌多少字节(默认 1.9s:足以让解码落在后面)")
    a = ap.parse_args()

    pcm = read_wav(WAV)
    raw = pcm.tobytes()
    sc = Sidecar(a.script)
    try:
        if sc.wait_for("ready", 90) is None:
            print("FAIL: sidecar 没有就绪")
            return 1
        sc.send({"cmd": "set", "key": "silence_sec", "value": 0})
        sc.wait_for("set_ack", 5)
        sc.send({"cmd": "reset"})
        sc.wait_for("reset", 5)
        for i in range(0, len(raw), a.chunk_bytes):
            piece = raw[i:i + a.chunk_bytes]
            import base64
            sc.send({"cmd": "audio", "pcm": base64.b64encode(piece).decode("ascii"),
                     "sample_rate": 16000})
        sc.send({"cmd": "pad", "ms": 600, "rms": 0.003})
        sc.wait_for("padded", 10)
        sc.send({"cmd": "finish"})
        final = sc.wait_for("final", 60) or {}
        text = str(final.get("text") or "")
        print("final :", repr(text))
        print("tail_chunks:", final.get("tail_chunks"))
        ok = "FREQUENTLY" in text and TAIL in text.split("FREQUENTLY", 1)[1]
        print("PASS" if ok else "FAIL: 尾巴(%s)被丢掉了" % TAIL)
        return 0 if ok else 1
    finally:
        sc.close()


if __name__ == "__main__":
    raise SystemExit(main())
