# -*- coding: utf-8 -*-
"""光标上下文探针 —— 读出「焦点输入框里光标之前的文本」，供上下文偏置用。

用法：
    python tools/ctx_probe.py --print        # 打印抓到的前文（人看，调试用）
    python tools/ctx_probe.py --terms        # 打印抽出的术语（一行一个）

为什么要单独一个脚本、而不是在 Rust 里做：
    读焦点输入框要靠 UI Automation 的 COM 接口。手写 vtable 偏移一旦对不上就是
    **静默内存错乱** —— 这正是本项目用「子进程 + JSON」把 sherpa-onnx 隔离出去的理由
    （见 crates/sainput-core/src/asr.rs 文件头）。Python 侧已装 `uiautomation`，
    读起来安全得多。Rust 只做纯文本抽词（crates/sainput-core/src/context.rs）。

与云端方案的差异（照实说）：云端方案把你刚发出去的历史文本打包上传、由**云端模型**消费；
我们只能读**当前光标前文**、且只能喂**本地热词表**。方向一致，强度不如云端。

失败即静默降级：没装 uiautomation / 焦点不是编辑框 / 超时 → 打印空、退出码 0。
绝不抛异常打断主流程。
"""
import argparse
import sys

# 抽术语的逻辑和 Rust 侧 context.rs::extract_terms 保持一致（两边都要能独立跑）。
_TERM_CHARS = set("_-./:+:#@\\")


def extract_terms(text, limit=24):
    """只抽含 ASCII 字母数字的串（英文词/缩写/版本号/路径）。纯中文不进热词表。"""
    out = []
    cur = []
    def flush():
        w = "".join(cur).strip()
        if len(w) >= 2 and any(c.isascii() and c.isalnum() for c in w) and w not in out:
            out.append(w)
        cur.clear()
    for ch in text:
        if (ch.isascii() and ch.isalnum()) or ch in _TERM_CHARS:
            cur.append(ch)
        else:
            flush()
    flush()
    out = [w for w in out if any(c.isascii() and c.isalnum() for c in w)]
    out.sort(key=len, reverse=True)
    return out[:limit]


def focused_text(timeout_ms=400):
    """读焦点元素的文本；任何失败返回 ''。

    两条读法，按可靠性排序：
      1) ValuePattern（编辑框的标准接口）—— 大多数原生/浏览器输入框走它。
      2) 退回「控件本身的 Name/Value」—— 有些自绘控件只暴露这个。
    只问一次、只问焦点元素，不遍历控件树，避免拖慢听写。

    注意：不要自作聪明去动 `comtypes.client.gen_dir`。本机那个目录 ACL 只读，
    看起来像是"该重定向到 %TEMP%"，但 comtypes 是按**模块名**加载生成代码的，
    重定向后会撞上已存在的同名模块（`Typelib different than module` → 加载失败）。
    默认路径实测能正常 import，保持默认。
    """
    try:
        import uiautomation as auto
    except Exception:
        return ""
    try:
        auto.SetGlobalSearchTimeout(timeout_ms / 1000.0)
    except Exception:
        pass
    try:
        el = auto.GetFocusedControl()
        if el is None:
            return ""
        # 1) ValuePattern
        try:
            vp = el.GetValuePattern()
            if vp is not None:
                v = vp.Value
                if v:
                    return v
        except Exception:
            pass
        # 2) 退回 Name（部分控件把内容放在 Name 上）
        try:
            n = el.Name
            if n:
                return n
        except Exception:
            pass
    except Exception:
        return ""
    return ""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--print", dest="do_print", action="store_true", help="打印抓到的前文")
    ap.add_argument("--terms", action="store_true", help="打印抽出的术语（一行一个）")
    ap.add_argument("--timeout-ms", type=int, default=400)
    a = ap.parse_args()

    text = focused_text(a.timeout_ms)
    if a.terms:
        for t in extract_terms(text):
            print(t)
    elif a.do_print:
        print(text)
    else:
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
