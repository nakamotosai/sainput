# -*- coding: utf-8 -*-
"""文本整形 —— 标点策略 + 英文术语合并（从旧 ainput 的三条整形逻辑移植）。

为什么单独一个文件：这两件事都是「不改语义、只改形状」的排版级处理，
和 `polish.py`（去标点模型毛刺）职责不同，混在一起会互相打架。

对照来源：
  · 中文标点转空格 —— 同类实现的用户设置里有「空格替换」这一档
    （用户配置文件里实测存在对应开关）。
  · 英文术语合并 —— 旧 ainput `normalize_personal_english_terms`，但它用 `\\b` 判边界，
    而汉字在 Python 正则里算 word 字符，导致「做R E A任务」匹配不上（只有英文隔开时才生效）。
    这里改用「前后不是 ASCII 字母数字」，中文夹缝里也能生效。

设计原则（和 itn.py 一致）：宁可少改，不可改错。
"""

import re

# ---------------------------------------------------------------- 标点策略

# 中文句读类标点。**问号不在列表里** —— 用户明确要求保留问号
# （他主要把语音输入用在 AI 对话/提示词里，逗号句号碍事，但问号承担语义）。
CJK_PUNCT = '。，、；：！．…‥'
# 全角/半角问号都保留；全角波浪线也不动（常作语气）
_KEEP = '？?'

# 半角标点里，只有「独立成句读」的才转；句点单独处理（见下）。
_HALF_SENTENCE = {'.': None, '!': None, ',': None, ';': None, ':': None}

# 句点：ASCII 的 '.' 只在**不是**小数/版本/域名/省略号时才当句读。
_DOT_NUM = re.compile(r'(?<=\d)\.(?=\d)')          # 3.14
_ABBREV = re.compile(r'[A-Za-z]\.[A-Za-z]')        # a.com / e.g


def _is_ascii_alnum_sandwich(chars, i):
    """标点两边都是 ASCII 字母数字 —— 说明它在英文/数字中间（cd ..、3.14），别动。"""
    prev = chars[i - 1] if i > 0 else None
    nxt = chars[i + 1] if i + 1 < len(chars) else None
    return (prev is not None and prev.isascii() and prev.isalnum()) and \
           (nxt is not None and nxt.isascii() and nxt.isalnum())


def cjk_punct_to_space(t):
    """中文标点 -> 空格（保留问号）。ASCII/数字/URL 里的标点一律不动。

    照搬旧 ainput `cjk_punctuation_to_space`，只把 '？' 从转换表里拿掉。
    """
    chars = list(t)
    out = []
    pending = False
    for i, ch in enumerate(chars):
        if ch in CJK_PUNCT and not _is_ascii_alnum_sandwich(chars, i):
            pending = True
            continue
        if pending:
            # 后面本来就有空白时不重复补，免得凭空多一个空格
            if not ch.isspace() and out:
                out.append(' ')
            pending = False
        out.append(ch)
    return ''.join(out)


# ---------------------------------------------------------------- 英文术语

# 已知的技术词：ASR 常把单个字母分开念，模型就吐成 "A I" / "V P S"。
KNOWN_TERMS = [
    ('A I A I', 'AI'), ('A I', 'AI'),
    ('V P S', 'VPS'),
    ('Code X', 'Codex'), ('code x', 'Codex'), ('CODE X', 'Codex'),
    ('A S R', 'ASR'), ('A P I', 'API'), ('U R L', 'URL'), ('C P U', 'CPU'),
    ('G P U', 'GPU'), ('U I', 'UI'), ('I D', 'ID'), ('O K', 'OK'),
]

# 通用规则:连续 ≥2 个「单字母 + 空格」再跟一个字母 —— 中间夹中文也认。
# 用否定环视代替 \b(汉字旁 \b 不成立,这是旧代码的 bug)。
# 末尾那个字母允许带数字:「M P 四」被 ITN 并成 `M P4` 之后仍要能收成 `MP4`。
_LETTER_GROUP = re.compile(r'(?<![A-Za-z0-9])(?:[A-Za-z] )+[A-Za-z][0-9]*(?![A-Za-z0-9])')


