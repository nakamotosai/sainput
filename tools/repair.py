# -*- coding: utf-8 -*-
"""sainput 重复修复 —— 给 ASR 的重复循环兜个底。

为什么需要：zipformer 这类流式模型在小声、语速快、或句中切换语言时会在词尾打转，
输出「动动动荡荡荡」「为什么为什么」这种。换更强的模型能消掉绝大部分（实测
x-asr-480ms 在真机中文上重复数为 0），但模型永远可能偶发，这里做最后一道保险。

原则：**只动几乎不可能是正常中文的形态**。
正常叠字（看看 / 说说 / 刚刚 / 天天 / 妈妈 / 慢慢）和正常叠词（很多很多 / 一步一步 /
可以可以）一律不碰；英文的重复（ALWAYS ALWAYS）也不碰。
"""
import re

# 这些字连着出现两遍基本就是解码打转（正常中文里它们不叠字）
_NEVER_DOUBLE = set(
    "的了是在有和与也就都很别出来去么什这那他它们到为以被把让给向从而且或但还又再经过着吗呢吧"
    "连叠址识错操续问方"
)

# 笑声/语气词：连着多个是正常的，压到 2 个
_LAUGH = set("哈嘿呵嘻嗯哦")

_CJK = re.compile(r"[\u4e00-\u9fff]")
_RE_RUN = re.compile(r"(.)\1{2,}")
_RE_BLOCK3 = re.compile(r"(.{3,10}?)\1+")   # 3 字以上片段重复 2 次即算打转
_RE_BLOCK2 = re.compile(r"(.{2})\1{2,}")    # 2 字片段要重复 3 次以上才算
_RE_PAIR = re.compile("([" + re.escape("".join(sorted(_NEVER_DOUBLE))) + r"])\1")


def _has_cjk(s):
    return bool(_CJK.search(s))


def repair(t):
    if not t:
        return t

    def _run(m):
        ch = m.group(1)
        return ch * (2 if ch in _LAUGH else 1)

    # 1) 同一个字连续 >=3 次 -> 1 个（叠叠叠叠叠叠 -> 叠）
    t = _RE_RUN.sub(_run, t)

    # 2) 片段原样重复：3 字以上重复 2 次、2 字重复 3 次以上才压。
    #    只对含中文的片段生效 —— 「ALWAYS ALWAYS」「很多很多」这种正常内容不许动。
    def _block(m):
        return m.group(1) if _has_cjk(m.group(1)) else m.group(0)

    for rx in (_RE_BLOCK3, _RE_BLOCK2):
        for _ in range(4):
            nt = rx.sub(_block, t)
            if nt == t:
                break
            t = nt

    # 3) 「不叠字」的字连着两遍 -> 一个（识别别 -> 识别）
    prev = None
    while prev != t:
        prev = t
        t = _RE_PAIR.sub(r"\1", t)
    return t


# ------------------------------------------------------------------ selftest
CASES = [
    ("金融动动动荡荡荡的表现", "金融动荡的表现"),
    ("识别别本身好像问题到是不大", "识别本身好像问题到是不大"),
    ("叠叠叠叠叠叠叠址字", "叠址字"),
    ("为什么为什么一直会有", "为什么一直会有"),
    ("重点重点重点呢", "重点呢"),
    ("连续发出来出来的", "连续发出来出来的"),        # 不规则重复修不了，原样保留
    ("哈哈哈哈太好笑了", "哈哈太好笑了"),
    ("我操操你这个视频太太有意思了", "我操你这个视频太太有意思了"),
    # 以下都是正常内容，一个字都不许动
    ("我看看再说", "我看看再说"),
    ("他刚刚走了", "他刚刚走了"),
    ("妈妈天天很忙", "妈妈天天很忙"),
    ("稍微慢一点儿", "稍微慢一点儿"),
    ("很多很多的人", "很多很多的人"),
    ("一步一步来", "一步一步来"),
    ("可以可以，没问题", "可以可以，没问题"),
    ("好好休息", "好好休息"),
    ("天天向上", "天天向上"),
    ("嗯嗯我听着呢", "嗯嗯我听着呢"),
    ("重点重点呢", "重点重点呢"),                  # 2 字只重复 2 次，可能是正常叠词，不动
    ("这是第一种第二种叫呃与 ALWAYS ALWAYS什么", "这是第一种第二种叫呃与 ALWAYS ALWAYS什么"),
    ("对我做了介绍啊那么我想说的是呢", "对我做了介绍啊那么我想说的是呢"),
    ("重点呢想谈三个问题首先呢就是这一轮全球金融动荡的", "重点呢想谈三个问题首先呢就是这一轮全球金融动荡的"),
]


def _selftest():
    bad = 0
    for src, want in CASES:
        got = repair(src)
        if got != want:
            bad += 1
            print("FAIL  %s\n  得到 %s\n  期望 %s" % (src, got, want))
    print("%d/%d 通过" % (len(CASES) - bad, len(CASES)))
    return bad


if __name__ == "__main__":
    import sys
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    if len(sys.argv) > 1:
        print(repair(" ".join(sys.argv[1:])))
    else:
        sys.exit(1 if _selftest() else 0)