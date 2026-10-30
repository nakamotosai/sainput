# -*- coding: utf-8 -*-
"""sainput ITN —— 把 ASR 吐出的汉字数字改写成阿拉伯数字（保守版）。

设计原则：宁可少改，不可改错。
只有两类语境才动手：
  1) 强格式：日期(年/月/日/号)、时刻(点分/点半)、百分比(百分之X)、金额(元/块钱)；
  2) 数字串后面紧跟明确的量词/单位，且数字串够长（≥2 字或含 十百千万）——
     这样正文里的「一个想法」「三本书」不会被写成「1个想法」，读起来才不像机器人。
歧义词（一点、十分、一下、三天两头、一两年……）一律不动；任何一处解析不出来就整段原样返回。
"""

import re
import sys

# 「常见英文词」白名单借 textfix 那一份(别抄两遍,两份迟早会漂)。
# 它用来区分「型号/版本」(MP / V / GPT)和「普通英文词」(hello / world):
# 只有前者才跟后面的汉字数字并成阿拉伯数字。
try:
    from textfix import COMMON_WORDS as _COMMON_WORDS
except Exception:                       # 拿不到就整条规则不动(宁可少改,不可改错)
    _COMMON_WORDS = set()

_CN_MAP = {
    '零': 0, '〇': 0, '一': 1, '二': 2, '两': 2, '三': 3, '四': 4,
    '五': 5, '六': 6, '七': 7, '八': 8, '九': 9,
}
_SMALL = {'十': 10, '百': 100, '千': 1000}
_BIG = {'万': 10 ** 4, '亿': 10 ** 8}

_N = '零〇一二三四五六七八九'            # 纯数位
_NU = '零〇一二两三四五六七八九十百千万亿'   # 数位 + 单位
_SP = '一二两三四五六七八九十'            # 时刻用(不含 零/〇)
# 逐位读法(验证码/号码/电话)专用的数字集:`幺` 算 1(「幺零零八六」= 10086),
# **不含**「两」(念号码不会说「两」,而「两」在正文里很常见)。
_ND = '零〇一二三四五六七八九幺'
_ND_MAP = dict(_CN_MAP)
_ND_MAP['幺'] = 1


def cn2num(s):
    """汉字数字 -> int；解析不了返回 None。支持 万/亿，也支持「二零二五」这种逐位读法。"""
    if not s:
        return None
    if any(c not in _CN_MAP and c not in _SMALL and c not in _BIG for c in s):
        return None
    if not any(c in _SMALL or c in _BIG for c in s):
        return int(''.join(str(_CN_MAP[c]) for c in s))   # 二零二五 -> 2025
    for big, mul in (('亿', _BIG['亿']), ('万', _BIG['万'])):
        if big in s:
            i = s.index(big)
            left, right = s[:i], s[i + 1:]
            l = cn2num(left) if left else 1
            r = cn2num(right) if right else 0
            if l is None or r is None:
                return None
            return l * mul + r
    total = 0
    cur = 0
    for ch in s:
        if ch in _CN_MAP:
            cur = _CN_MAP[ch]
        else:
            u = _SMALL[ch]
            if cur == 0:
                cur = 1          # 「十五」=15 / 「百」=100
            total += cur * u
            cur = 0
    return total + cur


def _digits(s):
    """逐位转数字串(保留前导零):零五 -> '05'。"""
    if not s or any(c not in _CN_MAP for c in s):
        return None
    return ''.join(str(_CN_MAP[c]) for c in s)


def _ndigits(s):
    """逐位读法的数字串:幺也算 1。`幺零零八六` -> '10086'。"""
    if not s or any(c not in _ND_MAP for c in s):
        return None
    return ''.join(str(_ND_MAP[c]) for c in s)


# 这些词整块保护，绝不改写（规则是启发式的，这层兜底挡住最容易翻车的成语/熟语）
_IDIOMS = [
    '三天两头', '一年半载', '一年到头', '一年一度', '三年五载', '一两年', '两三天', '三两天',
    '一个劲', '一个劲儿', '四人帮', '一天到晚', '一日三秋', '一分为二', '三长两短', '三五成群',
    '三番五次', '四面八方', '五湖四海', '十拿九稳', '千钧一发', '一毛不拔', '一落千丈',
    '十年寒窗', '百分百', '千言万语', '一模一样', '一五一十', '乱七八糟', '七上八下',
]


def _protect(t):
    hold = {}
    for i, w in enumerate(_IDIOMS):
        if w in t:
            k = '\x01%d\x02' % i
            hold[k] = w
            t = t.replace(w, k)
    return t, hold


