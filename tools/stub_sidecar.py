# -*- coding: utf-8 -*-
"""Stand-in for asr_sidecar.py: same JSON protocol, no model, no audio.

Let test_proxy_respawn.py exercise the proxy's recognizer handling -- spawn,
death, respawn -- in about a second instead of loading sherpa-onnx and 400 MB
of models. It answers every command the proxy sends, and can exit abruptly on
demand so the test has a deterministic "the recognizer died" event.

    --die-after-audio N    exit(3) right after answering the Nth audio command
                           (also STUB_DIE_AFTER_AUDIO, because the proxy builds
                           the sidecar's command line itself)
"""
from __future__ import annotations

import argparse
import json
import os
import sys


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model-dir", default="")
    ap.add_argument("--threads", type=int, default=2)
    ap.add_argument("--rule2", default="0")
    ap.add_argument("--punct-mode", default="keep")
    ap.add_argument("--punct-dir", default="")
    ap.add_argument("--hotwords", default="")
    ap.add_argument("--die-after-audio", type=int,
                    default=int(os.environ.get("STUB_DIE_AFTER_AUDIO", "0")))
    a = ap.parse_args()

    def emit(obj: dict) -> None:
        sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
        sys.stdout.flush()

    def note(message: str) -> None:
        sys.stderr.write(message + "\n")
        sys.stderr.flush()

    emit({"type": "ready", "model": "stub"})
    note("stub sidecar up (pid %d)" % os.getpid())

    audio_seen = 0
    text = ""
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except Exception:
            continue
        cmd = msg.get("cmd")
        if cmd == "set":
            emit({"type": "set_ack"})
        elif cmd == "reset":
            text = ""
            emit({"type": "reset"})
        elif cmd == "audio":
            audio_seen += 1
            text += "字"
            emit({"type": "partial", "text": text})
            if a.die_after_audio and audio_seen >= a.die_after_audio:
                note("stub sidecar exiting abruptly after %d audio commands" % audio_seen)
                os._exit(3)
        elif cmd == "render":
            emit({"type": "rendered"})
        elif cmd == "pad":
            emit({"type": "padded"})
        elif cmd == "finish":
            emit({"type": "final", "text": text, "reason": "finish", "tail_chunks": 0})
        elif cmd == "quit":
            return 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