def _group_guarded(s):
    """给 KNOWN_TERMS 的串加「整组边界」:不许是更长字母串的一部分。

    2026-10-31 用户报的漏字:`S A A A I`(saaaai.com)被拆成 `SAA AI` ——
    原因是 `t.replace('A I', 'AI')` 这种**裸子串替换**会命中长串的尾巴:
    `S A A A I` 里那个 `A I` 被替换 → `S A A AI` → 合并规则再也读不完整串。
    判据:左边不能是「字母 + 空格」、右边不能是「(空格)字母」,即它必须自成一组。
    """
    return (r'(?<![A-Za-z] )(?<![A-Za-z])' + re.escape(s) + r'(?! ?[A-Za-z])')


_TERM_RX = [(re.compile(_group_guarded(a)), b) for a, b in KNOWN_TERMS]


def merge_spaced_terms(t):
    """把被拆开的英文缩写粘回去:'R E A' -> 'REA'、'A I和V P S' -> 'AI和VPS'。

    只动「全由单字母组成、用空格分隔」的组;正常英文词(hello world)不受影响,
    因为 'hello' 不是单字母。'a b c' 这种会被粘成 'abc' —— 属可接受(语音里
    逐字母念才会出现这种形态)。
    """
    if not t:
        return t
    for rx, b in _TERM_RX:
        t = rx.sub(b, t)
    return _LETTER_GROUP.sub(lambda m: m.group(0).replace(' ', ''), t)


# ---------------------------------------------------------------- 大小写修复

# 为什么需要:本仓库的默认几个 x-asr 模型**词表里根本没有小写字母** ——
# 实测 tokens.txt 里纯字母 token 231 个,全部大写开头,小写零个;
# 用户日志里 56 个英文词有 55 个是全大写(连 TODAY / IS / AFTER 这种普通词都是)。
# 也就是说模型**物理上吐不出小写**,不是渲染层的 bug。
# 唯一能根治的是换模型(Nemotron 那个有 689 个小写 token),但它 0.6B 更吃算力。
#
# 🔴 设计红线(2026-10-31 按用户口径改):**默认小写**。
# 用户原话:「我现在为止出现过的所有英文几乎全应该是小写的」——
# 所以不再做「只把白名单词改回小写、其余原样保留」那种玩法(它的产物是「满屏大写里
# 夹几个小写词」),而是反过来:
#   · 技术缩写白名单(`KEEP_UPPER`)      → 保持全大写(AI / API / VPS ...)
#   · 专有名词表(`PROPER`)              → 规范混合写法(GITHUB → GitHub)
#   · 其余全大写词                       → 一律小写
#   · 已经是混合大小写的词               → 不动(人写的文本里别改)
#   · 「名字.后缀」整体                  → 见 `_dotted`(域名全小写;文件名的**名字那截保持原样**,
#                                          因为仓库里的 `AGENTS.md` / `README.md` 本来就是大写)

# 明确要保持全大写的(技术缩写 / 专有缩写;这些词没有「规范混合写法」)
KEEP_UPPER = {
    "AI", "API", "URL", "URI", "CPU", "GPU", "RAM", "ROM", "SSD", "HDD", "USB",
    "ASR", "TTS", "LLM", "VPS", "DNS", "HTTP", "HTTPS", "TCP", "UDP", "IP",
    "MAC", "VPN", "SSH", "CLI", "GUI", "OS", "PC", "UI", "UX", "ID", "OK", "PDF",
    "JSON", "XML", "HTML", "CSS", "SQL", "SDK", "IDE", "GPT", "RAG", "NLP", "OCR",
    "CUDA", "ONNX", "GGUF", "IOT", "WASM", "UTF", "ASCII", "REGEX", "TODO", "FIXME",
    "README", "LICENSE", "CHANGELOG", "MAKEFILE", "DOCKERFILE",
    "REA", "OMP", "CPA", "MCP", "ITN", "TSF", "IME", "D2D",
    "MP", "TV", "GPS",       # MP3 / MP4 这类型号:大写是常规写法(数字那截不受影响)
}

