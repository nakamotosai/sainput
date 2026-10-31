# -*- coding: utf-8 -*-
"""sainput ASR sidecar —— 流式识别 + 标点 + ITN。全本地，零网络。

协议（stdin/stdout 各一行一个 JSON）：
  in : {"cmd":"audio","pcm":"<base64 i16le mono 16k>"}
       {"cmd":"render","text":"..."}   只跑「ITN + 标点」，不碰 ASR（自检用）
       {"cmd":"finish"}                结束当前话音，出 final
       {"cmd":"reset"}                 丢弃当前会话
       {"cmd":"quit"}
  out: {"type":"ready","model":...,"decoding":...,"punct":bool,"itn":bool,"hotwords":int,
        "twopass":bool,"errors":int}
       {"type":"partial","text":渲染结果,"raw":ASR原文,"ms":渲染耗时,"errors":int}
       {"type":"final","text":渲染结果,"raw":ASR原文,"reason":"endpoint"|"finish","tail_chunks":int}
                                     tail_chunks = 定稿时补解了多少帧(收尾不再限 4 帧/轮)
                                     finish 还会带 "twopass"(本句是否真用了二遍)与 "twopass_ms"
       {"type":"rendered","text":...}
       {"type":"dbg","n":int,"still_ready":bool}   仅在 SAINPUT_PUMP_DEBUG=1 时发(探针用)
       {"type":"error","msg":...,"n":int}          n = 本进程累计错误条数(同类错误 1s 只发一条)

为什么标点内联、不再拆一个进程:CT-Transformer int8 在 ~20 字上 p50 只有 5ms、
80 字 ~21ms,而一帧音频是 32ms,占用可以忽略。

用法: python asr_sidecar.py --model-dir <dir> [--punct-dir <dir>] [--hotwords f.txt]
                            [--threads N] [--beam] [--no-itn] [--throttle-ms N]
                            [--twopass --twopass-model-dir <dir>]
"""
import sys, os, glob, json, base64, argparse, time, traceback, unicodedata

sys.stdout.reconfigure(encoding="utf-8", line_buffering=True)

import numpy as np
import sherpa_onnx

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import itn as itn_mod
import polish as polish_mod
import repair as repair_mod
import textfix as textfix_mod

RATE = 16000


def pick(md, kind):
    for pat in (f"{kind}*.int8.onnx", f"{kind}*.onnx"):
        g = sorted(glob.glob(os.path.join(md, pat)))
        if g:
            return g[0]
    raise SystemExit(f"model dir 里找不到 {kind}*.onnx: {md}")


