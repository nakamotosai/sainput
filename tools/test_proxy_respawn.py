# -*- coding: utf-8 -*-
"""The proxy must never keep serving audio to a recognizer that is gone.

Regression test for the failure that actually happened on this machine: the
sidecar died between two dictations, nothing recorded it (its stderr went to
DEVNULL, and a dead child raises nothing by itself), and the proxy happily kept
accepting audio -- so every dictation after that showed a live waveform and
returned no text, forever, with only `OSError(22, 'Invalid argument')` in the
log to guess from.

It also pins the two ways the *recovery* itself could go wrong:
  - a respawn racing the previous child's EOF (false death, duplicate child)
  - a recognizer swapped in mid-dictation, where committing the replacement's
    (empty) answer would look like the silent failure this exists to remove

Runs against stub_sidecar.py, so it needs no models and finishes in seconds.
Recognizers are killed from the outside with os.kill, so every phase is
deterministic; stub_sidecar's own counter (`DIE_AFTER`, per process) is only
used for the "it crashed on its own mid-session" phase. Every session here
sends PKTS = 3 audio commands, and each stub instance is replaced before its
counter can reach DIE_AFTER except in that phase.

Not pinned here: the EOF/`_spawn` race (a reader thread finishing on the
previous child while a respawn installs the next one). Its window is
sub-millisecond in production and it is closed by `Sidecar.state_lock`;
reproducing it needs a deliberately widened window, unlike everything above.
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
PKTS = 3          # audio commands per session
DIE_AFTER = 4     # ... and the stub exits by itself on its 4th one


class StagingError(RuntimeError):
    """A phase's own setup step failed (e.g. killing an already-dead pid).

    Raised instead of riding on `session()`'s (text, closed) contract, so a
    broken test phase fails loudly instead of looking like a proxy behavior.
    """


def session(packets: int = PKTS, timeout: float = 30.0, between=None) -> "tuple[str, str | None]":
    """One dictation: handshake, `packets` audio commands, hook, release packet.

    `between` runs after the audio and before the release, which is how a
    recognizer death *during* a dictation is staged deterministically.

    Returns (text, closed). `closed` is non-None when the proxy dropped the
    session instead of finishing it: a failure has to surface as an error or a
    close, never as a clean commit of partial text.
    """
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
            for _ in range(packets):
                ws.send(packet(0x02, 0x01, seq, b"\x00" * 6400))
                seq += 1
            if between is not None:
                # Wait for the preedit the recognizer sends for those audio
                # commands: it proves the proxy has forwarded them, so the hook
                # stages a death *after* this utterance's audio, not before it.
                deadline = time.monotonic() + 5.0
                while time.monotonic() < deadline:
                    try:
                        msg = ws.recv(timeout=0.2)
                    except TimeoutError:
                        continue
                    parsed = parse_server(msg)
                    if parsed and parsed[2] and not parsed[3]:
                        break
                try:
                    between()
                except Exception as exc:
                    raise StagingError(repr(exc)) from exc
            ws.send(packet(0x02, 0x03, -seq, b""))     # client's release packet
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                try:
                    msg = ws.recv(timeout=0.5)
                except TimeoutError:
                    continue
                parsed = parse_server(msg)
                if parsed and parsed[3]:
                    return parsed[2], None
    except ConnectionClosed as exc:
        return "", "closed:%s" % (getattr(exc, "code", None) or "?")
    except OSError as exc:
        return "", "socket:%r" % (exc,)
    return "", "timeout"


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

    def recognizers() -> "list[int]":
        """Pids of every recognizer this proxy has run, oldest first."""
        return [int(x) for x in
                re.findall(r"(?:model ready: |sidecar ready \()pid=(\d+)", read(proxy_log))]

    def kill_current() -> int:
        """Kill the recognizer the proxy is using right now; returns its pid."""
        for _ in range(3):
            pid = recognizers()[-1]
            try:
                os.kill(pid, signal.SIGTERM)
                return pid
            except OSError:
                time.sleep(0.2)     # already gone; its replacement is on the way
        raise StagingError("no live recognizer to kill")

    def wait_respawn(pid: int, timeout: float = 30.0) -> int:
        """Wait until the newest recognizer is not `pid`; return it (-1 on timeout)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            pids = recognizers()
            if pids and pids[-1] != pid:
                return pids[-1]
            time.sleep(0.1)
        return -1

    try:
        if not wait_log(proxy_log, "listening on wss", 20.0):
            print("proxy did not start; log:")
            print(read(proxy_log))
            return 1

        # 1. the path works at all
        text1, close1 = session()
        check("healthy dictation returns text", text1 == "字" * PKTS and close1 is None,
              "%r %s" % (text1, close1))

        # 2. a recognizer killed from outside is noticed and replaced
        first = kill_current()
        check("death is noticed and logged",
              wait_log(proxy_log, "sidecar died (pid=%d" % first, 15.0), "pid=%d" % first)
        now = wait_respawn(first)     # != first by construction; can be -1 (timeout)
        check("watchdog spawned a fresh recognizer", now > 0,
              "old=%d new=%d" % (first, now))

        # 3. the same proxy transcribes again, unattended
        text2, close2 = session()
        check("dictation works again, same proxy", text2 == "字" * PKTS and close2 is None,
              "%r %s" % (text2, close2))

        # 4. a recognizer that dies on its own mid-dictation is reported
        mark = len(read(proxy_log))
        text3, close3 = session()
        tail = read(proxy_log)[mark:]
        check("crash mid-dictation commits nothing",
              text3 == "" and " final " not in tail,
              "%r %r" % (text3, [ln for ln in tail.splitlines() if "final" in ln]))
        check("and is reported, never silently committed",
              close3 is not None or "session error" in tail,
              "close=%s tail=%r" % (close3, tail.strip().splitlines()[-2:]))
        check("error names the recognizer, not a socket errno",
              "exited (code=3)" in tail or "sidecar gone (write failed" in tail
              or "replaced mid-session" in tail or "replaced during finish" in tail,
              [ln for ln in tail.splitlines() if "session error" in ln][-1:])

        # 5. ... and still recovers
        text4, close4 = session()
        check("recovers after the crash", text4 == "字" * PKTS and close4 is None,
              "%r %s" % (text4, close4))

        # 6. a recognizer swapped *between the audio and the release* must fail
        #    loudly: the audio already sent went to the dead child, so a "final"
        #    from its replacement is not an answer to this utterance. Start from a
        #    freshly spawned recognizer so the stub's own counter cannot fire here
        #    (the death in this phase is the one swap() stages).
        wait_respawn(kill_current())
        mark = len(read(proxy_log))
        swapped: "list[bool]" = []

        def swap() -> None:
            # Wait out the proxy's post-audio drain window (0.8 s) before killing.
            # Inside it, the queued death message fires first and the session_pid
            # guard is never reached -- which is exactly how a first version of
            # this phase passed with the guard deleted.
            time.sleep(1.0)
            pid = kill_current()
            swapped.append(wait_respawn(pid) > 0)

        text5, close5 = session(packets=1, between=swap)
        tail = read(proxy_log)[mark:]
        check("recognizer swapped before the kill", swapped == [True], repr(swapped))
        # Both conjuncts can fail: text is what the client would have typed, and
        # the log line is how a silent commit would show up.
        check("swap mid-dictation commits nothing",
              text5 == "" and " final " not in tail,
              "%r %r" % (text5, [ln for ln in tail.splitlines() if "final" in ln]))
        check("swap mid-dictation is caught by the session_pid guard",
              "replaced mid-session" in tail or "replaced during finish" in tail,
              "close=%s %r" % (close5, [ln for ln in tail.splitlines() if "session error" in ln][-1:]))
        check("no cryptic OSError(22) anywhere", "OSError(22" not in read(proxy_log))

        # 7. and the next dictation works against the replacement
        text6, close6 = session()
        check("recovers after the swap", text6 == "字" * PKTS and close6 is None,
              "%r %s" % (text6, close6))

        # 8. the child's own output was kept (stderr used to go to DEVNULL)
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
        print("%-46s %s  %s" % (name, "PASS" if ok else "FAIL", detail))
    print("\nlogs: %s" % tmp)
    print("RESULT: %s" % ("PASS" if not failed else "FAIL (%d)" % failed))
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
