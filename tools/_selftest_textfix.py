# -*- coding: utf-8 -*-
"""文本流水线的纯函数回归(不碰键盘、不碰窗口,随时可跑)。

两段:
  A) `textfix.normalize` 的大小写 / 合并 / 后缀小写
  B) **整条流水线** `repair -> itn -> polish -> textfix` —— 输入用的是**会话日志里
     ASR 原样吐出来的 `[raw]` 字符串**,期望是「应该上屏的样子」。
     为什么要 B:2026-10-31 用户报的那几条(型号数字、点、重复字被吞)全在
     python 这一层,能在这里钉住就不必去真机上打字复现。
"""
import sys, io
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")
import textfix, repair, itn, polish


def pipeline(raw):
    """与 tools/asr_sidecar.py 的 render_text 同序(标点模型那一步在纯函数里没有,跳过)。"""
    t = repair.repair(raw)
    t = itn.normalize(t)
    t = polish.polish(t)
    t = textfix.normalize(t, 'keep', fix_case=True)
    return t.rstrip()


# A) textfix 单件
CASES_TEXTFIX = [
    ("MONDAY TODAY IS THE DAY", "monday today is the day"),
    ("我用 API 调的", "我用 API 调的"),
    ("VPS 上跑的 CODEX", "VPS 上跑的 CODEX"),
    ("HELLO WORLD", "hello world"),
    ("这个 AI 和 API 都要", "这个 AI 和 API 都要"),
    ("AFTER TWO DAYS", "after two days"),
    ("Random Person Name", "Random Person Name"),   # 名单外 -> 原样
    ("我CODEX还在做R E A任务", "我CODEX还在做REA任务"),
    # 2026-10-31:已知缩写表里的「A I」不再把长串切碎
    ("S A A A I", "SAAAI"),
    ("S A A A I.COM", "SAAAI.com"),
    ("S A A A I 点 COM", "SAAAI 点 COM"),            # 「点」归 ITN 管,这一层不动(见 B 段)
    # 后缀 / 顶级域名小写(白名单内才动)
    ("AGENTS.MD", "AGENTS.md"),
    ("SAAAI.COM", "SAAAI.com"),
    ("看 AI.GPT 那个", "看 AI.GPT 那个"),            # 名单外 -> 原样
]

# B) 整条流水线:左边是日志里 ASR 的原样输出,右边是应该上屏的样子
# 注意:这里不含 CT-Transformer 标点模型(离线跑不起来),所以**句内的空格**保持本层结果;
# 真机上标点模型会再整形一遍空格,不影响判断——只看数字/点/重复字这些。
CASES_PIPELINE = [
    # --- 2026-10-31 用户实测:英文 + 数字混说 ---
    ("比如我说 MP 四", "比如我说 MP4"),
    ("V 二版本", "V2版本"),
    ("MP 三", "MP3"),
    ("M P 四", "MP4"),                              # 拆开念的字母:先并数字,再收拢字母组
    ("AGENTS 点 M D", "AGENTS.md"),
    ("S A A A I 点 COM", "SAAAI.com"),
    ("甚至这个四个 A", "甚至这个四个 A"),
    # --- 2026-10-31 用户实测:重复的字被吞 ---
    ("AAAA", "AAAA"),
    ("saaaai", "saaaai"),
    ("等等等类似的话题", "等等等类似的话题"),
    ("来来来你试试", "来来来你试试"),
    ("喂喂喂听得见吗", "喂喂喂听得见吗"),
    ("我下午三点十五分开会", "我下午3点15分开会"),     # 时刻规则不许被「点」规则碰坏
    # --- 不许动的老行为 ---
    ("hello world 一二三", "hello world 一二三"),
    ("我的验证码是九零三七二一", "我的验证码是903721"),
    ("一二三四五", "12345"),
    ("他数了一遍一二三四五", "他数了一遍一二三四五"),
    ("跟我们 OMP 一样", "跟我们 OMP 一样"),
    ("金融动动动动荡荡荡荡的表现", "金融动荡的表现"),
    ("识别别本身好像问题到是不大", "识别本身好像问题到是不大"),
]

bad = 0
print("=== A) textfix ===")
for src, want in CASES_TEXTFIX:
    got = textfix.normalize(src)
    if got == want:
        print("OK  ", repr(src), "->", repr(got))
    else:
        bad += 1
        print("FAIL", repr(src), "\n     期望:", repr(want), "\n     实得:", repr(got))

print("\n=== B) repair -> itn -> polish -> textfix ===")
for src, want in CASES_PIPELINE:
    got = pipeline(src)
    if got == want:
        print("OK  ", repr(src), "->", repr(got))
    else:
        bad += 1
        print("FAIL", repr(src), "\n     期望:", repr(want), "\n     实得:", repr(got))

total = len(CASES_TEXTFIX) + len(CASES_PIPELINE)
print("\n=== %d passed / %d failed (共 %d) ===" % (total - bad, bad, total))
sys.exit(1 if bad else 0)