def _restore(t, hold):
    for k, w in hold.items():
        t = t.replace(k, w)
    return t


def _r(pat, build):
    rx = re.compile(pat)

    def f(m):
        out = build(m.groups())
        return m.group(0) if out is None else out

    return rx, f


def _g(*vals):
    out = []
    for v in vals:
        if v is None or v == '':
            out.append(None)
            continue
        n = cn2num(v)
        if n is None:
            return None
        out.append(str(n))
    return out


# --- 规则体 --------------------------------------------------------------
def _date3(g):
    v = _g(g[0], g[1], g[2])
    return None if v is None else '%s年%s月%s%s' % (v[0], v[1], v[2], g[3])


def _date2(g):
    v = _g(*g)
    return None if v is None else '%s年%s月' % (v[0], v[1])


def _month_day(g):
    v = _g(g[0], g[1])
    return None if v is None else '%s月%s%s' % (v[0], v[1], g[2])


def _year(g):
    v = _g(*g)
    return None if v is None else '%s年' % v[0]


def _month(g):
    v = _g(*g)
    return None if v is None else '%s月' % v[0]


def _hms(g):
    v = _g(*g)
    return None if v is None else '%s点%s分' % (v[0], v[1])


def _half(g):
    v = _g(g[0])
    return None if v is None else '%s点%s' % (v[0], g[1])


def _pct(g):
    head = _g(g[0])
    if head is None:
        return None
    if g[1] is None:
        return '%s%%' % head[0]
    tail = _digits(g[1])
    return None if tail is None else '%s.%s%%' % (head[0], tail)


def _decimal(g):
    head = _g(g[0])
    if head is None:
        return None
    tail = _digits(g[1])
    return None if tail is None else '%s.%s' % (head[0], tail)


def _money(g):
    v = _g(g[0])
    return None if v is None else '%s%s' % (v[0], g[1])


def _unit(g):
    raw = g[0]
    # 单字「一」「三」+ 量词保持汉字,正文才不别扭(一个/三个/一本书)
    if len(raw) < 2 and not any(c in _SMALL or c in _BIG for c in raw):
        return None
    v = _g(raw)
    return None if v is None else '%s%s' % (v[0], g[1])


# 「验证码是一二三四五」这类:**前面有明确的号码提示词**才逐位转数字。
# 为什么单独一条:整句都是数字的那条兜底在 `whole_digits()`(整句只有一个数字串);
# 夹在句子中间的只有靠提示词才敢动手 —— 否则「他数了一遍一二三四五」也会被改。
# 顺序讲究:长词排在短词前面(身份证号 先于 身份证、订单号 先于 单号)。
_CUES = ('验证码|校验码|识别码|取件码|激活码|提货码|付款码|密码|口令|'
         '身份证号|身份证|订单号|流水号|单号|编号|号码|工号|学号|卡号|'
         '手机号|电话|座机|车牌号|车牌|房间号|座位号|门牌号|邮编')


def _after_cue(g):
    """g = (提示词, 中间词或 None, 数字串)。"""
    run = _ndigits(g[2])
    if run is None:
        return None
    return '%s%s%s' % (g[0], g[1] or '', run)


_UNITS = ('个|次|人|岁|米|千米|公里|克|千克|公斤|斤|吨|度|页|行|条|张|件|台|套|本|份|杯|瓶|'
          '层|楼|间|位|名|遍|场|倍|折|小时|分钟|秒')


# --- 型号 / 版本 / 域名 里的「拉丁 + 数字」「点」 ---------------------------------
# 2026-10-31 用户实测三条(原话都在这天的会话日志里):
#   `MP 四` -> `MP四`(要 `MP4`)、`V 二版本` -> 要 `V2版本`、`MP 三` -> 要 `MP3`
#   `AGENTS 点 M D` -> 要 `AGENTS.md`、`S A A A I 点 COM` -> 要 `saaaai.com`
# 判据只有两条,都很窄:
#   1) 「拉丁词(可隔一个空格)+ 1~4 位汉字数字」,**且这个词不是常见英文词**
#      —— 借 textfix 的 `COMMON_WORDS`(与那边「只动白名单」同一条原则)。
#      所以 `MP`/`V`/`GPT` 会转,而 `hello world 一二三` 一个都不动。
#   2) 「点」两边都是 ASCII 字母数字(隔空格也认)-> `.`。
#      `三点五` 走的是上面的小数点规则(left 是汉字),这里不会误伤。
# 数字串后面允许跟的**中文字**(只有这些才认「型号/版本」)。
# 为什么要这份白名单(2026-10-31 全语料回归抓到的假阳性):
#   `跟我们 OMP 一样` 被判成型号 → `OMP1样`,而「一样 / 一下 / 一年」都是常用词。
# 所以判据收紧成:后面要么不是汉字(空格/标点/结尾/ASCII),要么是这几个「版本类」字。
_MODEL_TAIL = '版区代期号系列星档成折层楼届'