def emit(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def note(msg):
    """stderr 上的一行说明。写不进去(控制台已 detach / 句柄失效)**绝不能**影响
    识别 —— 上屏永远比日志重要,所以这里吞掉一切异常。"""
    try:
        print(msg, file=sys.stderr, flush=True)
    except Exception:
        pass


# ------------------------------------------------------------------ 兜底报错
# 每条命令的兜底异常 → error JSON。**必须限流**:Rust 收到 error 就 cancel_buffer()
# 取消整段组合;若某类异常每帧必现(比如句柄坏了),原来的行为就是「输入法完全不能用,
# 而且只有隐藏的 stderr 里有日志」。现在同类错误 1s 只报一条,并把累计条数带在
# ready/partial 上 —— 用户至少能看出「它一直在报同一个错」。
_err = {"sig": None, "at": 0.0, "count": 0}


def err_signature(exc):
    """同类错误的指纹:异常类型 + 最内层抛出位置(文件名:行号)。"""
    tb = exc.__traceback__
    where = ""
    while tb is not None:
        where = f"{os.path.basename(tb.tb_frame.f_code.co_filename)}:{tb.tb_lineno}"
        tb = tb.tb_next
    return f"{type(exc).__name__}@{where}"


def report_error(exc=None, sig=None, msg=None):
    """报一条 error 并计数(限流:同一类错误 1s 最多一条)。

    `exc` 走异常路径时必须在 except 块里调用(traceback.format_exc 要读当前异常);
    协议层的错误(bad json / unknown cmd)直接给 sig + msg。
    """
    _err["count"] += 1
    if sig is None:
        sig = err_signature(exc)
    now = time.monotonic()
    if sig == _err["sig"] and (now - _err["at"]) < 1.0:
        return                      # 同类错误 1s 内只报一条,别把 Rust 侧的整句刷掉
    _err["sig"], _err["at"] = sig, now
    if msg is None:
        msg = traceback.format_exc(limit=2)
    if _err["count"] > 1:
        msg += f"(本进程累计第 {_err['count']} 条错误)"
    emit({"type": "error", "msg": msg, "n": _err["count"]})


# ------------------------------------------------------------------ 标点守卫
def content_key(t):
    """正文指纹:去掉空白 / 标点 / 符号,大小写归一。

    用 Unicode 类别判,而不是手写一张标点表 —— 手写的表一定会漏(半角标点、全角
    标点、省略号、波浪号这类符号各算一类),漏一个就会把**正常加了标点的结果**
    误判成「吃掉了正文」,守卫反而把标点丢掉。

    类别 P*(标点)/Z*(分隔)/S*(符号)都不算正文,剩下的(汉字、字母、数字……)
    才是「正文」。标点模型只许动这些,动了就说明它把话吃了。
    """
    out = []
    for c in t:
        if c.isspace():
            continue
        if unicodedata.category(c)[0] in ("P", "Z", "S"):
            continue
        out.append(c)
    return "".join(out).casefold()


def punctuate_guarded(punct, t):
    """跑标点模型,但**验一遍正文有没有缺**。返回 (文本, 是否吃掉了正文)。

    为什么要这道闸:sherpa 的 CT-Transformer 会偶发只回**前一小段** ——
    实测 `我理解不了的你怎么报错了我现在就在 OPEN CODE 里面你居然跟我说必须用
    OPEN CODE 才能用免费模型我现`(59 字)只回了 `我理解不了的,你怎么报错了?`
    (14 字),而只差一个字的邻居输入就正常。直接采信等于把用户刚说的话从屏幕上
    删掉(下游上屏器只会按「文本变短了」去退格),而这条路径**一声不响**。
    标点和热词一样只是加分项:宁可这句不带标点,也不能少一个字。
    """
    if punct is None:
        return t, False
    try:
        out = punct.add_punctuation(t)
    except Exception as e:
        note(f"[sidecar] 标点模型报错,本句退回未加标点的文本: {e}")
        return t, False
    if out and content_key(out) == content_key(t):
        return out, False
    return t, True


def render_text(t, punct=None, use_itn=True, punct_mode="keep",
                merge_terms=True, fix_case=True):
    """ITN -> 标点 -> 排版整形。render() 的纯函数版本(不碰 live 状态,好测)。"""
    if not t:
        return t
    # 顺序有讲究:先去重复循环,再做 ITN 和标点 ——
    # 让标点模型看到的是干净文本,不然它会给打转的片段也加标点。
    t = repair_mod.repair(t)
    if use_itn:
        t = itn_mod.normalize(t)
    # 整句就是一个数字串(读验证码 / 号码):**不加标点**。
    # 标点模型会给纯数字句补一个句号(而且纯数字没有 CJK,polish 还会把它排成英文句点),
    # 上屏就成了「12345.」—— 用户要的是「读验证码直接贴」,多一个句点就贴不进去。
    # 读号码本来也不是句子,断句信息没有任何价值。
    bare_number = t.isascii() and t.isdigit()
    if punct is not None and not bare_number:
        t, dropped = punctuate_guarded(punct, t)
        if dropped:
            note("[sidecar] 标点模型丢了正文,本句退回未加标点的文本")
    t = polish_mod.polish(t)
    # 排版整形放最后:标点已经落定,才轮到「标点转空格」和「英文拆字合并」。
    # 反过来做的话,标点模型会看到被抹掉标点的句子,断句会变差。
    if merge_terms or punct_mode != "keep" or fix_case:
        t = textfix_mod.normalize(t, punct_mode, fix_case=fix_case)
    # 句尾空白一律砍掉:开「标点转空格」时句末的「。」会变成空格(定稿也留着),
    # 上屏就变成「12345 」—— 用户要「读验证码直接贴」,尾巴上多一个空格就贴不进去。
    # 流式阶段也一样:句尾那个空格下一帧还要重算,本来就不该显示。
    return t.rstrip()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model-dir", required=True)
    ap.add_argument("--punct-dir", default=None, help="CT-Transformer 标点模型目录")
    ap.add_argument("--hotwords", default=None, help="热词文件（一行一个词）")
    # 默认 2。**实测增加线程反而更慢**（本机 24 逻辑核）：
    #   1 线程 8.6ms/帧，2 线程 7.5ms，4 线程 10.5ms，12 线程 13.7ms。
    # 这个小模型的线程调度开销大于并行收益，2 是最优区间。
    ap.add_argument("--threads", type=int, default=2)
    ap.add_argument("--beam", action="store_true", help="modified_beam_search（热词需要）")
    ap.add_argument("--no-itn", action="store_true", help="关闭阿拉伯数字归一化")
    ap.add_argument("--punct-mode", default="keep", choices=["keep", "space"],
                    help="标点策略：keep=原样（默认）/ space=中文标点转空格（保留问号）")
    ap.add_argument("--hotwords-extra", default=None,
                    help="额外热词文件（每行一个词）—— 用于上下文偏置：把光标前的词临时喂给识别器")
    ap.add_argument("--no-fix-case", action="store_true",
                    help="不修英文大小写（默认开：本仓库模型词表没有小写字母，常见词会被改回小写）")
    ap.add_argument("--no-merge-terms", action="store_true",
                    help="关闭英文拆字合并（默认开：'R E A' -> 'REA'）")
    ap.add_argument("--rule1", type=float, default=10.0,
                    help="还没识别出任何字时的静音断句阈值（秒）")
    ap.add_argument("--rule2", type=float, default=10.0,
                    help="已经识别出字之后的静音断句阈值（秒）。调大 = 句中停顿不容易被切句")
    ap.add_argument("--rule3", type=float, default=20.0,
                    help="最长一句（秒），超了就强制断句")
    ap.add_argument("--blank-penalty", type=float, default=0.0,
                    help="压重复循环：>0 让空白更可能出现，循环会被打断")
    ap.add_argument("--throttle-ms", type=int, default=150,
                    help="流式渲染节拍:至少隔这么久才重算标点并刷新屏幕")
    # 二遍重打:流式照旧用主模型(保首字手感),松键后用更准的模型把这**一整句**
    # 重解一遍覆盖 final。默认关 —— 不传 --twopass 就**不加载**二遍模型,启动不拖慢。
    ap.add_argument("--twopass", action="store_true",
                    help="松键后用 --twopass-model-dir 的模型对本句整段音频重解一遍")
    ap.add_argument("--twopass-model-dir", default=None,
                    help="二遍用的模型目录(只有 --twopass 时才加载)")
    a = ap.parse_args()
    if a.twopass and not a.twopass_model_dir:
        raise SystemExit("--twopass 必须配 --twopass-model-dir")

    # ---- 热词 ----------------------------------------------------------
    n_hot = 0
    if a.hotwords:
        if not os.path.exists(a.hotwords):
            raise SystemExit(f"热词文件不存在: {a.hotwords}")
        with open(a.hotwords, encoding="utf-8") as f:
            n_hot = sum(1 for ln in f if ln.strip() and not ln.startswith("#"))
        if n_hot:
            a.beam = True    # 热词只在 modified_beam_search 下生效；空表就别白白慢下来

    # ---- ASR -----------------------------------------------------------
    # 热词要能被 sherpa-onnx 编码成 token，必须配对正确的 `modeling_unit` / `bpe_vocab`：
    #   · 有 `bpe.vocab` 的模型 → modeling_unit=bpe + bpe_vocab=<那个文件>
    #   · 只有 `bpe.model`（没有 .vocab）的模型 → 仍然走 bpe，但 bpe_vocab 要给 **tokens.txt**
    #     ← 这是本仓库默认模型（x-asr-160ms）的情况。2026-10-17 实测：
    #       不给 bpe_vocab（原来的行为）会退回 cjkchar，然后 sherpa 报
    #       `Cannot find ID for token …` 并 **skipping** —— 热词**静默失效**，不报错、不抛异常。
    #       也就是说在修好这一处之前，`hotwords.txt` 对这个默认模型一直是**没生效的**。
    #   · cjkchar 只适合「tokens.txt 里就是单个汉字」的老式中文模型。
    #
    # 抽成函数是因为**二遍模型是另一个目录**(见 --twopass):词表和 bpe 是跟着模型目录走的,
    # 拿主模型的 tokens 去喂二遍模型 = 同一类静默失效。
    def hotword_cfg_for(md):
        toks = os.path.join(md, "tokens.txt")
        bv = os.path.join(md, "bpe.vocab")
        if os.path.exists(bv):
            return "bpe", bv
        if os.path.exists(os.path.join(md, "bpe.model")) and os.path.exists(toks):
            return "bpe", toks
        return "cjkchar", ""

    hw_unit, hw_vocab = hotword_cfg_for(a.model_dir)

    hw = a.hotwords if (a.hotwords and n_hot) else ""
    # 上下文偏置会**按句**给 per-stream 热词，所以只要可能在运行时用热词
    # （静态热词表非空 · 或前端配了偏置文件 · 或 --beam），识别器就必须**建在
    # modified_beam_search + 正确的 BPE 分词配置上**。
    # 否则 per-stream 热词会撞上不是 BPE 的识别器 → sherpa `Cannot find ID for token` → 静默跳过。
    want_hotwords = bool(hw) or bool(a.hotwords_extra) or bool(a.beam)

    def build(hw_file, method, md=None):
        """按某个模型目录建识别器。md 省略 = 主模型;二遍重打会传二遍目录。"""
        md = md or a.model_dir
        unit, vocab = (hw_unit, hw_vocab) if md == a.model_dir else hotword_cfg_for(md)
        extra = {}
        if want_hotwords:
            # 即使这次没有静态热词文件，也把 BPE 分词配置带上 —— per-stream 热词要用它。
            extra["modeling_unit"] = unit
            if vocab:
                extra["bpe_vocab"] = vocab
        if hw_file:
            extra["hotwords_file"] = hw_file
            extra["hotwords_score"] = 1.5
        return sherpa_onnx.OnlineRecognizer.from_transducer(
            tokens=os.path.join(md, "tokens.txt"),
            encoder=pick(md, "encoder"),
            decoder=pick(md, "decoder"),
            joiner=pick(md, "joiner"),
            num_threads=a.threads,
            sample_rate=RATE,
            feature_dim=80,
            decoding_method=method,
            blank_penalty=a.blank_penalty,
            # ⚠️ 内置 endpoint 关掉 —— 它的门槛是**构造参数**，改一次要重建识别器（重载模型几秒）。
            # 用户要求托盘里能随时换 5/10/15/30 秒，所以改由我们自己按帧计时判定。
            # 语义保持不变：连续静音超过 threshold 就定稿；`--rule1/--rule2` 仍然生效，
            # 作为启动时的初始值（托盘改了会覆盖 live 值）。
            enable_endpoint_detection=False,
            rule1_min_trailing_silence=a.rule1,
            rule2_min_trailing_silence=a.rule2,
            rule3_min_utterance_length=a.rule3,
            provider="cpu",
            **extra,
        )

    method = "modified_beam_search" if (a.beam or hw or a.hotwords_extra) else "greedy_search"
    try:
        rec = build(hw, method)
    except Exception as e:
        if not hw:
            raise
        # 热词只是加分项,绝不能因为它把整个语音输入搞挂。
        # (走 note() 而不是裸 print:句柄失效时裸 print 抛 OSError,会被外层兜底捕获
        #  → 发 error JSON → Rust 直接 cancel_buffer()。日志绝不能反噬上屏。)
        note(f"[sidecar] 热词加载失败,本次不带热词继续: {e}")
        hw = ""
        n_hot = 0
        method = "modified_beam_search" if a.beam else "greedy_search"
        rec = build("", method)

    # ---- 标点 ----------------------------------------------------------
    punct = None
    if a.punct_dir:
        if not os.path.isdir(a.punct_dir):
            raise SystemExit(f"标点模型目录不存在: {a.punct_dir}")
        cands = glob.glob(os.path.join(a.punct_dir, "model*.onnx"))
        if not cands:
            raise SystemExit(f"标点模型目录里没有 model*.onnx: {a.punct_dir}")
        cands.sort(key=lambda p: (".int8" not in p, p))   # 优先 int8
        cfg = sherpa_onnx.OfflinePunctuationConfig(
            model=sherpa_onnx.OfflinePunctuationModelConfig(
                ct_transformer=cands[0], num_threads=1
            )
        )
        punct = sherpa_onnx.OfflinePunctuation(cfg)

    # ---- 二遍重打(可选,默认关)----------------------------------------
    # 流式阶段仍然用主模型(保 960ms 首字手感);松键(finish)时才用这里的模型把
    # **本句整段音频**重解一遍,用更准的结果覆盖 final。
    # 依据(实测):同仓库的 480ms 档整段解码明显更准
    #   `TODAY AFTER TOMORROW` → `THE DAY AFTER TOMORROW`、`时态先上去` → `时态写上去`;
    # 代价:10s 语音约 +1.0s(实测整段解码 480ms 档 0.97s / 960ms 档 0.70s)。
    #
    # 用 **OnlineRecognizer** 而不是 OfflineRecognizer:480ms 档是**流式**模型
    # (encoder 带流式状态),拿 OfflineRecognizer 加载它会直接把 python 进程打崩
    # (2026-10-31 实测:进程退出码 255)。整段喂给流式识别器 + input_finished,
    # 走的 chunk 序列与流式完全一致,只是不再边收边出。
    tp = None
    if a.twopass:
        if not os.path.isdir(a.twopass_model_dir):
            # 目录不对也**不退出**(退出 = 整个语音输入没了):安静地退回单遍。
            note(f"[sidecar] 二遍模型目录不存在,--twopass 本次失效: {a.twopass_model_dir}")
        else:
            try:
                tp = build(hw, method, md=a.twopass_model_dir)
            except (Exception, SystemExit) as e:
                # 二遍只是「更准」的加分项:加载不了就一路退化回流式结果,绝不因此不出字。
                # (pick() 找不到 onnx 时抛的是 SystemExit —— 它不算 Exception,得单独接住。)
                note(f"[sidecar] 二遍模型加载失败,--twopass 本次失效: {e}")
                tp = None

    # 本句 PCM:二遍要重解**整段音频**,所以流式阶段必须在内存里留一份。
    # 16k float32 = 64KB/秒,10 秒一句才 640KB,句末(reset_stream)立刻释放 ——
    # 但**必须有上限**:VAD 断句是兜底(阈值可被托盘调大),不能让它无限攒着。
    # 超上限就把本句标记成放弃二遍,安静退化,不丢句。
    PCM_MAX = int(RATE * 180)
    pcm = {"buf": [], "n": 0, "skip": False}

    def pcm_clear():
        pcm["buf"].clear()
        pcm["n"] = 0
        pcm["skip"] = False

    def pcm_push(x):
        if tp is None or pcm["skip"]:
            return          # 没开二遍(或本句已放弃):一个字都不留
        if pcm["n"] + len(x) > PCM_MAX:
            note("[sidecar] 本句超过 180 秒,放弃二遍重打")
            pcm_clear()
            pcm["skip"] = True
            return
        pcm["buf"].append(x)
        pcm["n"] += len(x)

    def twopass_decode():
        """用二遍模型把本句整段音频重解一遍。返回 (原文 或 None, 耗时ms)。"""
        if tp is None or pcm["skip"] or pcm["n"] <= 0:
            return None, 0.0
        x = np.concatenate(pcm["buf"]) if len(pcm["buf"]) > 1 else pcm["buf"][0]
        t0 = time.perf_counter()
        try:
            s = tp.create_stream()
            s.accept_waveform(RATE, x)
            s.input_finished()
            # 整段喂完再解:这里不用限流式那个 4 帧上限 —— 本来就在等结果,
            # 一次解完(10 秒音频 = 20 个 chunk)。
            n = 0
            while tp.is_ready(s) and n < 100000:
                tp.decode_stream(s)
                n += 1
            raw = (tp.get_result(s) or "").strip()
        except Exception as e:
            # 失败必须降级:宁可这句用流式结果,也不能把整句弄丢。
            note(f"[sidecar] 二遍重打报错,本句沿用流式结果: {e}")
            return None, (time.perf_counter() - t0) * 1000.0
        return (raw or None), (time.perf_counter() - t0) * 1000.0

    use_itn = not a.no_itn
    # 运行中可改的策略（托盘开关会推过来）。用可变容器而非裸变量，
    # 这样内层函数能直接改，不必到处 nonlocal。
    live = {"punct_mode": a.punct_mode, "merge_terms": not a.no_merge_terms,
            "ctx_bias": True, "fix_case": not a.no_fix_case,
            # 静音多少秒算一句说完（托盘可改，立刻生效）。0 = 不断句，只认松手。
            "silence_sec": float(a.rule2)}

    # 标点:每一拍都整句重算(和 2026-10-30 之前的行为一致)。
    #
    # ⚠️ 这里曾经按 500ms 节拍降频 + 复用前缀标点(2026-10-31 体检的「省 CPU」那一项)。
    # 当天就被用户反馈打回来了,实测数据(同一段 8.8s 中英混说音频,各测 2~3 次,结果稳定):
    #
    #   500ms 节拍(降频)          → 退格 24 次
    #   每拍重算(本行)            → 退格 **15** 次
    #
    # 原因是「越晚改越贵」:标点模型会在**句中**插一笔(实测那例是在第 11 个字后面插了个「?」),
    # 插一笔会让后面**整条尾巴位移**,目标窗口上只能退格重打整条尾巴 —— 而重打的长度
    # 等于「插入点之后的尾巴长度」。降频让这一笔更晚落,尾巴更长,重打更贵,用户看到的就是
    # 「回退后重新输入有点慢」。降频省下的 CPU 只有几十毫秒/句(约 0.5% 单核),不划算。
    #
    # 定稿(final)一如既往整句重算,不受任何节拍限制。
    def render(t, final=False):
        """ITN -> 标点 -> 排版整形。返回 (文本, 耗时ms)。"""
        if not t:
            return "", 0.0
        t0 = time.perf_counter()
        out = render_text(t, punct, use_itn,
                          live["punct_mode"], live["merge_terms"], live["fix_case"])
        return out, (time.perf_counter() - t0) * 1000.0

    emit({
        "type": "ready",
        "model": os.path.basename(a.model_dir.rstrip("\\/")),
        "decoding": method,
        "punct": punct is not None,
        "itn": use_itn,
        "hotwords": n_hot,
        "twopass": tp is not None,
        "errors": _err["count"],
    })

    # ---- 上下文偏置 -----------------------------------------------------
    # 前端（sainput 主循环）会把「光标前最近的词」写进这个文件；每句开始时读一次，
    # 作为**该句专属**的热词喂给识别器（sherpa-onnx 的 create_stream 支持 per-stream 热词）。
    # 为什么用热词而不是改解码器：热词能在线生效、不需要重启 sidecar，也天然是"偏置"而不是"硬替换"。
    # ⚠️ 这是**本地**偏置（喂热词），不是把历史文本编码上传的云端偏置。
    def read_ctx_hotwords():
        # 托盘里关掉上下文偏置后，这一句就不再喂热词（文件可能还在，但不再读）
        if not live.get("ctx_bias", True):
            return None
        if not a.hotwords_extra:
            return None
        try:
            if not os.path.exists(a.hotwords_extra):
                return None
            with open(a.hotwords_extra, encoding="utf-8") as f:
                words = [ln.strip() for ln in f
                         if ln.strip() and not ln.lstrip().startswith("#")]
            if not words:
                return None
            # sherpa-onnx 的 per-stream 热词就是空格分隔的一行
            return " ".join(words)
        except Exception as e:
            note(f"[sidecar] 上下文热词读取失败(不影响识别): {e}")
            return None

    # 关键：流要**惰性**创建（在收到这一句的第一帧音频时才建），不能在上句 finish 后就建。
    # 因为 Rust 侧是在「按下热键开始新一句」那一刻才写偏置文件的；如果 sidecar 在上句
    # finish 时就建流，读到的还是上一轮的旧文件 —— 偏置会整整晚一句生效（静默失效）。
    _stream = [None]
    _ctx_loaded = [0]

    def new_stream():
        """立刻建一条流（读当前偏置文件）。"""
        hw_str = read_ctx_hotwords()
        if hw_str:
            try:
                s = rec.create_stream(hotwords=hw_str)
                note(f"[sidecar] 上下文偏置: 送入 {len(hw_str.split())} 个词")
                return s
            except Exception as e:
                note(f"[sidecar] 上下文热词注入失败(退回普通流): {e}")
        return rec.create_stream()

    def ensure_stream():
        """惰性：本句还没有流就现在建（这时偏置文件已是最新）。"""
        if _stream[0] is None:
            _stream[0] = new_stream()
        return _stream[0]

    # ⚠️ 启动时**不建**流:见上面注释 —— 此刻偏置文件还是上一轮的,建了也用不上,
    #    白白多建一条(下一条第一帧还会再建一次),只会晚一句生效。
    stream = None
    last_raw = ""       # 上一次 ASR 原文(已渲染/已发出去的那一版)
    last_out = ""       # 上一次渲染后、已经发给前端的文本
    last_tick = 0.0
    # 连续静音时长（秒）。内置 endpoint 已关，断句由我们自己按这个计时判。
    # ⚠️ 必须在循环**之前**初始化：早先漏了这行，`silence_sec += dur` 抛 NameError，
    #    被外层 except 吞掉 —— 识别照常，但静音断句**从来没生效过**，而且没有任何日志。
    silence_sec = 0.0
    # 上一次 pump 实际解了多少帧 —— finish 用它报告定稿时解码是否落后过。
    pump_last_decodes = [0]

    def is_silent_frame(x):
        """按 RMS 判这一帧算不算安静。阈值与 Rust 侧 `tail_rms` 同量级。"""
        if len(x) == 0:
            return True
        rms = float(np.sqrt(np.mean(np.square(x))))
        return rms < 0.012

    def pump(force=False, drain_all=False):
        """吃音频 -> 出 partial。force=True 时忽略节流(final 用)。

        drain_all=True 时**不限每轮解码帧数**:只在定稿(finish)用。
        """
        nonlocal last_raw, last_out, last_tick, stream
        stream = ensure_stream()      # 本句第一帧到这里才建流 —— 此时偏置文件已是最新
        # 一次 pump 最多解这么多帧,剩下的留给下一轮 —— **只限流式显示的轮次**。
        #
        # 为什么必须限:单帧解码平均只要 8~10ms,但实测约 20% 的帧会突刺到
        # 40~95ms(sherpa 内部的内存分配/beam 搜索抖动,不是文本变长导致的 ——
        # 0~9 字和 30~39 字的平均耗时都是 10ms)。
        # 不限的话,一帧突刺 → 音频积压 → 下一轮 while 连解好几帧 →
        # 又一次突刺...... 雪球越滚越大,用户看到的就是卡住然后一次全出来。
        # 限量后积压被摊平到多轮,画面是持续小步更新,不会长时间静默。
        #
        # ⚠️ 但**收尾(finish)不能限量**:解码一旦落后(长句、机器忙、模型慢),
        # 限量就等于把还没解的尾巴留在流里,`get_result` 吐出的就是缺尾巴的句子。
        # 所以定稿走 drain_all=True,一次把剩下的全解完 —— 这时本来就没人看画面了。
        _MAX_DECODE_PER_PUMP = 4
        _limit = (1 << 30) if drain_all else _MAX_DECODE_PER_PUMP
        _n = 0
        while _n < _limit and rec.is_ready(stream):
            rec.decode_stream(stream)
            _n += 1
        pump_last_decodes[0] = _n
        if os.environ.get("SAINPUT_PUMP_DEBUG"):
            # 只在调试时开(probe_tail.py 会带上这个环境变量):让探针能看见
            # 「这一轮解了几帧 / 解完还剩没解完的帧」,用来判断积压。
            emit({"type": "dbg", "n": _n, "still_ready": bool(rec.is_ready(stream))})
        raw = rec.get_result(stream)
        if raw == last_raw and not force:
            return raw
        now = time.monotonic()
        if not force and (now - last_tick) * 1000.0 < a.throttle_ms:
            # 还没到节拍,先不刷新屏幕。
            # ⚠️ 这里**绝不能**更新 last_raw(体检报告 §7):被节流的这一版是「延后」,
            # 不是「丢」—— 下一拍它还要重新渲染上屏。早先把 last_raw 写在节流判断之前,
            # 那一版就被记成「已处理」,只要 ASR 不再变它就永远上不了屏
            # (throttle 调大即显形)。
            return raw
        last_tick = now
        last_raw = raw      # 只有真正走完渲染、这一版才算「已处理」
        out, ms = render(raw, final=force)
        if not force:
            # 流式阶段不显示句尾还没定稿的标点（否则屏幕上一闪一闪）。
            # 但如果用户开了「标点转空格」，句读本来就应该显示成空格 ——
            # 这时直接剥掉会让屏幕上看到「…是怎么回事尤其是」（词全粘住），
            # 和用户要的效果正好相反。所以那种模式下把标点位置换成空格。
            out = polish_mod.strip_terminal(out, as_space=(live["punct_mode"] == "space"))
        if not out:
            return raw          # 没东西可显示就别发,免得下游把屏上的字擦掉
        if force or out != last_out:
            last_out = out
            emit({"type": "partial", "text": out, "raw": raw, "ms": round(ms, 2),
                  "errors": _err["count"]})
        return raw

    def reset_stream():
        nonlocal stream, last_raw, last_out, last_tick
        # **不要**在这里建流 —— 见 new_stream 上方注释:此刻偏置文件还是上一轮的,
        # 真建了就整整晚一句生效。这里只标记"本句无流",等第一帧音频到了再惰性建。
        _stream[0] = None
        stream = None
        last_raw = ""
        last_out = ""
        last_tick = 0.0
        pcm_clear()                         # 本句 PCM 用完即释放(二遍只重解本句)

    for raw in sys.stdin.buffer:
        raw = raw.strip()
        if not raw:
            continue
        try:
            msg = json.loads(raw)
        except Exception as e:
            report_error(sig="bad json", msg=f"bad json: {e}")
            continue
        cmd = msg.get("cmd")
        try:
            if cmd == "audio":
                b = base64.b64decode(msg.get("pcm") or "")
                if b:
                    x = np.frombuffer(b, dtype="<i2").astype("float32") / 32768.0
                    stream = ensure_stream()
                    stream.accept_waveform(RATE, x)
                    pcm_push(x)         # 二遍重打要用整段音频(没开 --twopass 时是空操作)
                    pump()
                    # 自己判静音时长（内置 endpoint 已关，见上面注释）
                    dur = len(x) / float(RATE)
                    if is_silent_frame(x):
                        silence_sec += dur
                    else:
                        silence_sec = 0.0
                    threshold = float(live.get("silence_sec") or 0.0)
                    if threshold > 0 and silence_sec >= threshold:
                        # VAD 自动断句：这一句定稿，复位等下一句。
                        #
                        # 与同类产品的取舍（2026-10-17 按用户反馈调整）：
                        #   · 有的产品全程**没有**「静音 N 秒就断句」—— 只有松手才 end_audio；
                        #   · 也有的产品是「松键 fast-stop / VAD / no-speech timeout」三选一，
                        #     且 VAD 是服务端语义判断，不是纯计时。
                        # 用户的实际抱怨是「说着说着要想很久」，所以纯计时断句对他不合适。
                        # 现在 rule1/rule2 默认拉到 10 秒（--rule1/--rule2 可调），
                        # 让 VAD 退化成**兜底**（防止忘了松手一直录），而不是主要断句手段。
                        pump(force=True, drain_all=True)
                        emit({"type": "final", "text": last_out, "raw": last_raw,
                              "reason": "endpoint", "tail_chunks": pump_last_decodes[0]})
                        # 与 finish 完全一致的复位(体检报告 §7):原来只调 rec.reset(stream),
                        # 那条流还留着本句的 per-stream 热词偏置,而且下一句还会沿用
                        # 上一句的节拍/上屏状态。reset_stream 是「整条流丢掉、下一帧再惰性建」,
                        # 顺带把本句 PCM 释放掉(二遍只重解本句)。
                        # ⚠️ 二遍重打**不**在这里做:此刻用户还在说,同步重解 1s 会卡住
                        # 音频回路;它只在 finish(已松键)时跑。
                        reset_stream()
                        silence_sec = 0.0
            elif cmd == "pad":
                # 一次性灌入「房间底噪」当静音：喂饱模型的右上下文，
                # 让最后一个字吐出来（前端就不用再干等 600ms 实时音频）
                ms = int(msg.get("ms") or 0)
                rms = float(msg.get("rms") or 0.003)
                n = int(RATE * ms / 1000.0)
                if n > 0:
                    noise = np.random.normal(0.0, max(rms, 0.0005), n)
                    noise = np.clip(noise, -1.0, 1.0).astype(np.float32)
                    stream = ensure_stream()
                    stream.accept_waveform(RATE, noise)
                    pcm_push(noise)     # 这段底噪也是本句音频的一部分(二遍要重解同样的输入)
                    pump()
                emit({"type": "padded", "ms": ms})
            elif cmd == "finish":
                silence_sec = 0.0
                stream = ensure_stream()
                stream.input_finished()
                # 定稿:把流里剩下的帧**全部**解完再取结果。见 pump 里那段注释 ——
                # 流式的每轮 4 帧限量用在收尾上就是丢尾巴。
                pump(force=True, drain_all=True)
                # 二遍重打:松键了,用更准的模型把**本句整段音频**重解一遍覆盖 final。
                # 失败/没开 → tp_used=False,last_out/last_raw 保持流式结果(安静降级)。
                tp_used, tp_ms = False, 0.0
                if tp is not None:
                    raw2, tp_ms = twopass_decode()
                    if raw2:
                        # 二遍结果同样走完整渲染(标点/ITN):final=True 保证精度,
                        # 不做流式那套标点节拍与「复用上一次结果」的近似。
                        out2, _ = render(raw2, final=True)
                        if out2:
                            last_raw, last_out = raw2, out2
                            tp_used = True
                        else:
                            note("[sidecar] 二遍结果渲染为空,本句沿用流式结果")
                emit({"type": "final", "text": last_out, "raw": last_raw,
                      "reason": "finish", "tail_chunks": pump_last_decodes[0],
                      "twopass": tp_used, "twopass_ms": round(tp_ms, 1)})
                reset_stream()
            elif cmd == "render":
                # 自检用的纯渲染:要精度,不受流式标点节拍限制。
                out, ms = render(msg.get("text") or "", final=True)
                emit({"type": "rendered", "text": out, "ms": round(ms, 2)})
            elif cmd == "set":
                # 托盘开关推送：立刻改策略，不用重开 sidecar。
                # key 用的是 conf 里的键名（punct_mode / merge_terms / ctx_bias），
                # Rust 侧直接把 key 字符串发过来，这里翻译成内部状态。
                k = (msg.get("key") or "").strip()
                v = msg.get("value")
                if k == "punct_mode":
                    live["punct_mode"] = str(v) if v is not None else a.punct_mode
                elif k == "merge_terms":
                    live["merge_terms"] = bool(v) if v is not None else (not a.no_merge_terms)
                elif k == "silence_sec":
                    try:
                        live["silence_sec"] = float(v)
                    except (TypeError, ValueError):
                        pass
                elif k == "fix_case":
                    live["fix_case"] = bool(v) if v is not None else True
                elif k == "ctx_bias":
                    live["ctx_bias"] = bool(v) if v is not None else True
                emit({"type": "set_ack", "key": k, "ok": True})
            elif cmd == "reset":
                reset_stream()
                emit({"type": "reset", "ok": True})
            elif cmd == "quit":
                break
            else:
                report_error(sig=f"unknown cmd {cmd}", msg=f"unknown cmd {cmd}")
        except Exception as e:
            report_error(e)


if __name__ == "__main__":
    main()