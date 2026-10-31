#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""假 ASR 侧车:按脚本吐 partial/final,用来回归上屏竞态。

为什么需要它:`sainput run` 里那套上屏/竞态逻辑(迟到帧、重复定稿、焦点变更)
只有把**真实的 Msg 序列**喂进真实的 worker 才算验过 —— 而真 ASR 吐不出确定的序列
(要麦克风、还随机、还慢)。这个假侧车说**同一套 JSON 协议**,把内容换成脚本里的固定值,
于是整条链路(热键 → worker → StreamWriter → 真注入)可以无人值守地复跑。

协议与 `tools/asr_sidecar.py` 一致:
  in : {"cmd":"audio",...} / {"cmd":"pad",...} / {"cmd":"finish"} / {"cmd":"reset"} /
       {"cmd":"set",...} / {"cmd":"render",...} / {"cmd":"quit"}
  out: {"type":"ready",...} / {"type":"partial",...} / {"type":"final",...} /
       {"type":"reset_ack"}

脚本由环境变量 `FAKE_ASR_SCRIPT` 给,分号分隔:
  w:<ms>            等 ms 毫秒
  p:<text>          发一条 partial
  f:<text>:<reason> 发一条 final(reason: finish / endpoint)
  r                 发 reset_ack
脚本在**收到第一条 audio** 之后开始跑(也就是"这一句真的开始了"),不需要跟热键对表。
不设环境变量时用默认序列。
"""
import json
import os
import sys
import threading
import time

DEFAULT_SCRIPT = "w:500;p:你好;w:200;p:你好世界;w:200;f:你好世界:finish;w:500;p:迟到的一帧;w:300"


def emit(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def run_script(script):
    for step in script.split(";"):
        step = step.strip()
        if not step:
            continue
        kind, _, rest = step.partition(":")
        try:
            if kind == "w":
                time.sleep(int(rest) / 1000.0)
            elif kind == "p":
                emit({"type": "partial", "text": rest, "raw": rest, "ms": 1.0, "errors": 0})
            elif kind == "f":
                text, _, reason = rest.rpartition(":")
                emit({"type": "final", "text": text, "raw": text,
                      "reason": reason or "finish", "tail_chunks": 0})
            elif kind == "r":
                emit({"type": "reset_ack"})
        except Exception as e:  # 脚本写错也要留下痕迹,不能让它静默地什么都不发
            emit({"type": "error", "msg": f"fake script step {step!r}: {e}", "n": 1})
    emit({"type": "dbg", "n": 0, "still_ready": True})


def main():
    script = os.environ.get("FAKE_ASR_SCRIPT") or DEFAULT_SCRIPT
    emit({"type": "ready", "model": "fake-asr", "decoding": "scripted",
          "punct": False, "itn": False, "hotwords": 0})
    started = threading.Event()

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except Exception:
            continue
        cmd = msg.get("cmd")
        if cmd == "audio":
            if not started.is_set():
                started.set()
                threading.Thread(target=run_script, args=(script,), daemon=True).start()
        elif cmd == "reset":
            emit({"type": "reset_ack"})
        elif cmd == "quit":
            return 0
        # pad / finish / set / render:脚本自己安排时间线,这里一律忽略
    return 0


if __name__ == "__main__":
    sys.exit(main())