def _model_num(g):
    word, digits = g[0], g[1]
    if not _COMMON_WORDS:              # 白名单不可用:这条规则整条不动
        return None
    if word.lower() in _COMMON_WORDS:  # 常见英文词 + 空格 + 汉字数字 → 不是型号,不动
        return None
    run = _ndigits(digits)
    return None if run is None else '%s%s' % (word, run)


def _dot(g):
    return '%s.%s' % (g[0], g[1])


_RULES = [
    _r(r'([' + _NU + r']{2,4})年([' + _NU + r']{1,3})月([' + _NU + r']{1,3})([日号])', _date3),
    _r(r'([' + _NU + r']{2,4})年([' + _NU + r']{1,3})月', _date2),
    _r(r'(?<![个每这那几多数半整上下本])([' + _NU + r']{1,3})月([' + _NU + r']{1,3})([日号])', _month_day),
    _r(r'(?<![个每这那几多数半整上下])([' + _NU + r']{1,4})年', _year),
    _r(r'(?<![个每这那几多数半整上下])([' + _NU + r']{1,3})月', _month),
    _r(r'([' + _SP + r']{1,3})点([' + _SP + r']{1,3})分', _hms),
    _r(r'([' + _SP + r']{1,3})点(半|整)', _half),
    _r(r'百分之([' + _NU + r']{1,6})(?:点([' + _N + r']{1,6}))?', _pct),
    _r(r'([' + _SP + r'])点([' + _N + r']{1,9})', _decimal),
    _r(r'([' + _NU + r']{1,6})(元|块钱)', _money),
    _r(r'([' + _NU + r']{1,6})(' + _UNITS + r')', _unit),
    # 提示词 + 数字串(逐位):「验证码是一二三四五」→「验证码是12345」
    _r(r'(' + _CUES + r')(是|为|叫做|:|:)?([' + _ND + r']{2,})', _after_cue),
    # 「点」夹在 ASCII 之间 → `.`(域名 / 文件名):AGENTS 点 M D → AGENTS.M D
    # 右边**必须是字母**(点后面那截是后缀/顶级域名);所以 `3点15分` 这种
    # 时刻/小数不会被误伤 —— 那是上面 `_hms` 的产物,右边是数字。
    _r(r'([A-Za-z0-9]) ?点 ?([A-Za-z])', _dot),
    # 拉丁词(可隔一个空格)+ 1~4 位汉字数字 → 并成阿拉伯数字:MP 四 → MP4 / V 二版本 → V2版本
    # 右边只认:非汉字(空格/标点/结尾/ASCII)或 `_MODEL_TAIL` 里那几个版本类字
    # —— 「OMP 一样」这种常用词因此不会被误判成型号。
    _r(r'(?<![A-Za-z0-9])([A-Za-z][A-Za-z0-9]*) ?([' + _ND + r']{1,4})'
       r'(?![' + _ND + r'])(?=$|[' + _MODEL_TAIL + r']|[^\u4e00-\u9fff])', _model_num),
]


# 整句两侧允许的空白/句尾标点(不参与「是不是纯数字」的判断)
_WHOLE_EDGE = ' \t\u3002.\uff0c,\u3001;\uff1b:\uff1a!\uff01?\uff1f'   # 空白 + 。.，,、;；:：!！?？


def whole_digits(text):
    """整句就是一个数字串(允许首尾空白与句尾标点)-> 逐位阿拉伯数字;否则 None。

    用户场景(2026-10-30 报的):读验证码时说一串数字,期待的是 `12345`,
    拿到的是「一二三四五」。判据只看**整句**:夹在句子中间的数字串一律交给
    提示词规则(见 `_CUES`)—— 猜错一次就是把用户的话改坏。
    """
    if text is None:
        return None
    core = text.strip(_WHOLE_EDGE)
    if len(core) < 2:
        return None
    return _ndigits(core)


def normalize(text):
    """汉字数字 -> 阿拉伯数字。改不动就原样返回。"""
    if not text:
        return text
    t, hold = _protect(text)
    # 整句数字串先判:放在 `_protect` 之后 —— 保护词占位符会打断「整句纯数字」,
    # 所以「一五一十」这类熟语不会被当号码改掉。
    whole = whole_digits(t)
    if whole is not None:
        return whole
    for rx, f in _RULES:
        t = rx.sub(f, t)
    return _restore(t, hold)


