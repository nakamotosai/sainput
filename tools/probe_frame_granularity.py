# -*- coding: utf-8 -*-
"""T6.5 采集粒度 A/B:32ms 帧 vs 16ms 帧,首字延迟与解码开销各是多少。

判据(**都可机检**,不靠感觉):
  首字延迟 = 该句**第一条有字的 `P` 行**的 `ms`  —— 那是边车时间线上的音频时刻,
             也就是"喂进去多少毫秒音频才吐出第一个字"。
  解码开销 = `P` 行括号里的 `(x.xms)`,即每次解码调用本身的耗时;同时统计次数
             与"每秒音频的解码次数" —— 帧变细就是调用次数翻倍,代价必须看这个。

跑法(每次一个全新进程 = 一句,避免进程间噪声混进来):
    python tools/probe_frame_granularity.py --label 32ms
    # 改 crates/sainput-core/src/audio.rs 的 FRAME_MS=16 并重建后:
    python tools/probe_frame_granularity.py --label 16ms
结果是 tmp/probe-frame-<label>.json,拿两份对比。

⚠️ 引擎跑在**私有桌面**上,不打扰当前窗口;用 `SAINPUT_SINGLE_NS` 隔离,
   绝不会接管你正在用的那个实例。
"""
import argparse
import json
import os
import pathlib
import re
import statistics
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hdesk  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parent.parent
DESKTOP = "SAI-FrameGranularity"
NS = "sainput-frameab"
WAV = REPO / "models/sherpa-onnx-streaming-zipformer-zh-14M-2023-02-23/test_wavs/0.wav"
ENV_KEYS = ("SAINPUT_SINGLE_NS", "SAINPUT_DIAG_AUTOSTART", "SAINPUT_DIAG_TARGET_HWND")
P_RE = re.compile(r"^P\s+(\d+)ms\s+(.*?)\s+\[raw\]\s+(.*?)\s+\(([\d.]+)ms\)\s*$")


def first(cands, what):
    for c in cands:
        if c.exists():
            return c
    print(f"FAIL: {what} 不存在")
    sys.exit(2)


def one_run(sainput, host, wav, run_idx, release_at):
    """跑一句,返回 (首字延迟ms, 解码耗时列表, 秒数)。"""
    h = hdesk.open_desktop(DESKTOP)
    saved = {k: os.environ.get(k) for k in ENV_KEYS}
    hh = hp = None
    hpid = epid = 0
    try:
        os.environ["SAINPUT_SINGLE_NS"] = NS
        os.environ["SAINPUT_DIAG_AUTOSTART"] = "1500"  # 进程内触发热键,不合成按键
        hh, ht, hpid = hdesk.launch(
            str(host), f"--life 60000 --log {REPO / 'tmp' / 'framehost.log'}", DESKTOP)
        edit = None
        t0 = time.time()
        while time.time() - t0 < 10:
            edit = hdesk.find_edit(hpid, h)
            if edit:
                break
            time.sleep(0.2)
        if not edit:
            raise RuntimeError("宿主窗口没起来")
        os.environ["SAINPUT_DIAG_TARGET_HWND"] = hex(edit)

        # 引擎的会话日志是它自己命名写进 logs/ 的(`--log` 不改变它),
        # 所以用"启动前/后差集"认出这一次的那份。
        before = {p.name for p in (REPO / "logs").glob("*.log")}
        hp, hpt, epid = hdesk.launch(
            str(sainput),
            f"run --feed {wav}" + (f" --feed-release-at {release_at}" if release_at > 0 else "")
            + " --ime-wmchar on --no-leading-space --no-trailing-space"
            + " --overlay-mode off",
            DESKTOP)
        # 等到**第一句定稿**(F 行)就收工 —— 一句就够,不陪它循环
        log = None
        t0 = time.time()
        while time.time() - t0 < 45:
            new = [p for p in (REPO / "logs").glob("*.log") if p.name not in before]
            if new:
                log = max(new, key=lambda p: p.stat().st_mtime)
                txt = log.read_text(encoding="utf-8", errors="replace")
                if re.search(r"^F\s+\d+ms", txt, re.M):
                    break
            time.sleep(0.4)
        time.sleep(0.3)
        txt = log.read_text(encoding="utf-8", errors="replace") if log else ""
        hdesk.close(hpt)
        return parse(txt)
    finally:
        if hp:
            hdesk.terminate(epid)
            hdesk.close(hp)
        if hh:
            hdesk.close(hh)
        hdesk.close(h)
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


