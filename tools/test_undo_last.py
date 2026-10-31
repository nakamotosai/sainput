# -*- coding: utf-8 -*-
"""T5.2「收回上一句」回归(私有桌面,用户看不见)。

判据是**一对**:
  1) 打开收回(`SAINPUT_DIAG_UNDO_AFTER_MS`)→ 那一句话必须从靶子里**消失**(EDIT 为空);
  2) 同样的流程、不收 → 那句话必须在(正好"你好世界")。
第二条是必须的:没有它,第一条在"整条链路根本没上屏"时也会绿 —— 而那种绿毫无意义。

为什么用消息通道(`--ime-wmchar on`)来验:收回的退格必须**按本句当初那条通道**退
(`backspace_with`);用消息通道跑,正好逼到"退格走 WM_KEYDOWN/UP"那半边 ——
直接调 `inject::backspace`(只会 SendInput)在这条路上一个都退不掉。

跑法:
    cargo build --release --target-dir target-test -p sainput-cli -p sainput-tsf-host
    python tools/test_undo_last.py
"""
import os
import pathlib
import sys
import time
import wave

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hdesk  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parent.parent
DIRS = ["target-t14b/x86_64-pc-windows-gnu/release", "target-test/x86_64-pc-windows-gnu/release",
        "target/x86_64-pc-windows-gnu/release", "target/release"]
DESKTOP = "SAI-UndoLast"
NS = "sainput-undotest"
ENV = ("SAINPUT_SINGLE_NS", "SAINPUT_DIAG_AUTOSTART", "SAINPUT_DIAG_TARGET_HWND",
       "FAKE_ASR_SCRIPT", "SAINPUT_DIAG_UNDO_AFTER_MS")


def find(rel, what):
    for d in DIRS:
        p = REPO / d / rel
        if p.exists():
            return p
    print(f"FAIL: {what} not built")
    sys.exit(2)


def make_wav(path):
    if path.exists():
        return path
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(16000)
        w.writeframes(b"\x00\x00" * 6400)
    return path


def run_case(host, sainput, wav, undo_after_ms, expect):
    h = hdesk.open_desktop(DESKTOP)
    saved = {k: os.environ.get(k) for k in ENV}
    hh = hp = None
    hpid = epid = 0
    try:
        os.environ["SAINPUT_SINGLE_NS"] = NS
        os.environ["SAINPUT_DIAG_AUTOSTART"] = "1200"
        os.environ["FAKE_ASR_SCRIPT"] = "w:400;p:你好世;p:你好世界;w:300;f:你好世界:finish;w:600"
        hh, ht, hpid = hdesk.launch(str(host), f"--life 26000 --log {REPO / 'tmp' / 'undo-host.log'}", DESKTOP)
        edit = None
        t0 = time.time()
        while time.time() - t0 < 10:
            edit = hdesk.find_edit(hpid, h)
            if edit:
                break
            time.sleep(0.2)
        if not edit:
            return False, "宿主窗口没起来"
        os.environ["SAINPUT_DIAG_TARGET_HWND"] = hex(edit)
        if undo_after_ms:
            os.environ["SAINPUT_DIAG_UNDO_AFTER_MS"] = str(undo_after_ms)
        else:
            os.environ.pop("SAINPUT_DIAG_UNDO_AFTER_MS", None)
        logdir = REPO / "tmp" / "engine-undo"
        logdir.mkdir(parents=True, exist_ok=True)
        for f in logdir.glob("sainput-*.log"):
            f.unlink()
        hp, hpt, epid = hdesk.launch(
            str(sainput),
            "run --script " + str(REPO / "tools" / "fake_asr_sidecar.py")
            + f' --python "{sys.executable}"'
            + f" --feed {wav} --feed-restart"
            + " --ime-wmchar on --no-leading-space --no-trailing-space --overlay-mode off"
            + f" --log-dir {logdir}",
            DESKTOP)
        time.sleep(7.0)
        got = hdesk.read_edit(edit)
        logs = sorted(logdir.glob("sainput-*.log"))
        txt = logs[-1].read_text(encoding="utf-8", errors="replace") if logs else ""
        detail = f"text={got!r} expect={expect!r}"
        if undo_after_ms:
            detail += f" | 日志有收回={('收回上一句' in txt)}"
        hdesk.close(hpt)
        return got == expect, detail
    finally:
        if hp:
            hdesk.terminate(epid)
            hdesk.close(hp)
        if hh:
            hdesk.terminate(hpid)
            hdesk.close(ht)
            hdesk.close(hh)
        hdesk.close(h)
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


def main():
    host = find("sainput-tsf-host.exe", "host")
    sainput = find("sainput.exe", "engine")
    tmp = REPO / "tmp"
    tmp.mkdir(exist_ok=True)
    wav = make_wav(tmp / "undo-silence.wav")
    fails = []
    for name, undo_ms, expect in (
        ("收回上一句:那句话必须消失(T5.2)", 1200, ""),
        ("对照:不收回时必须留着那句话", 0, "你好世界"),
    ):
        ok, detail = run_case(host, sainput, wav, undo_ms, expect)
        print(f"[{'PASS' if ok else 'FAIL'}] {name}: {detail}")
        if not ok:
            fails.append(name)
    print("\n" + ("ALL PASS" if not fails else f"FAILED: {', '.join(fails)}"))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
