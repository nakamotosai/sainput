# -*- coding: utf-8 -*-
"""T0.2 回归:宿主壳在私有桌面上活着、内容一字不差、而且用户看不见。

三件事一次验完(判据来自 `PLAN-NEXT.md` T0.2):
  1. 壳能建窗口 + 经典 EDIT,并拿到焦点;
  2. `WM_CHAR` 往返一字不差 —— 含非 ASCII(中文走 UTF-8 文件喂进去,绕开命令行编码);
  3. 窗口只在**私有桌面**上,console 桌面枚举该 pid = 0(用户看不见、抢不到焦点)。

跑法:
    python tools/test_tsf_host.py
退出码 0 = 全过;非 0 = 哪一条没过看输出。
"""
import pathlib
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
CANDIDATES = [
    REPO / "target-test/x86_64-pc-windows-gnu/release/sainput-tsf-host.exe",
    REPO / "target-test/release/sainput-tsf-host.exe",
    REPO / "target/x86_64-pc-windows-msvc/release/sainput-tsf-host.exe",
]
CN = "你好abc123,这是宿主壳自检。"
ASCII = "helloTOKEN123"


def find_exe():
    for c in CANDIDATES:
        if c.exists():
            return c
    print("FAIL: sainput-tsf-host.exe not built; run: cargo build --release -p sainput-tsf-host")
    sys.exit(2)


def run_prove(exe, args, expect=None, dwell=9000):
    cmd = [sys.executable, str(REPO / "tools/hdesk.py"), "prove",
           "--exe", str(exe), "--args", args, "--dwell", str(dwell)]
    if expect is not None:
        cmd += ["--expect-text", expect]
    r = subprocess.run(cmd, cwd=str(REPO), capture_output=True, text=True,
                       encoding="utf-8", errors="replace")
    return r


def main():
    exe = find_exe()
    tmp = REPO / "tmp" / "test-tsf-host"
    tmp.mkdir(parents=True, exist_ok=True)
    fails = []

    # --- 1/2 ASCII 内联:同时验跨进程读回(WM_GETTEXT) ---
    log = tmp / "ascii.log"
    if log.exists():
        log.unlink()
    r = run_prove(exe, f"--life 3000 --log {log} --selftest {ASCII}", expect=ASCII)
    ok_ascii = r.returncode == 0 and log.exists() and "selftest PASS" in log.read_text(encoding="utf-8")
    print(f"[1] ascii round-trip + cross-process readback: {'PASS' if ok_ascii else 'FAIL'}")
    if not ok_ascii:
        fails.append("ascii")
        print(r.stdout[-1500:], r.stderr[-500:])

    # --- 3 中文走 UTF-8 文件(命令行编码不可信) ---
    payload = tmp / "payload.txt"
    payload.write_text(CN, encoding="utf-8")
    log_cn = tmp / "cn.log"
    if log_cn.exists():
        log_cn.unlink()
    r = run_prove(exe, f"--life 3000 --log {log_cn} --selftest-file {payload}", dwell=9000)
    txt = log_cn.read_text(encoding="utf-8") if log_cn.exists() else ""
    ok_cn = r.returncode == 0 and f'selftest PASS expected="{CN}" actual="{CN}"' in txt
    print(f"[2] non-ascii round-trip (utf-8 payload file): {'PASS' if ok_cn else 'FAIL'}")
    if not ok_cn:
        fails.append("cn")
        print(r.stdout[-1500:], r.stderr[-500:])
        print("host log:", txt)

    # --- 4 隔离:两轮都要满足 console 桌面 0 个窗口(prove 自己会断言) ---
    isolation = "console_windows=0" in r.stdout and "PASS -- windows live on the private desktop" in r.stdout
    print(f"[3] invisible to the user (private desktop only): {'PASS' if isolation else 'FAIL'}")
    if not isolation:
        fails.append("isolation")

    print("\n" + ("ALL PASS" if not fails else f"FAILED: {', '.join(fails)}"))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
