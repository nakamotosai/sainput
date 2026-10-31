# -*- coding: utf-8 -*-
"""T2.4 后半段回归:注册/注销脚本的**闸门与自述**是否正确(不需要 UAC,不改系统)。

背景:系统里曾被注册进去一条指向 `target-test` 的 TIP(带语言 profile),于是
输入法列表里多一条"选中就崩"的项,而且没人知道。要防的是两类事:
  1. **注册到开发产物** —— 必须被拒(除非显式 `-AllowDev`),且**一个字节都不许改**;
  2. **状态不可自述** —— `verify` 必须给出结论(STALE/NOT-REGISTERED/OK)+ 退出码,
     这样"装完到底指向哪份 DLL"是可机检的,而不是靠人肉翻注册表。

判据(全部可机检、在任何机器上都确定):
  A. `verify` 的**退出码与结论一致**(0=OK / 1=NOT-REGISTERED / 4=STALE);
  B. `verify -ExpectDll <别的路径>` 绝不报 OK;
  C. `register -Dll <开发产物>` → 退出 2 + 明说拒绝,且注册表状态**前后一致**;
  D. `register -Dll <不存在的文件>` → 退出 2;
  E. `register -Dll <发布版> -Apply` 在非管理员下 → 退出 3(需要一次提权),且不改动;
  F. 脚本必须带 UTF-8 BOM(PS 5.1 读无 BOM 的中文注释会吞掉下一行代码)。

跑法:python tools/test_tsf_register.py
"""
import pathlib
import re
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
SCRIPT = REPO / "tools" / "tsf_register.ps1"
DEV_DLL = REPO / "target-test" / "x86_64-pc-windows-gnu" / "release" / "sainput_tsf.dll"
REL_DLL = REPO / "target" / "x86_64-pc-windows-gnu" / "release" / "sainput_tsf.dll"


def run(*args):
    p = subprocess.run(
        ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(SCRIPT), *args],
        capture_output=True, text=True, encoding="utf-8", errors="replace",
    )
    return p.returncode, (p.stdout or "") + (p.stderr or "")


def verdict_of(out):
    m = re.search(r"^verdict: (.+)$", out, re.M)
    return m.group(1).strip() if m else None


def state_lines(out):
    """注册表状态那几行(用来证明'被拒时什么都没改')。"""
    keep = []
    for ln in out.splitlines():
        if ln.startswith(("state:", "  HKLM", "  dll", "  version", "  registered-at", "  profile")):
            keep.append(ln.strip())
    return keep


def main():
    fails = []

    def check(name, ok, detail=""):
        print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f"  {detail}" if detail else ""))
        if not ok:
            fails.append(name)

    # F. BOM 必须先查(否则后面所有中文注释都可能静默吞行)
    check("脚本带 UTF-8 BOM", SCRIPT.read_bytes()[:3] == b"\xef\xbb\xbf")

    # A. verify 的结论与退出码一致
    rc, out = run("-Action", "verify")
    v = verdict_of(out)
    if v is None:
        check("verify 给出结论", False, f"没解析到 verdict 行;退出码 {rc}")
    else:
        want_rc = 0 if v == "OK" else (4 if v.startswith("STALE") else 1)
        check("verify 结论与退出码一致", rc == want_rc, f"verdict={v} 退出码={rc}(期望 {want_rc})")

    # B. 拿别的路径当期望 → 绝不报 OK
    rc2, out2 = run("-Action", "verify", "-ExpectDll", str(REPO / "不存在的" / "sainput_tsf.dll"))
    v2 = verdict_of(out2)
    check("期望路径不符时不报 OK", v2 is not None and v2 != "OK", f"verdict={v2}")

    # C. 开发产物必须被拒,且不改注册表
    before = state_lines(run("-Action", "verify")[1])
    rc3, out3 = run("-Action", "register", "-Dll", str(DEV_DLL), "-Apply")
    after = state_lines(run("-Action", "verify")[1])
    # 判据用 ASCII 那行(PS 5.1 的输出是控制台代码页,中文会被读成乱码)
    refused = "refusing to register a dev build" in out3
    check("拒绝注册开发产物", rc3 == 2 and refused, f"退出码 {rc3} 有拒绝行={refused}")
    # 正斜杠也必须拦住(只认反斜杠 = 闸门形同虚设)
    dev_fwd = str(DEV_DLL).replace("\\", "/")
    rc3b, out3b = run("-Action", "register", "-Dll", dev_fwd, "-Apply")
    check("正斜杠写的开发产物也拦住", rc3b == 2 and "refusing to register a dev build" in out3b,
          f"退出码 {rc3b}")
    check("两次被拒后注册表仍一字未改", state_lines(run("-Action", "verify")[1]) == before)
    check("被拒时注册表一字未改", before == after)

    # D. 不存在的 DLL
    rc4, _ = run("-Action", "register", "-Dll", str(REPO / "nope" / "x.dll"), "-Apply")
    check("不存在的 DLL → 退出 2", rc4 == 2, f"退出码 {rc4}")

    # E. 非管理员 + -Apply → 退出 3,且不改动
    if REL_DLL.exists():
        rc5, out5 = run("-Action", "register", "-Dll", str(REL_DLL), "-Apply")
        elevated = rc5 == 0  # 管理员跑就会真的装,这时只要求它不再报"需要提权"
        check("提权闸门(非管理员=3 / 已提权=0)", rc5 in (0, 3), f"退出码 {rc5}")
        after2 = state_lines(run("-Action", "verify")[1])
        if not elevated:
            check("需要提权时不改动", before == after2)
    else:
        print("[SKIP] 发布版 DLL 还没构建,跳过提权闸门检查")

    print("\n" + ("ALL PASS" if not fails else f"FAILED: {', '.join(fails)}"))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