# 专有名词 / 品牌 / 工具:全大写出现时还原成规范写法(键一律小写)
PROPER = {
    "github": "GitHub", "gitlab": "GitLab", "gitea": "Gitea", "git": "Git",
    "windows": "Windows", "linux": "Linux", "macos": "macOS", "mac": "Mac",
    "android": "Android", "ios": "iOS", "ubuntu": "Ubuntu", "debian": "Debian",
    "codex": "Codex", "claude": "Claude", "openai": "OpenAI", "chatgpt": "ChatGPT",
    "deepseek": "DeepSeek", "gemini": "Gemini", "grok": "Grok", "kimi": "Kimi",
    "python": "Python", "rust": "Rust", "javascript": "JavaScript",
    "typescript": "TypeScript", "powershell": "PowerShell", "bash": "Bash",
    "docker": "Docker", "kubernetes": "Kubernetes", "nodejs": "Node.js",
    "npm": "npm", "yarn": "Yarn", "bun": "Bun", "deno": "Deno",
    "react": "React", "vue": "Vue", "svelte": "Svelte", "vite": "Vite",
    "pytorch": "PyTorch", "numpy": "NumPy", "pandas": "pandas", "jupyter": "Jupyter",
    "obsidian": "Obsidian", "notion": "Notion", "telegram": "Telegram",
    "discord": "Discord", "slack": "Slack", "wechat": "WeChat", "weixin": "WeChat",
    "tailscale": "Tailscale", "cloudflare": "Cloudflare", "vercel": "Vercel",
    "huggingface": "HuggingFace", "openrouter": "OpenRouter", "photoshop": "Photoshop",
    "blender": "Blender", "godot": "Godot", "unity": "Unity", "unreal": "Unreal",
    "wifi": "WiFi", "iphone": "iPhone", "ipad": "iPad", "macbook": "MacBook",
    "youtube": "YouTube", "whatsapp": "WhatsApp", "onedrive": "OneDrive",
    "chrome": "Chrome", "chromium": "Chromium", "firefox": "Firefox", "safari": "Safari",
    "ipv": "IPv", "vtuber": "VTuber", "whisper": "Whisper", "luma": "Luma",
    "obs": "OBS", "wireshark": "Wireshark", "virtualbox": "VirtualBox", "vmware": "VMware",
    "ytdlp": "yt-dlp", "ffmpeg": "FFmpeg", "ollama": "Ollama", "llama": "LLaMA",
}

# 常见英文词表 —— ⚠️ 现在**不再**用于大小写判断(改口径后全大写一律小写),
# 但 `itn.py` 的「型号 / 版本」规则要用它区分「MP/V/GPT 这种名字」和 hello/world 这种普通词。
COMMON_WORDS = {
    "the", "and", "for", "you", "not", "but", "all", "can", "her", "was", "one",
    "our", "out", "day", "get", "has", "him", "his", "how", "its", "new", "now",
    "old", "see", "two", "way", "who", "boy", "did", "its", "let", "put", "say",
    "she", "too", "use", "that", "with", "have", "this", "will", "your", "from",
    "they", "know", "want", "been", "good", "much", "some", "time", "very", "when",
    "come", "here", "just", "like", "long", "make", "many", "more", "only", "over",
    "such", "take", "than", "them", "well", "were", "what", "work", "year", "back",
    "call", "came", "each", "even", "give", "hand", "high", "keep", "last", "left",
    "life", "live", "look", "made", "most", "move", "must", "need", "next",
    "open", "part", "play", "right", "said", "same", "seem", "show", "side", "tell",
    "turn", "used", "want", "ways", "week", "went", "word", "world", "would", "write",
    "about", "after", "again", "below", "could", "every", "first", "found", "great",
    "house", "large", "learn", "never", "other", "place", "plant", "point", "right",
    "small", "sound", "spell", "still", "study", "their", "there", "these", "thing",
    "think", "three", "under", "water", "where", "which", "while", "white", "whole",
    "years", "young", "monday", "tuesday", "wednesday", "thursday", "friday",
    "saturday", "sunday", "january", "february", "march", "april", "june", "july",
    "august", "september", "october", "november", "december", "today", "tomorrow",
    "yesterday", "hello", "thanks", "please", "sorry", "yes", "no",
    "is", "are", "was", "were", "be", "been", "am", "do", "does", "did", "has",
    "have", "had", "days", "day", "week", "month", "year", "hours", "minutes",
    "seconds", "morning", "evening", "night", "today", "tonight",
}

