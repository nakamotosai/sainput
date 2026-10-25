# -*- coding: utf-8 -*-
"""The proxy must never keep serving audio to a recognizer that is gone.

Regression test for the failure that actually happened on this machine: the
sidecar died between two dictations, nothing recorded it (its stderr went to
DEVNULL, and a dead child raises nothing by itself), and the proxy happily kept
accepting audio -- so every dictation after that showed a live waveform and
returned no text, forever, with only `OSError(22, 'Invalid argument')` in the
log to guess from.

Runs against stub_sidecar.py, so it needs no models and finishes in seconds:
  1. a healthy session returns text
  2. after the recognizer is killed, the watchdog spawns a fresh one
  3. that same proxy then transcribes again, unattended
  4. a recognizer dying mid-session is reported, not swallowed
  5. the next session works, and the child's own stderr was kept
"""
from __future__ import annotations

import json
import os
import re
import signal
import ssl
import subprocess
import sys
import tempfile
import time

from websockets.exceptions import ConnectionClosed
from websockets.sync.client import connect

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from test_doubao_proxy import packet, parse_server

HERE = os.path.dirname(os.path.abspath(__file__))
PROXY = os.path.join(HERE, "sainput_doubao_proxy.py")
STUB = os.path.join(HERE, "stub_sidecar.py")
PORT = 19099
URL = "wss://127.0.0.1:%d/api/v3/sauc/bigmodel_async" % PORT
PKTS = 3          # audio commands per session here
DIE_AFTER = 4     # ... and the stub exits on its 4th one


def session(packets: int = PKTS, timeout: float = 30.0) -> str:
    """One dictation: handshake, N audio packets, then the final packet."""
    ctx = ssl.create_default_context(cafile=os.path.join(HERE, "ca.pem"))
    try:
        with connect(URL, ssl=ctx, max_size=None, open_timeout=15, close_timeout=3) as ws:
            request = {
                "user": {"uid": "respawn-test"},
                "audio": {"format": "pcm", "codec": "raw", "rate": 16000, "bits": 16, "channel": 1},
                "request": {"model_name": "bigmodel"},
            }
            ws.send(packet(0x01, 0x01, 1, json.dumps(request).encode("utf-8")))
            seq = 2
            for i in range(packets):
                last = i == packets - 1
                ws.send(packet(0x02, 0x03 if last else 0x01,
                               -seq if last else seq, b"\x00" * 6400))
                seq += 1
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                try:
                    msg = ws.recv(timeout=0.5)
                except TimeoutError:
                    continue
                parsed = parse_server(msg)
                if parsed and parsed[3]:
                    return parsed[2]
    except (ConnectionClosed, OSError):
        return ""     # the proxy dropped the session; that is a failed dictation
    return ""


def read(path: str) -> str:
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            return f.read()
    except OSError:
        return ""


def wait_log(path: str, needle: str, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if needle in read(path):
            return True
        time.sleep(0.1)
    return False


def main() -> int:
    tmp = tempfile.mkdtemp(prefix="proxy-respawn-")
    proxy_log = os.path.join(tmp, "proxy.log")
    sidecar_log = os.path.join(tmp, "sidecar.log")
    env = dict(os.environ, SAINPUT_PROXY_LOG=proxy_log, SAINPUT_SIDECAR_LOG=sidecar_log,
               STUB_DIE_AFTER_AUDIO=str(DIE_AFTER))
    proc = subprocess.Popen(
        [sys.executable, PROXY, "--port", str(PORT),
         "--python", sys.executable, "--script", STUB,
         "--model-dir", "stub", "--punct-dir", "", "--hotwords", "",
         "--cert", os.path.join(HERE, "server.pem"),
         "--key", os.path.join(HERE, "server.key")],
        env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    checks: "list[tuple[str, bool, str]]" = []

    def check(name: str, ok: bool, detail: str = "") -> None:
        checks.append((name, bool(ok), detail))

    try:
        if not wait_log(proxy_log, "listening on wss", 20.0):
            print("proxy did not start; log:")
            print(read(proxy_log))
            return 1
        m = re.search(r"model ready: pid=(\d+)", read(proxy_log))
        first_pid = int(m.group(1)) if m else -1

        text1 = session()
        check("healthy session returns text", text1 == "字" * PKTS, repr(text1))

        os.kill(first_pid, signal.SIGTERM)
        noticed = wait_log(proxy_log, "sidecar died (pid=%d" % first_pid, 15.0)
        check("death is noticed and logged", noticed, "pid=%d" % first_pid)
        back = re.search(r"sidecar ready \(pid=(\d+)", read(proxy_log))
        new_pid = int(back.group(1)) if back else -1
        check("watchdog spawned a fresh recognizer",
              wait_log(proxy_log, "sidecar ready (pid=", 30.0) and new_pid != first_pid,
              "old=%d new=%d" % (first_pid, new_pid))

        text2 = session()
        check("dictation works again, same proxy", text2 == "字" * PKTS, repr(text2))

        text3 = session()      # the 4th audio command is the stub's last breath
        check("mid-session death returns no text", text3 == "", repr(text3))
        log = read(proxy_log)
        check("failure names the recognizer, not a socket errno",
              "exited (code=3)" in log or "sidecar gone (write failed" in log,
              [ln for ln in log.splitlines() if "session error" in ln][-1:])
        check("no cryptic OSError(22) anywhere", "OSError(22" not in log)

        text4 = session()
        check("and recovers a second time", text4 == "字" * PKTS, repr(text4))
        check("recognizer's own output is kept",
              wait_log(sidecar_log, "stub sidecar up", 5.0), sidecar_log)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except Exception:
            proc.kill()

    failed = 0
    for name, ok, detail in checks:
        if not ok:
            failed += 1
        print("%-42s %s  %s" % (name, "PASS" if ok else "FAIL", detail))
    print("\nlogs: %s" % tmp)
    print("RESULT: %s" % ("PASS" if not failed else "FAIL (%d)" % failed))
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
