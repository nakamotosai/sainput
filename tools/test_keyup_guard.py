# -*- coding: utf-8 -*-
"""T6.7 回归:进程外看门狗 —— **硬崩之后按键不残留**。

为什么必须这么测:这条功能存在的唯一意义就是"进程没机会收尾"那一刻。任何
"看起来没问题"的检查都盖不到它 —— 必须真的把一个装了钩子的引擎**硬杀掉**
(TerminateProcess,不给它任何清理机会),再看外面那只看门狗有没有补发抬起。

判据(都可机检):
  A. 硬杀(钩子活着时被杀)→ 看门狗日志出现 `VIOLENT-EXIT`;
  B. 父进程**从没装过钩子**(拿 `sainput config` 当父进程)→ 日志**绝不允许**出现
     `VIOLENT-EXIT`(这条防的是"看门狗变成乱发按键的东西");
  C. 两个命名空间互不串(测试用 `SAINPUT_SINGLE_NS` 隔离,不碰你日常那份)。

⚠️ 引擎跑在**私有桌面** + 独立命名空间,`--dry-run` 不打字;看门狗日志写在 `logs/guard-<pid>.log`。
"""
import os
import pathlib
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hdesk  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parent.parent
BIN = REPO / "target-test/x86_64-pc-windows-gnu/release"
if not (BIN / "sainput.exe").exists():
    BIN = REPO / "target/x86_64-pc-windows-gnu/release"
SAINPUT = BIN / "sainput.exe"
HOST = BIN / "sainput-tsf-host.exe"
DESKTOP = "SAI-KeyGuard"
ENV_KEYS = ("SAINPUT_SINGLE_NS", "SAINPUT_DIAG_AUTOSTART", "SAINPUT_DIAG_TARGET_HWND")


GUARD_LOG = REPO / "logs" / "guard.log"


def guard_lines():
    """看门狗日志(单文件,每行带 `parent=<pid>`)。"""
    if not GUARD_LOG.exists():
        return []
    return GUARD_LOG.read_text(encoding="utf-8", errors="replace").splitlines()


def main():
    if not SAINPUT.exists():
        print(f"FAIL: 没构建 {SAINPUT}")
        return 2
    fails = []
    d = hdesk.open_desktop(DESKTOP)
    saved = {k: os.environ.get(k) for k in ENV_KEYS}
    hh = hp = None
    hpid = epid = 0
    try:
        os.environ["SAINPUT_SINGLE_NS"] = "sainput-keyguard"
        os.environ["SAINPUT_DIAG_AUTOSTART"] = "60000"  # 一直"按着",引擎处于活着的钩子状态
        hh, _t, hpid = hdesk.launch(str(HOST), f"--life 60000 --log {REPO}/tmp/guard-host.log", DESKTOP)
        edit = None
        t0 = time.time()
        while time.time() - t0 < 10:
            edit = hdesk.find_edit(hpid, d)
            if edit:
                break
            time.sleep(0.2)
        os.environ["SAINPUT_DIAG_TARGET_HWND"] = hex(edit)
        hp, _pt, epid = hdesk.launch(
            str(SAINPUT), "run --hotkey rightctrl --no-tray --no-overlay --dry-run", DESKTOP)
        # ⚠️ 看门狗是**父进程退出之后**才写日志的(它整个存在意义就在那一刻),
        # 所以这里只能等引擎自己就绪,不能等日志 —— 先去等日志就是鸡生蛋。
        time.sleep(10)
        hp_before = [ln for ln in guard_lines() if ln.startswith(f"parent={epid} ")]
        # A. 硬杀(不给任何清理机会)
        hdesk.terminate(epid)
        hdesk.close(hp)
        hp = None
        ok_a = False
        txt = ""
        t0 = time.time()
        while time.time() - t0 < 25:
            mine = [ln for ln in guard_lines() if ln.startswith(f"parent={epid} ")]
            if "VIOLENT-EXIT" in "\n".join(mine):
                ok_a = True
                txt = "\n".join(mine)
                break
            time.sleep(0.5)
        if not ok_a:
            mine = [ln for ln in guard_lines() if ln.startswith(f"parent={epid} ")]
            txt = "\n".join(mine) if mine else "(这个 pid 一行都没有 —— 看门狗没起来或没写日志)"
        print(f"[{'PASS' if ok_a else 'FAIL'}] 硬崩后补发抬起  日志={txt.strip()[:100]!r}")
        if not ok_a:
            fails.append("硬崩补发")

        # B. 父进程从没装过钩子 → 绝不允许补发
        victim = hdesk.launch(str(SAINPUT), "config", DESKTOP)
        vpid = victim[2]
        time.sleep(1.5)
        g = hdesk.launch(
            str(SAINPUT),
            f'guard --parent {vpid} --log "{GUARD_LOG}"',
            DESKTOP,
        )
        time.sleep(4)
        hdesk.terminate(vpid)
        time.sleep(5)
        hdesk.terminate(g[2])
        hdesk.close(g[0])
        hdesk.close(victim[0])
        mine2 = [ln for ln in guard_lines() if ln.startswith(f"parent={vpid} ")]
        txt2 = "\n".join(mine2)
        clean = bool(mine2) and "VIOLENT-EXIT" not in txt2
        print(f"[{'PASS' if clean else 'FAIL'}] 无状态时不乱发  日志={txt2.strip()[:80]!r}")
        if not clean:
            fails.append("无状态误补发")
    finally:
        if hp:
            hdesk.terminate(epid)
            hdesk.close(hp)
        if hh:
            hdesk.close(hh)
        hdesk.close(d)
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    print("\n" + ("ALL PASS" if not fails else f"FAILED: {', '.join(fails)}"))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
