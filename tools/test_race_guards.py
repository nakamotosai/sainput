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


def engine_log_dir(tag):
    """引擎的日志开关是 **`--log-dir <目录>`**(不是 `--log <文件>`)—— 传错了它一声不吭地不写。

    2026-10-31 实测踩到:用例里读"引擎日志尾部"的诊断一直读到空(那些断言全靠读回文本,
    所以没人发现)。现在统一用一个独立目录,既拿得到日志,也不跟用户自己那份混在一起。
    """
    d = REPO / "tmp" / f"engine-{tag}"
    d.mkdir(parents=True, exist_ok=True)
    for f in d.glob("sainput-*.log"):
        f.unlink()
    return d


def read_engine_log(d):
    """目录里最新的那份会话日志(引擎自己按时间戳命名)。"""
    files = sorted(d.glob("sainput-*.log"))
    if not files:
        return ""
    return files[-1].read_text(encoding="utf-8", errors="replace")


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
        logdir = engine_log_dir("race")
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
            + f" --log-dir {logdir}",
            DESKTOP)
        time.sleep(settle)
        got = hdesk.read_edit(edit)
        ok = got == expect
        detail = f"text={got!r} expect={expect!r}"
        if not ok:
            tail = read_engine_log(logdir).splitlines()[-10:]
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


def run_focus_case(host, sainput, wav, guard, expect_in_second):
    """T4.3:按住热键说话**中途切窗口**,半句不许落到新窗口。

    为什么这条必须跑在**真桌面**:守卫复核的是 `GetForegroundWindow()`,而私有桌面上
    前台恒为 0(量不出变化)。两个宿主窗口会短暂出现在屏幕上,几十秒自己退。

    `guard=False` 是**对照组**:关掉守卫时必须能复现"后半句打进新窗口"——
    否则这条测试就是"永远绿的",证明不了任何事。
    """
    hh = ht = hh2 = ht2 = hp = hpt = None
    hpid = hpid2 = epid = 0
    try:
        os.environ["SAINPUT_SINGLE_NS"] = NS
        os.environ["SAINPUT_DIAG_AUTOSTART"] = "1500"
        os.environ.pop("SAINPUT_DIAG_TARGET_HWND", None)
        hh, ht, hpid = hdesk.launch(
            str(host), f"--life 30000 --log {REPO / 'tmp' / 'focus-hostA.log'}", "WinSta0\\Default")
        editA = None
        t0 = time.time()
        while time.time() - t0 < 10:
            editA = hdesk.find_edit(hpid)
            if editA:
                break
            time.sleep(0.2)
        if not editA:
            return False, "宿主 A 没起来"
        # 引擎:A 是前台 → 按下时记下的就是 A
        logdir = engine_log_dir("focus")
        args = ("run --script " + str(REPO / "tools" / "fake_asr_sidecar.py")
                + f' --python "{sys.executable}"'
                + f" --feed {wav} --feed-restart"
                + " --ime-wmchar on --no-leading-space --no-trailing-space --overlay-mode off"
                + (" --focus-guard" if guard else "")
                + f" --log-dir {logdir}")
        hp, hpt, epid = hdesk.launch(str(sainput), args, "WinSta0\\Default")
        # **必须等"这一句真的开始了"再切窗口**:引擎从启动到第一次按下约 1.0~1.3s(实测),
        # 早了会让 B 在按下之前就成了前台 —— 那时守卫记下的本来就是 B,用例没意义
        # (2026-10-31 踩过:日志里"本句目标"和前台是同一个窗口)。
        # 这里**不能**轮询日志里的标记:引擎的 `--log` 是缓冲写,落盘要等到收摊(实测),
        # 所以用实测出来的启动时长,把强断言放在后面(守卫开:必须看到那条"前台窗口变了")。
        time.sleep(1.6)
        hh2, ht2, hpid2 = hdesk.launch(
            str(host), f"--life 12000 --log {REPO / 'tmp' / 'focus-hostB.log'}", "WinSta0\\Default")
        editB = None
        t0 = time.time()
        while time.time() - t0 < 6:
            editB = hdesk.find_edit(hpid2)
            if editB:
                break
            time.sleep(0.2)
        time.sleep(9.0)
        a = hdesk.read_edit(editA) if editA else ""
        b = hdesk.read_edit(editB) if editB else "<B 没起来>"
        txt = read_engine_log(logdir)
        moved = "说话期间前台窗口变了" in txt
        dropped = "本句未上屏" in txt
        if guard:
            # 判据(全部机械可检):
            #  1) 引擎自己**说了**它发现前台变了并中止本句;
            #  2) 它**说了**这一句没上屏;
            #  3) 靶子 A 里一个字都没有(半句不落原窗口,更不落新窗口)。
            # 不再断言 B 为空:切窗之后的**下一句**以 B 为目标,落进 B 是正确行为
            #(守卫管的是"这一句",不是"从此不许打 B")。
            # 「本句未上屏」那行只在**定稿时**才打印:切窗可能发生在定稿之后(那时它只中止
            # 注入、不再打印那句),所以它只作为信息,不作判据。
            ok = moved and a == ""
            detail = f"守卫开: A={a!r} B={b!r} 日志:变了={moved} 未上屏={dropped}"
        else:
            # 对照组:守卫关掉时**不许**出现那两行 —— 否则这条用例永远是绿的,证明不了任何事。
            ok = (not moved) and (not dropped) and a != ""
            detail = f"对照组(守卫关): A={a!r} B={b!r} 日志:变了={moved} 未上屏={dropped}"
        return ok, detail
    finally:
        # 与 run_case 同风格:terminate 收 pid,close 收句柄
        for pid_, thr in ((epid, hpt), (hpid2, ht2), (hpid, ht)):
            if pid_:
                hdesk.terminate(pid_)
            if thr:
                hdesk.close(thr)
        for k in ("SAINPUT_SINGLE_NS", "SAINPUT_DIAG_AUTOSTART", "SAINPUT_DIAG_TARGET_HWND"):
            os.environ.pop(k, None)


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
    # T4.3:中途切窗口 —— 守卫开时必须拦住,关时必须能复现(对照组)
    for guard, name in ((True, "焦点变更中止本句(T4.3,守卫开)"),
                        (False, "对照组:守卫关时会打进新窗口")):
        ok, detail = run_focus_case(host, sainput, wav, guard, expect_in_second=True)
        print(f"[{'PASS' if ok else 'FAIL'}] {name}: {detail}")
        if not ok:
            cases.append((name, None, None))

    fails = []
    for name, script, expect in cases:
        if script is None:
            fails.append(name)
            continue
        ok, detail = run_case(host, sainput, wav, script, expect)
        print(f"[{'PASS' if ok else 'FAIL'}] {name}: {detail}")
        if not ok:
            fails.append(name)

    print("\n" + ("ALL PASS" if not fails else f"FAILED: {', '.join(fails)}"))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