# 全大写的「词 + 空格 + 词」序列（用于整段判断，尽量别把连写缩写切碎）
_ALPHA_WORD = re.compile(r'(?<![A-Za-z])[A-Za-z]{2,}(?![A-Za-z])')


def fix_english_case(t):
    """全大写英文 → 默认小写;技术缩写保持大写,专有名词还原规范写法。

    口径(2026-10-31 用户钦定):「我出现过的所有英文几乎全应该是小写的」。
    `KEEP_UPPER` / `PROPER` 是**数据**,觉得不对直接改这两张表,不用改代码。"""
    if not t:
        return t

    def rep(m):
        w = m.group(0)
        if w in KEEP_UPPER:
            return w
        canon = PROPER.get(w.lower())
        if canon is not None:
            return canon
        if w.isupper():
            return w.lower()
        return w    # 已经是混合大小写(人写的文本 / 之前还原过的)→ 不动

    return _ALPHA_WORD.sub(rep, t)


# ---------------------------------------------------------------- 「名字.后缀」

_TLD = {
    "com", "cn", "net", "org", "io", "ai", "dev", "app", "me", "co", "jp", "us",
    "uk", "de", "fr", "xyz", "top", "site", "online", "tech", "store", "shop",
    "live", "tv", "cc", "info", "biz", "pro", "club", "art", "blog", "wiki",
    "cloud", "space", "fun", "run", "page", "host", "link", "email", "group",
}
_FILE_EXT = {
    "md", "txt", "json", "yml", "yaml", "toml", "ini", "cfg", "conf", "log",
    "py", "rs", "js", "ts", "sh", "bat", "ps1", "exe", "dll", "so", "lock",
    "zip", "rar", "7z", "pdf", "png", "jpg", "jpeg", "svg", "ico", "webp",
    "html", "css", "xml", "csv", "tsv", "db", "sqlite", "env", "bin", "onnx",
    "whl", "mp3", "mp4", "wav", "ttf", "otf", "woff", "pem", "key", "bak",
}
# 「名字.后缀」整体:`AGENTS.MD` / `SAAAI.COM`。只在后缀在两张表里时才处理。
_DOTTED = re.compile(r'(?<![A-Za-z0-9])([A-Za-z][A-Za-z0-9]*)\.([A-Za-z]{2,10})(?![A-Za-z0-9])')


def dotted_names(t, hold):
    """域名整段小写;文件名只小写后缀、**名字那截保持原样**。

    · `SAAAI.COM`  → `saaaai.com`(顶级域名:整段小写 —— 用户报的 saaaai.com 就是这个)
    · `AGENTS.MD`  → `AGENTS.md`(配置文件:仓库里本来就写作 `AGENTS.md`,名字别动)
    `hold` 是占位符表:文件名的名字那截会换成一个不含字母的哨兵串,
    等大小写处理完再换回来(所以「默认小写」碰不到它)。
    """
    if not t or '.' not in t:
        return t

    def rep(m):
        name, ext = m.group(1), m.group(2)
        low = ext.lower()
        if low in _TLD:
            return (name + '.' + ext).lower()
        if low in _FILE_EXT:
            k = '\x01%d\x02' % len(hold)
            hold[k] = name + '.' + low
            return k
        return m.group(0)

    return _DOTTED.sub(rep, t)


# ---------------------------------------------------------------- 组合入口

def normalize(t, punct_mode='keep', fix_case=True):
    """按策略整形。punct_mode:
    · 'keep'  —— 原样(默认,行为与今天完全一致)
    · 'space' —— 中文标点转空格(保留问号)
    英文术语合并始终执行(它只修 ASR 的拆字,不改语义)。
    fix_case=True 时顺带把常见英文词的全大写改回小写(本仓库模型吐不出小写)。
    """
    if not t:
        return t
    if punct_mode == 'space':
        t = cjk_punct_to_space(t)
    t = merge_spaced_terms(t)
    # 顺序:先合并(`M D` → `MD`),再处理「名字.后缀」,最后才动大小写 ——
    # 文件名的名字那截这时已被换成占位符,不会被「默认小写」误伤。
    hold = {}
    t = dotted_names(t, hold)
    if fix_case:
        t = fix_english_case(t)
    for k, v in hold.items():
        t = t.replace(k, v)
    return t