# ------------------------------------------------------------------ selftest
CASES = [
    ('二零二五年三月十五日出发', '2025年3月15日出发'),
    ('二零二六年一月一号开始', '2026年1月1号开始'),
    ('三月十五日交', '3月15日交'),
    ('十二月三十一号截止', '12月31号截止'),
    ('我下午三点十五分开会', '我下午3点15分开会'),
    ('九点半到', '9点半到'),
    ('百分之三十七的人同意', '37%的人同意'),
    ('百分之百正确', '100%正确'),
    ('占比百分之三点五', '占比3.5%'),
    ('三十元一份', '30元一份'),
    ('一百块钱', '100块钱'),
    ('三点一四一五九二六', '3.1415926'),
    ('买了三十四个', '买了34个'),
    ('来了十八个人', '来了18个人'),
    ('他今年三十五岁', '他今年35岁'),
    ('一共有三千五百米', '一共有3500米'),
    ('一点小事', '一点小事'),
    ('有一点问题', '有一点问题'),
    ('他十分高兴', '他十分高兴'),
    ('等我一下', '等我一下'),
    ('三天两头跑来', '三天两头跑来'),
    ('一年半载回不来', '一年半载回不来'),
    ('四人帮的事', '四人帮的事'),
    ('一个劲地说', '一个劲地说'),
    ('一两年就好了', '一两年就好了'),
    ('我有一个想法', '我有一个想法'),
    ('这件事我一个人干', '这件事我一个人干'),
    ('一条新消息', '一条新消息'),
    ('两小时后', '两小时后'),
    ('十二分钟就好', '12分钟就好'),
    ('hello world 一二三', 'hello world 一二三'),
    ('没有数字的句子,原样返回。', '没有数字的句子,原样返回。'),
    # --- 逐位数字串(验证码 / 号码),2026-10-30 用户报的那条 ---
    ('一二三四五', '12345'),                        # 整句就是号码
    ('零五零一', '0501'),                           # 前导零要留住
    ('我的验证码是九零三七二一', '我的验证码是903721'),
    ('验证码:一二三四五。', '验证码:12345。'),
    ('电话是幺零零八六', '电话是10086'),             # 幺 = 1
    ('身份证号四四零三零六', '身份证号440306'),
    ('一五一十地说', '一五一十地说'),                # 熟语保护
    ('他数了一遍一二三四五', '他数了一遍一二三四五'),   # 句子中间、又没有提示词:不动
    ('一二三四五六七八九十', '一二三四五六七八九十'),   # 含「十」= 数数,不是号码
    # --- 型号 / 版本 / 域名(2026-10-31 用户实测那三条)---
    ('MP 四', 'MP4'),                                # 型号:拉丁词 + 汉字数字
    ('V 二版本', 'V2版本'),
    ('MP 三', 'MP3'),
    ('GPT 五 出来了吗', 'GPT5 出来了吗'),
    ('AGENTS 点 M D', 'AGENTS.M D'),                 # 「点」夹在 ASCII 之间 → `.`(空格交给合并规则)
    ('SAAAI 点 COM', 'SAAAI.COM'),
    ('三点五', '3.5'),                               # 小数点规则先跑,不受「点」规则影响
    ('下午三点开个会', '下午三点开个会'),             # 只有「点半/点分」才转时刻;裸「点」不动
    ('hello world 一二三', 'hello world 一二三'),      # 常见英文词 + 汉字数字:不动(原用例)
    ('MP 三个', 'MP 三个'),                          # 后面是量词 = 数量,不是型号
    # 全语料回归抓到的假阳性(2026-10-31):拉丁词后面跟的**常用词**不许被当成型号
    ('跟我们 OMP 一样', '跟我们 OMP 一样'),
    ('OMP 一下就好', 'OMP 一下就好'),
    ('这个是 V 二区', '这个是 V2区'),                  # 版本类字(区)才转
]


def _selftest():
    bad = 0
    for src, want in CASES:
        got = normalize(src)
        ok = got == want
        if not ok:
            bad += 1
        print('%s  %-22s -> %-22s %s' % ('ok  ' if ok else 'FAIL', src, got,
                                         '' if ok else '(期望 %s)' % want))
    print('\n%d/%d 通过' % (len(CASES) - bad, len(CASES)))
    return bad


if __name__ == '__main__':
    sys.stdout.reconfigure(encoding='utf-8', errors='replace')
    if len(sys.argv) > 1:
        print(normalize(' '.join(sys.argv[1:])))
    else:
        sys.exit(1 if _selftest() else 0)