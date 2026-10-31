# -*- coding: utf-8 -*-
"""T1.5 + T2.3 回归:引擎 ↔ 薄壳的命名管道(帧协议、状态机、生命周期、幂等与乱序)。

判据:
  T1.5 ——
    1. 端点名按 pid 算:`\\\\.\\pipe\\sainput-tsf-<pid>`,激活期间可连、反激活后消失;
    2. 帧协议:4 字节小端长度 + 负载;`P/S/C/X/Q` 各有明确应答,未知命令回 `E`;
    3. 状态机:`S` 记住组合态、`C` 累加已定稿字节并清空组合态、`X` 清空;
    4. 一个客户端断开后,下一个还能连;断开后重连不残留脏状态。
  T2.3 —— 句子序号是幂等与乱序的全部依据:
    5. **重复定稿**同一句:只算一次(插两次就是可见的重字);
    6. **迟到帧**:序号 ≤ 已定稿到 的 `S`/`C` 一律丢弃,不许盖回来;
    7. 新句(序号更大)照常工作。
  T2.2 ——
    8. 组合态闲置 5s 由看门狗自动取消(不留半句在别人文档里)。

换一套位数组合来跑(32 位薄壳 + 32 位宿主):
    cargo build --release --target i686-pc-windows-msvc --target-dir target-test-msvc -p sainput-tsf -p sainput-tsf-host
    SAINPUT_TSF_DIR=target-test-msvc/i686-pc-windows-msvc/release python tools/test_tsf_pipe.py

跑法:
    cargo build --release --target-dir target-test -p sainput-tsf -p sainput-tsf-host
    python tools/test_tsf_pipe.py
"""
import os
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
CN = "你好,组合态"


def find(rel, what):
    # `SAINPUT_TSF_DIR` = 换一套"薄壳 + 宿主"来跑(例如 32 位那套)。
    # 引擎不跟着换 —— 它本来就是另一个进程。
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


def q(f) -> str:
    """Q 的应答正文:`preedit|committed 字节|当前句|已定稿到`。"""
    r = exchange(f, b"Q")
    assert r[:1] == b"O", repr(r)
    return r[1:].decode("utf-8", "replace")


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

    def set_frame(sid, text):
        return b"S" + f"{sid} {text}".encode()

    def commit_frame(sid, text):
        return b"C" + f"{sid} {text}".encode()

    try:
        hp, ht, pid = hdesk.launch(
            str(host), f"--life 26000 --tip-hold 22000 --log {hlog} --load-tip {dll}", DESKTOP)
        name = rf"\\.\pipe\sainput-tsf-{pid}"
        print(f"host pid={pid} pipe={name}")
        f = connect(name)
        check("激活期间管道可连", f is not None)
        if f:
            r = exchange(f, b"P" + VERSION)
            check("P ping 应答版本", r[:1] == b"O" and VERSION in r, r.decode("utf-8", "replace"))
            check("Q 初始状态为空", q(f) == "|0|0|0", q(f))

            r = exchange(f, set_frame(1, CN))
            check("S 记住组合态", r == b"O" + CN.encode(), repr(r))
            check("Q 能读回组合态与当前句", q(f) == f"{CN}|0|1|0", q(f))

            n = len(CN.encode())
            r = exchange(f, commit_frame(1, CN))
            check("C 累加已定稿字节", r == b"O" + str(n).encode(), repr(r))
            check("C 之后组合态被清空", q(f) == f"|{n}|1|1", q(f))

            # ---- T2.3:幂等与乱序 ----
            r = exchange(f, commit_frame(1, CN))
            check("重复定稿同一句被丢弃(不插两次)", r == b"O" + str(n).encode() and q(f) == f"|{n}|1|1",
                  f"{r!r} / {q(f)}")
            r = exchange(f, commit_frame(0, "更早的句子"))
            check("序号更小的定稿被丢弃", q(f) == f"|{n}|1|1", q(f))
            r = exchange(f, set_frame(1, "旧句的迟到尾巴"))
            check("旧句迟到的 S 被丢弃(盖不回来)", q(f) == f"|{n}|1|1", q(f))
            r = exchange(f, set_frame(2, "新的一句"))
            check("新句照常工作", q(f) == f"新的一句|{n}|2|1", q(f))
            r = exchange(f, commit_frame(2, "新的一句"))
            n2 = n + len("新的一句".encode())
            check("新句定稿累加", q(f) == f"|{n2}|2|2", q(f))

            # ---- T2.2:看门狗 ----
            exchange(f, set_frame(3, "会被丢弃的组合态"))
            time.sleep(6.5)
            check("闲置 5s 后组合态被自动取消", q(f) == f"|{n2}|3|2", q(f))

            r = exchange(f, b"X")
            check("X 取消", r == b"O", repr(r))
            r = exchange(f, b"Z")
            check("未知命令回 E", r[:1] == b"E", repr(r))
            r = exchange(f, b"P" + b"wrong-version")
            check("版本不匹配回 E", r[:1] == b"E", repr(r))
            f.close()

            f2 = connect(name, timeout=4.0)
            check("断开后下一个客户端仍可连", f2 is not None)
            if f2:
                check("第二个客户端 ping 正常", exchange(f2, b"P" + VERSION)[:1] == b"O")
                f2.close()

        # ---- 引擎侧:真的让 `sainput` 从**另一张桌面**发一句过来 ----
        sainput = find("sainput.exe", "engine")
        edit = hdesk.find_edit(pid, h)
        check("宿主有文本控件句柄", edit is not None)
        if edit:
            f3 = connect(name, timeout=4.0)
            before = q(f3) if f3 else ""
            if f3:
                f3.close()
            r = subprocess.run(
                # `--pid` 是必须的:窗口在别的桌面上,引擎那边推不出 pid(句柄是桌面作用域的)。
                [str(sainput), "selftest", "compose", "引擎经管道发来的一句", "--mode", "tsf",
                 "--hwnd", hex(edit), "--pid", str(pid), "--frames", "3"],
                capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=60)
            check("引擎(TSF 通道)退出码 0", r.returncode == 0, (r.stderr or "").strip()[:200])
            check("引擎日志显示走的是 TSF 组合态", "TSF 组合态" in (r.stdout or ""),
                  (r.stdout or "").strip()[:200])
            f4 = connect(name, timeout=4.0)
            if f4:
                after = q(f4)
                f4.close()
                check("管道状态记录了引擎的定稿", after != before, f"{before!r} -> {after!r}")
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