def parse(txt):
    """从日志里取**第一句**的首字延迟 + 这一句的每次解码耗时。

    句的边界是 `F `(定稿)/`T `(松手收尾)行;`P ` 是这一句的逐帧假设。
    `P` 行前面那个 `ms` 是**边车时间线**(喂进去多少音频),所以它天然就是
    "首字延迟",不含进程启动、模型加载那些噪声。
    """
    first_ms = None
    decodes = []
    for ln in txt.splitlines():
        ln = ln.strip()
        if ln.startswith(("F ", "T ")):
            if first_ms is not None:
                break  # 第一句已经结束
            continue
        m = P_RE.match(ln)
        if not m:
            continue
        ms, text, _raw, dec = int(m.group(1)), m.group(2), m.group(3), float(m.group(4))
        if not text.strip():
            continue
        decodes.append(dec)
        if first_ms is None:
            first_ms = ms
    return first_ms, decodes


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--label", required=True, help="这次测的是哪个粒度(写进结果文件)")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--wav", default=str(WAV))
    ap.add_argument("--release-at", type=float, default=0.0)
    ap.add_argument("--bin", default="")
    a = ap.parse_args()

    sainput = pathlib.Path(a.bin) if a.bin else first([
        REPO / "target-test/x86_64-pc-windows-gnu/release/sainput.exe",
        REPO / "target/x86_64-pc-windows-gnu/release/sainput.exe",
    ], "sainput.exe")
    host = first([
        REPO / "target-test/x86_64-pc-windows-gnu/release/sainput-tsf-host.exe",
        REPO / "target/x86_64-pc-windows-gnu/release/sainput-tsf-host.exe",
    ], "sainput-tsf-host.exe")
    print(f"引擎 {sainput}\n靶子 {host}\nwav {a.wav}\n=== {a.label} ===")

    rows = []
    for i in range(a.runs):
        first_ms, decodes = one_run(sainput, host, a.wav, i, a.release_at)
        if first_ms is None:
            print(f"  第 {i + 1} 轮:没拿到首字(这一轮作废)")
            continue
        avg = statistics.mean(decodes) if decodes else 0.0
        rows.append({
            "first_ms": first_ms,
            "decodes": len(decodes),
            "decode_avg_ms": round(avg, 2),
            "decode_max_ms": round(max(decodes), 2) if decodes else 0.0,
            "decode_total_ms": round(sum(decodes), 2),
        })
        print(f"  第 {i + 1} 轮:首字 {first_ms}ms | 解码 {len(decodes)} 次,"
              f"均 {avg:.2f}ms 峰 {max(decodes):.2f}ms 合计 {sum(decodes):.2f}ms"
              f" | 每次解码间隔 {first_ms / max(1, len(decodes)):.0f}ms 音频")

    if not rows:
        print("没有任何有效数据")
        return 1
    summary = {
        "label": a.label,
        "audio_seconds": a.release_at,
        "first_ms_median": statistics.median(r["first_ms"] for r in rows),
        "first_ms_min": min(r["first_ms"] for r in rows),
        "decode_avg_ms": round(statistics.mean(r["decode_avg_ms"] for r in rows), 2),
        "decode_calls_median": statistics.median(r["decodes"] for r in rows),
        "decode_total_ms_median": statistics.median(r["decode_total_ms"] for r in rows),
        "runs": rows,
    }
    out = REPO / "tmp" / f"probe-frame-{a.label}.json"
    out.write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"\n首字延迟(中位) {summary['first_ms_median']:.0f}ms | "
          f"解码均 {summary['decode_avg_ms']}ms × {summary['decode_calls_median']:.0f} 次/句"
          f" = {summary['decode_total_ms_median']:.0f}ms/{a.release_at:.0f}s 音频")
    print(f"结果写入 {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
