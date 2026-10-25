# -*- coding: utf-8 -*-
"""Offline oracle for dumped live sessions.

sainput_doubao_proxy.py --dump-dir writes <session>.wav (exactly the audio the
live recognizer heard) plus <session>.txt (the text the IME committed). This
script replays every dumped wav through a *fresh* sidecar the same way the live
release path does (all audio -> pad 600 ms room noise -> finish) and prints the
two texts side by side.

  live  == oracle  -> the release path is innocent; the tail never arrived or the
                      model itself dropped it.
  live  != oracle  -> the tail is lost between the live audio and the final text.
"""
from __future__ import annotations

import argparse
import base64
import glob
import os
import queue
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe_tail import RATE, Sidecar, chunks_of, read_wav  # noqa: E402


def replay(sc, pcm, pad_ms: int) -> dict:
    sc.send({"cmd": "set", "key": "silence_sec", "value": 0})
    sc.wait_for("set_ack", 5)
    sc.send({"cmd": "reset"})
    sc.wait_for("reset", 5)
    for piece in chunks_of(pcm, 6400):
        sc.send({"cmd": "audio", "pcm": base64.b64encode(piece).decode("ascii"),
                 "sample_rate": RATE})
    if pad_ms:
        sc.send({"cmd": "pad", "ms": pad_ms, "rms": 0.003})
        sc.wait_for("padded", 10)
    sc.send({"cmd": "finish"})
    final = sc.wait_for("final", 60)
    return final or {}


def parse_dump_name(path: str) -> dict:
    """<HHMMSS.mmm>-live<audio_ms>ms-dump<dump_ms>ms-t<text_len>.wav -> metadata.

    Files written before this naming scheme (or by hand) return {}.
    """
    import re
    m = re.search(r"-live(\d+)ms-dump(\d+)ms-t(\d+)$", os.path.basename(path)[:-4])
    if not m:
        return {}
    return {"live_ms": int(m.group(1)), "dump_ms": int(m.group(2)), "live_chars": int(m.group(3))}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--pad-ms", type=int, default=600)
    ap.add_argument("--script", default=os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                                    "asr_sidecar.py"))
    a = ap.parse_args()

    wavs = sorted(glob.glob(os.path.join(a.dir, "*.wav")), key=os.path.getmtime)
    if not wavs:
        print("no dumped sessions in", a.dir)
        return 1
    sc = Sidecar(a.script)
    if sc.wait_for("ready", 90) is None:
        print("sidecar not ready")
        return 1
    same = 0
    compared = 0
    try:
        for wav in wavs[-12:]:
            meta = parse_dump_name(wav)
            pcm = read_wav(wav)
            txt_path = wav[:-4] + ".txt"
            live = ""
            if os.path.exists(txt_path):
                live = open(txt_path, encoding="utf-8").read().strip()
            print("=" * 8, os.path.basename(wav), "audio_ms=%d" % (len(pcm) / 16.0))
            if meta and meta["dump_ms"] < meta["live_ms"]:
                print("  SKIP: dump 只有 %dms,而这次会话送了 %dms —— 60s 上限截断了,"
                      "重放结果不可比" % (meta["dump_ms"], meta["live_ms"]))
                continue
            if meta and meta["live_chars"] == 0:
                out = replay(sc, pcm, a.pad_ms)
                print("  NOTE: 这次现场没有拿到任何文本(t0),不算尾巴丢失;"
                      "重放结果只用来看看这段音频本来能识别出什么")
                print("  tail_chunks=%s" % out.get("tail_chunks"))
                print("  oracle:", repr(str(out.get("text") or "")))
                print("  oracle_raw:", repr(str(out.get("raw") or "")))
                continue
            out = replay(sc, pcm, a.pad_ms)
            oracle = str(out.get("text") or "")
            raw = str(out.get("raw") or "")
            ok = live == oracle
            compared += 1
            same += 1 if ok else 0
            print("  tail_chunks=%s" % out.get("tail_chunks"))
            print("  live  :", repr(live), "MATCH" if ok else "")
            print("  oracle:", repr(oracle), "" if ok else "DIFF")
            if not ok:
                print("  oracle_raw:", repr(raw))
    finally:
        sc.close()
    print("\n%d/%d identical" % (same, compared))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
