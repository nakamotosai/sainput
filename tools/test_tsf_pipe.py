# -*- coding: utf-8 -*-
"""T1.5 回归:引擎 ↔ 薄壳的命名管道(帧协议、状态机、生命周期)。

判据(`PLAN-NEXT.md` T1.5,管道这一半):
  1. 端点名按 pid 算:`\\\\.\\pipe\\sainput-tsf-<pid>`,激活期间可连、反激活后消失;
  2. 帧协议:4 字节小端长度 + 负载;`P/S/C/X/Q` 各有明确应答,未知命令回 `E`;
  3. 状态机:`S` 记住组合态、`C` 累加已定稿字节并清空组合态、`X` 清空;
  4. 一个客户端断开后,**下一个还能连**(服务端循环不崩、不留脏状态)。

为什么用管道:薄壳跑在目标应用进程里(唯一能碰 TSF 上下文的地方),文本在引擎进程里。
管道跨进程、不跨桌面、不受 UIPI 限制。

跑法:
    cargo build --release --target-dir target-test -p sainput-tsf -p sainput-tsf-host
    python tools/test_tsf_pipe.py
"""
import pathlib
import struct
import subprocess
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hdesk  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parent.parent
DIRS = ["target-test/x86_64-pc-windows-gnu/release", "target-test/release",
        "target/x86_64-pc-windows-gnu/release", "target/release"]
DESKTOP = "SAI-TsfPipe"
VERSION = b"sainput-tsf/1"


def find(rel, what):
    for d in DIRS:
        p = REPO / d / rel
        if p.exists():
            return p
    print(f"FAIL: {what} not built")
    sys.exit(2)


def frame(payload: bytes) -> bytes:
    return struct.pack("<I", len(payload)) + payload


def read_exact(f, n):
    buf = b""
    while len(buf) < n:
        chunk = f.read(n - len(buf))
        if not chunk:
            raise EOFError("pipe closed")
        buf += chunk
    return buf


def recv(f) -> bytes:
    n = struct.unpack("<I", read_exact(f, 4))[0]
    return read_exact(f, n)


def connect(name, timeout=8.0):
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            return open(name, "r+b", buffering=0)
        except (FileNotFoundError, PermissionError, OSError):
            time.sleep(0.1)
    return None


def exchange(f, payload: bytes) -> bytes:
    f.write(frame(payload))
    return recv(f)


def main():
    host = find("sainput-tsf-host.exe", "host")
    dll = find("sainput_tsf.dll", "sainput_tsf.dll")
    tmp = REPO / "tmp"
    tmp.mkdir(exist_ok=True)
    hlog = tmp / "pipe-host.log"
    if hlog.exists():
        hlog.unlink()

    h = hdesk.open_desktop(DESKTOP)
    hp = ht = None
    fails = []

    def check(name, ok, detail=""):
        print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
        if not ok:
            fails.append(name)

    try:
        # 保持激活 6 秒:管道只在这段窗口里可连
        hp, ht, pid = hdesk.launch(
            str(host), f"--life 24000 --tip-hold 20000 --log {hlog} --load-tip {dll}", DESKTOP)
        name = rf"\\.\pipe\sainput-tsf-{pid}"
        print(f"host pid={pid} pipe={name}")
        f = connect(name)
        check("激活期间管道可连", f is not None)
        if f:
            r = exchange(f, b"P" + VERSION)
            check("P ping 应答版本", r[:1] == b"O" and VERSION in r, r.decode("utf-8", "replace"))

            r = exchange(f, b"Q")
            check("Q 初始状态为空", r == b"O|0", repr(r))

            cn = "你好,组合态"
            r = exchange(f, b"S" + cn.encode())
            check("S 记住组合态", r == b"O" + cn.encode(), repr(r))

            r = exchange(f, b"Q")
            check("Q 能读回组合态", r == b"O" + cn.encode() + b"|0", repr(r))

            r = exchange(f, b"C" + cn.encode())
            check("C 累加已定稿字节", r == b"O" + str(len(cn.encode())).encode(), repr(r))

            r = exchange(f, b"Q")
            check("C 之后组合态被清空", r == b"O|" + str(len(cn.encode())).encode(), repr(r))

            r = exchange(f, b"X")
            check("X 取消", r == b"O", repr(r))

            # 看门狗:组合态闲置超时自动取消 —— 不让半句话留在别人的文档里
            r = exchange(f, b"S" + "会被丢弃的组合态".encode())
            check("S 再设一次组合态", r[:1] == b"O", repr(r))
            time.sleep(6.5)
            r = exchange(f, b"Q")
            check("闲置 5s 后组合态被自动取消", r == b"O|" + str(len(cn.encode())).encode(), repr(r))

            r = exchange(f, b"Z")
            check("未知命令回 E", r[:1] == b"E", repr(r))

            r = exchange(f, b"P" + b"wrong-version")
            check("版本不匹配回 E", r[:1] == b"E", repr(r))
            f.close()

            # 同一个服务端,第二个客户端还能连(循环不崩)
            f2 = connect(name, timeout=4.0)
            check("断开后下一个客户端仍可连", f2 is not None)
            if f2:
                r = exchange(f2, b"P" + VERSION)
                check("第二个客户端 ping 正常", r[:1] == b"O", repr(r))
                f2.close()

        # ---- 引擎侧:真的让 `sainput` 从**另一张桌面**发一句过来 ----
        # 命名管道是内核对象,跨桌面没有障碍 —— 这条就是"引擎能打到目标进程"的证据。
        sainput = find("sainput.exe", "engine")
        edit = hdesk.find_edit(pid, h)
        check("宿主有文本控件句柄", edit is not None)
        if edit:
            f3 = connect(name, timeout=4.0)
            before = exchange(f3, b"Q") if f3 else b""
            if f3:
                f3.close()
            r = subprocess.run(
                # `--pid` 是必须的:窗口在别的桌面上,引擎那边
                # `GetWindowThreadProcessId` 推不出 pid(句柄是桌面作用域的)。
                # 生产路径不需要它 —— 引擎和焦点窗口永远在同一张桌面上。
                [str(sainput), "selftest", "compose", "引擎经管道发来的一句", "--mode", "tsf",
                 "--hwnd", hex(edit), "--pid", str(pid), "--frames", "3"],
                capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=60)
            check("引擎(TSF 通道)退出码 0", r.returncode == 0, (r.stderr or "").strip()[:200])
            check("引擎日志显示走的是 TSF 组合态", "TSF 组合态" in (r.stdout or ""), (r.stdout or "").strip()[:200])
            f4 = connect(name, timeout=4.0)
            if f4:
                after = exchange(f4, b"Q")
                f4.close()
                # 引擎那一句是 3 帧 + 定稿一次:已定稿字节数必须增加
                grew = after != before
                check("管道状态记录了引擎的定稿", grew, f"{before!r} -> {after!r}")
            else:
                check("引擎之后管道仍可连", False)

        code = hdesk.wait_exit(hp, 30000)
        check("宿主正常退出", code == 0, f"exit={code}")
        time.sleep(0.3)
        late = connect(name, timeout=1.5)
        check("反激活/退出后端点消失", late is None)
        if late:
            late.close()
    finally:
        if hp:
            hdesk.close(hp)
        if ht:
            hdesk.close(ht)
        hdesk.close(h)

    if fails and hlog.exists():
        print("\n--- host log ---\n" + hlog.read_text(encoding="utf-8", errors="replace"))
    print("\n" + ("ALL PASS" if not fails else f"FAILED: {', '.join(fails)}"))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
