# -*- coding: utf-8 -*-
"""T2.4 回归:TSF 通道坏掉时**自动退一档**,而不是把整句丢掉。

判据:
  1. 引擎日志里明确记下「TSF 通道不可用」;
  2. 这一句**照样上屏**(组合态下 `injected` 一直是空的,所以退档重来就是整句一次落地,
     一个字都不该丢);
  3. 之后的句子继续走退档后的通道(不再一句一句去撞那个坏通道)。

怎么把"坏通道"造出来:薄壳支持 `SAINPUT_TSF_PROTOCOL_FAKE=<串>` **谎报协议版本** ——
引擎握手时对不上就退档。这是真实故障(引擎与薄壳版本不一致 / 装了个旧的)的忠实模拟,
不需要真去装两个版本。

跑法:
    cargo build --release --target-dir target-test -p sainput-cli -p sainput-tsf -p sainput-tsf-host
    python tools/test_tsf_fallback.py
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
DESKTOP = "SAI-TsfFallback"
NS = "sainput-fbtest"
ENV_KEYS = ("SAINPUT_SINGLE_NS", "SAINPUT_DIAG_AUTOSTART", "FAKE_ASR_SCRIPT",
            "SAINPUT_DIAG_TARGET_HWND", "SAINPUT_DIAG_FALLBACK_POLICY",
            "SAINPUT_TSF_PROTOCOL_FAKE")


def find(rel, what):
    override = os.environ.get("SAINPUT_TSF_DIR")
    if override and rel != "sainput.exe":
        p = pathlib.Path(override) / rel
        if p.exists():
            return p
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


def main():
    host = find("sainput-tsf-host.exe", "host")
    dll = find("sainput_tsf.dll", "sainput_tsf.dll")
    sainput = find("sainput.exe", "engine")
    tmp = REPO / "tmp"
    tmp.mkdir(exist_ok=True)
    wav = make_wav(tmp / "race-silence.wav")
    hlog = tmp / "fb-host.log"
    if hlog.exists():
        hlog.unlink()
    logs_dir = REPO / "logs"
    logs_dir.mkdir(exist_ok=True)
    before = {p.name for p in logs_dir.glob("sainput-*.log")}

    saved = {k: os.environ.get(k) for k in ENV_KEYS}
    h = hdesk.open_desktop(DESKTOP)
    hp = ep = None
    hpid = epid = 0
    fails = []

    def check(name, ok, detail=""):
        print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
        if not ok:
            fails.append(name)

    try:
        # 薄壳谎报版本 -> 引擎握手一定失败
        os.environ["SAINPUT_TSF_PROTOCOL_FAKE"] = "sainput-tsf/OLD"
        hp, ht, hpid = hdesk.launch(
            str(host), f"--life 26000 --tip-hold 20000 --log {hlog} --load-tip {dll}", DESKTOP)
        edit = None
        t0 = time.time()
        while time.time() - t0 < 10:
            edit = hdesk.find_edit(hpid, h)
            if edit:
                break
            time.sleep(0.2)
        check("宿主窗口起来了", edit is not None)
        if not edit:
            return 1

        os.environ["SAINPUT_SINGLE_NS"] = NS
        os.environ["SAINPUT_DIAG_AUTOSTART"] = "1200"
        # 只跑一句:T2.4 要证的是"坏通道上这一句照样上屏",句子边界的时序是另一码事
        # (那由 `tools/race`/wav 那几条覆盖)。
        os.environ["FAKE_ASR_SCRIPT"] = (
            "w:400;p:你好;w:200;p:你好世界;w:200;f:你好世界:finish;w:400"
        )
        os.environ["SAINPUT_DIAG_TARGET_HWND"] = hex(edit)
        # 私有桌面上 SendInput 进不去(0/6 事件被丢),所以把退档策略钉在消息通道上
        os.environ["SAINPUT_DIAG_FALLBACK_POLICY"] = "on"

        hp2, hpt, epid = hdesk.launch(
            str(sainput),
            "run --script " + str(REPO / "tools" / "fake_asr_sidecar.py")
            + f' --python "{sys.executable}"'
            + f" --feed {wav} --feed-restart --ime-wmchar tsf"
            + " --no-leading-space --no-trailing-space --overlay-mode off",
            DESKTOP)
        ep = hp2
        # 轮询读回(激活窗口 + 占位时间,固定 sleep 会读出假时序)
        got = ""
        t0 = time.time()
        while time.time() - t0 < 25:
            got = hdesk.read_edit(edit)
            if got == "你好世界":
                break
            time.sleep(0.3)
        new_logs = sorted(set(p.name for p in logs_dir.glob("sainput-*.log")) - before)
        log = ""
        if new_logs:
            log = (logs_dir / new_logs[-1]).read_text(encoding="utf-8", errors="replace")

        check("引擎记下了「TSF 通道不可用」", "TSF 通道不可用" in log,
              "" if "TSF 通道不可用" in log else log[-400:])
        check("坏通道上这一句照样上屏(没丢字)", got == "你好世界", f"text={got!r}")
        check("之后不再逐句重试坏通道(退档是粘的)",
              log.count("TSF 通道不可用") == 1, f"出现 {log.count('TSF 通道不可用')} 次")
        hdesk.close(hpt)
    finally:
        if ep:
            hdesk.terminate(epid)
            hdesk.close(ep)
        if hp:
            hdesk.terminate(hpid)
            hdesk.close(hp)
        hdesk.close(h)
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    print("\n" + ("ALL PASS" if not fails else f"FAILED: {', '.join(fails)}"))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
