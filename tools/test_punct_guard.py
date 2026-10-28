# -*- coding: utf-8 -*-
"""回归测试:标点模型不许吃掉正文。

背景 —— 「说英文整句被跳过/删掉」的真实机制(2026-10-28 复现并修好):
  render() 原来无条件采信 CT-Transformer 的返回值。这个模型会**偶发只回前一小段**:

      in  我理解不了的你怎么报错了我现在就在 OPEN CODE 里面你居然跟我说必须用
          OPEN CODE 才能用免费模型我现                     (59 字)
      out 我理解不了的,你怎么报错了?                        (14 字)  ← 丢掉 45 字

  只差一个字的邻居输入(58 / 60 字)又是正常的 —— 是英文单词把 token 数顶到了
  某个边界上,所以「说到英文」才踩得到。识别原文(`[raw]`)是完整的,丢掉的是
  **渲染结果**,而下游上屏器只看到「文本变短了」,于是按差值退格 —— 用户看到的就是
  「说到英文,它就直接自动跳过了 / 全删掉了」。
  真实日志里已经发生过多次,例如 `logs/sainput-20261017-185340.log:1042`
  (58 字 → 13 字)与 `logs/sainput-20261028-074200.log:2658`(67 字 → 59 字)。

修法:标点和热词一样只是加分项 —— `punctuate_guarded()` 每次跑完都验一遍
「正文指纹」,模型只要动了正文就整句退回未加标点的文本(宁可不带标点,也不能少字)。

跑法:
  python tools/test_punct_guard.py

不需要麦克风、不需要 exe,只跑文本链路。
"""
from __future__ import annotations

import argparse
import glob
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, HERE)

import asr_sidecar as sc  # noqa: E402

PUNCT_DIR = os.path.join(
    REPO, "models", "sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8")

# 全部是**真实日志里的 [raw]** —— 每一个都被实测过会让标点模型吃掉正文。
# 前两条是丢得最狠的,最后一条是 2026-10-28 当天那次(丢了尾巴「他上次听到的那」)。
PINNED = [
    ("2026-10-17 OPEN CODE", 39,
     "我理解不了的你怎么报错了我现在就在 OPEN CODE 里面你居然跟我说必须用 OPEN CODE 才能用免费模型我现"),
    ("2026-10-18 终端框", 10,
     "然后我打开这个输入法的时候不知道为什么它总是会弹出来一个终端框你刚才都开了十几个了一个都没关掉"
     "我现在已经把它们关掉了但现在还有一还开着一个这应该是内部调试专用的框吧你平时不用打开的吧应该做成应该"),
    ("2026-10-20 视频网站", 7,
     "我之前看视频网站的时候看到过一些他们做的还挺好的很流畅很丝滑那种模式也有对应的项目"),
    ("2026-10-28 第五 首次进去", 3,
     "第五首次进去之后应该是从第一手开始播放如果用户关掉了下次再进来之后应该是记住他上次听到哪里"
     "然后重新开始播放他上次听到的那首"),
]


class TruncatingPunct:
    """假标点模型:只回前 5 个字 —— 用来给守卫做「必须拒绝」的定点测试。"""

    def add_punctuation(self, t: str) -> str:
        return t[:5] + "。"


class NormalPunct:
    """假标点模型:老老实实只加标点。守卫必须接受它。"""

    def add_punctuation(self, t: str) -> str:
        return t + "。"


