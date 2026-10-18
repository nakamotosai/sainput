# -*- coding: utf-8 -*-
"""sainput 排版润色 —— 标点模型出来之后的最后一道整形。

标点模型（CT-Transformer）不懂排版，会留下这些毛刺：
  · 数字和 % 之间塞空格：37 % -> 37%
  · 中文标点前后多空格
  · 纯英文句子被加上中文标点：hello world，this is a test。 -> hello world, this is a test.
这里只做「不改语义、只去毛刺」的替换。
"""

import re

_CJK = r'\u4e00-\u9fff'
_PUNCT_CJK = '，。！？、；：）】》”’'
_OPEN_CJK = '（【《“‘'

_FW2HW = {
    '，': ', ', '。': '. ', '？': '? ', '！': '! ',
    '、': ', ', '；': '; ', '：': ': ',
    '（': ' (', '）': ')',
}


def polish(t):
    if not t:
        return t
    t = re.sub(r'(\d)\s+%', r'\1%', t)
    t = re.sub(r'[ \t]+(?=[' + _PUNCT_CJK + r'])', '', t)
    t = re.sub(r'(?<=[' + _OPEN_CJK + r'])[ \t]+', '', t)
    t = re.sub(r'(?<=[' + _CJK + r'])[ \t]+(?=[' + _CJK + r'])', '', t)
    if not re.search(r'[' + _CJK + r']', t):
        for a, b in _FW2HW.items():
            t = t.replace(a, b)
        t = re.sub(r'[ \t]{2,}', ' ', t).strip()
    return t

# 句尾标点：流式过程中这一笔还没定稿（下一批字很可能把它变成逗号或挪位置），
# 显示出来只会一闪一闪。定稿（final）时才保留。
_TERM = '。．.！!？?；;，,、：:'


def strip_terminal(t, as_space=False):
    """砍掉句尾未定稿的标点。

    `as_space=True`：把砍掉的标点位置**换成一个空格**，而不是直接消失。
    为什么需要这个模式：用户开了「标点转空格」之后，句读本来就该显示成空格；
    流式阶段如果直接剥掉，屏幕上看到的就是「…是怎么回事尤其是」—— 词全粘在一起，
    和用户要的「逗号变空格」正好相反。
    """
    t = t.rstrip()
    had = False
    while t and t[-1] in _TERM:
        t = t[:-1].rstrip()
        had = True
    if had and as_space and t:
        t = t + ' '
    return t


def render_partial(t, as_space=False):
    """流式显示用：润色 + 砍掉句尾未定稿标点（可选换成空格）。"""
    return strip_terminal(polish(t), as_space=as_space)
