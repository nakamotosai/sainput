# -*- coding: utf-8 -*-
"""T4.1–T4.3 回归:三条竞态防护(README §14 承诺过的那三条)。

判据(全部在**真实的** worker + 真实注入链路上验,不是纯函数自说自话):
  1. **final 之后迟到的 streaming 一律丢弃** —— 屏幕上只有定稿那一版,迟到帧一个字都不许出现;
  2. **重复定稿**不许打两遍。

做法:用假侧车(`tools/fake_asr_sidecar.py`)把确定的 Msg 序列喂进真 worker,
引擎与靶子窗口都跑在**私有桌面**上,读回用 `hdesk read_edit`。
不需要麦克风、不需要合成按键、用户看不见。

⚠️ 单实例:引擎带 `SAINPUT_SINGLE_NS` 跑,只认自己的锁与自己的进程名 ——
**绝不会**去接管(更不会杀掉)用户正在用的那个实例。

跑法:
    cargo build --release --target-dir target-test -p sainput-cli -p sainput-tsf-host
    python tools/test_race_guards.py
"""
import os
import pathlib
import sys
import time
import wave

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hdesk  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parent.parent
DIRS = ["target-test/x86_64-pc-windows-gnu/release", "target-test/release",
        "target/x86_64-pc-windows-gnu/release", "target/release"]
DESKTOP = "SAI-RaceGuards"
NS = "sainput-racetest"
ENV_KEYS = ("SAINPUT_SINGLE_NS", "SAINPUT_DIAG_AUTOSTART", "FAKE_ASR_SCRIPT",
            "SAINPUT_DIAG_TARGET_HWND")


def find(rel, what):
    for d in DIRS:
        p = REPO / d / rel
        if p.exists():
            return p
    print(f"FAIL: {what} not built")
    sys.exit(2)


def make_wav(path):
    """400ms 静音(16k 单声道 16bit):引擎要真音频才肯走采集那条路,内容无所谓。"""
    if path.exists():
        return path
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(16000)
        w.writeframes(b"\x00\x00" * 6400)
    return path


def run_case(host, sainput, wav, script, expect, settle=7.0):
    """起宿主(靶子)→ 起引擎(假侧车脚本)→ 读回断言 → 收摊。"""
    h = hdesk.open_desktop(DESKTOP)
    saved = {k: os.environ.get(k) for k in ENV_KEYS}
    hh = hp = None
    hpid = epid = 0
    try:
        # 环境变量由父进程继承 —— CreateProcess 默认就把整份环境传下去
        os.environ["SAINPUT_SINGLE_NS"] = NS
        os.environ["SAINPUT_DIAG_AUTOSTART"] = "1200"  # 进程内部触发热键,不合成按键
        os.environ["FAKE_ASR_SCRIPT"] = script

        hh, ht, hpid = hdesk.launch(
            str(host), f"--life 40000 --log {REPO / 'tmp' / 'race-host.log'}", DESKTOP)
        edit = None
        t0 = time.time()
        while time.time() - t0 < 10:
            edit = hdesk.find_edit(hpid, h)
            if edit:
                break
            time.sleep(0.2)
        if not edit:
            return False, "宿主窗口没起来"

        # 私有桌面上没有前台窗口,引擎得由我们把目标窗口直接给它
        os.environ["SAINPUT_DIAG_TARGET_HWND"] = hex(edit)
        log = REPO / "tmp" / "race-engine.log"
        if log.exists():
            log.unlink()
        hp, hpt, epid = hdesk.launch(
            str(sainput),
            "run --script " + str(REPO / "tools" / "fake_asr_sidecar.py")
            + f' --python "{sys.executable}"'
            + f" --feed {wav} --feed-restart"
            # 私有桌面上 **SendInput 根本进不去**(实测 0/6 事件被丢)——
            # 这正好也是我们做消息通道的原因。测试里强制 WM_CHAR:
            # 目标是个经典 EDIT,和记事本同一类,走的就是生产里那条消息通道。
            + " --ime-wmchar on"
            + " --no-leading-space --no-trailing-space --overlay-mode off"
            + f" --log {log}",
            DESKTOP)
        time.sleep(settle)
        got = hdesk.read_edit(edit)
        ok = got == expect
        detail = f"text={got!r} expect={expect!r}"
        if not ok and log.exists():
            tail = log.read_text(encoding="utf-8", errors="replace").splitlines()[-10:]
            detail += "\n    引擎日志尾部:\n      " + "\n      ".join(tail)
        hdesk.close(hpt)
        return ok, detail
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
    wav = make_wav(tmp / "race-silence.wav")

    cases = [
        # 定稿之后来了一条迟到的 partial:它一个字都不许出现
        ("final 之后迟到的 streaming 被丢弃(T4.1/T4.2)",
         "w:400;p:你好;w:200;p:你好世界;w:200;f:你好世界:finish;w:600;p:迟到的一帧;w:400",
         "你好世界"),
        # 同一句被定稿两次:屏幕上只许有一份
        ("重复定稿不打两遍(T4.1)",
         "w:400;p:一句话;w:200;f:一句话:finish;w:500;f:一句话:finish;w:400",
         "一句话"),
    ]
    fails = []
    for name, script, expect in cases:
        ok, detail = run_case(host, sainput, wav, script, expect)
        print(f"[{'PASS' if ok else 'FAIL'}] {name}: {detail}")
        if not ok:
            fails.append(name)

    print("\n" + ("ALL PASS" if not fails else f"FAILED: {', '.join(fails)}"))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