def load_punct():
    cands = sorted(glob.glob(os.path.join(PUNCT_DIR, "model*.onnx")),
                   key=lambda p: (".int8" not in p, p))
    if not cands:
        return None
    import sherpa_onnx
    return sherpa_onnx.OfflinePunctuation(sherpa_onnx.OfflinePunctuationConfig(
        model=sherpa_onnx.OfflinePunctuationModelConfig(ct_transformer=cands[0],
                                                        num_threads=1)))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--verbose", action="store_true")
    a = ap.parse_args()
    bad = 0

    # ---- 1) 守卫的定点测试(不需要模型)-------------------------------------
    # 指纹必须把各类标点/符号都抹掉:漏一类,正常加了标点的结果就会被误判成
    # 「吃掉了正文」,守卫反过来把标点丢掉。标点用转义写,免得被任何一环做全角化。
    puncts = (",.;:!?\uff0c\u3002\uff01\uff1f\uff1a\uff1b\u3001\u2026"
              "\u3010\u3011\u300a\u300b\u3008\u3009\u300c\u300d(){}[]<>"
              "\u2014_\u2013~\u00b7|/\\+=*&^%$#@`'\"")
    got = sc.content_key("A" + puncts + "B\uff0cC")
    ok = got == "abc"
    print(("OK   " if ok else "FAIL "), f"指纹要抹掉标点/符号、留下正文: {got!r}")
    bad += 0 if ok else 1

    out, dropped = sc.punctuate_guarded(TruncatingPunct(), "我在录音识别文字的过程中只要说到英文它就")
    ok = dropped and out == "我在录音识别文字的过程中只要说到英文它就"
    print(("OK   " if ok else "FAIL "), "吃掉正文的标点模型必须被拒绝(退回原文)")
    bad += 0 if ok else 1

    out, dropped = sc.punctuate_guarded(NormalPunct(), "你好世界")
    ok = (not dropped) and out == "你好世界。"
    print(("OK   " if ok else "FAIL "), "只加标点的模型必须被接受")
    bad += 0 if ok else 1

    out, dropped = sc.punctuate_guarded(None, "你好世界")
    ok = (not dropped) and out == "你好世界"
    print(("OK   " if ok else "FAIL "), "没有标点模型时原样返回")
    bad += 0 if ok else 1

    # stderr 坏掉时(detach 后的控制台句柄),守卫的说明行写不出去 —— 但它**绝不能**
    # 把识别搞挂:早先这里是裸 print,一旦抛 OSError 就会顺着异常把这一句的文本吞掉。
    class Broken:
        def write(self, _s):
            raise OSError(9, "Bad file descriptor")

        def flush(self):
            raise OSError(9, "Bad file descriptor")

    saved = sys.stderr
    sys.stderr = Broken()
    src = "我在录音识别文字的过程中只要说到英文它就"
    try:
        # 输入够长,桩模型必然截断 → 走进「丢掉正文」分支 → 必须去写那一行 note()
        got = sc.render_text(src, TruncatingPunct(), True, "keep", True, True)
    except Exception as e:  # noqa: BLE001
        got = f"<raised {type(e).__name__}: {e}>"
    finally:
        sys.stderr = saved
    ok = got == src
    print(("OK   " if ok else "FAIL "), f"stderr 写不出去也不能吞掉这一句: {got!r}")
    bad += 0 if ok else 1

    # ---- 2) 真模型 + 真链路 -------------------------------------------------
    punct = load_punct()
    if punct is None:
        print("SKIP  没找到标点模型目录:", PUNCT_DIR)
        return 1 if bad else 0

    raw_losses = 0
    for name, was, text in PINNED:
        raw = punct.add_punctuation(text)
        raw_lost = len(sc.content_key(text)) - len(sc.content_key(raw))
        raw_losses += 1 if raw_lost > 0 else 0
        got = sc.render_text(text, punct, True, "space", True, True)
        lost = len(sc.content_key(text)) - len(sc.content_key(got))
        ok = lost <= 0
        print(("OK   " if ok else "FAIL "),
              f"{name:24s} 原文{len(text):3d}字 -> 渲染{len(got):3d}字 丢{lost:2d}"
              f"(修之前丢 {was};光跑标点模型丢 {raw_lost})")
        if a.verbose or not ok:
            print("        out:", got)
        bad += 0 if ok else 1

    if raw_losses == 0:
        print("NOTE 这个标点模型目前没有再复现吃掉正文 —— 守卫仍是必须的,"
              "上面第一条定点测试盯着它")
    else:
        print(f"NOTE 标点模型仍然会吃掉正文({raw_losses}/{len(PINNED)} 条),"
              "守卫把它们全部挡住了")

    print()
    print("=== %s ===" % ("全部通过" if bad == 0 else f"{bad} 项失败"))
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
