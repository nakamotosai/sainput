# sainput —— 低延迟、全本地的语音输入法

**Windows 10/11 x64** · **MIT License** · **Rust 主体 + Python ASR 子进程** · 版本 `0.2.4`

按住热键说话，松开上屏；文字在说话过程中逐字出现，自带标点、阿拉伯数字归一化和热词。
识别与标点全部在本机完成，**运行期不联网**。

```
双击 start.bat
→ 光标放进能打字的窗口（记事本 / 浏览器 / 微信 / VSCode）
→ 按住【右 Ctrl】说话，松开
→ 文字逐字上屏
→ 托盘图标常驻；退出走「右键托盘 → 退出 sainput」
```

> 用 `start.bat` 启动时控制台会自动收起（脚本带了 `--hide-console`）。想留一个调试窗口，
> 直接跑 `sainput run`（不带 `--hide-console`）即可。

---

## 1. 它现在能做什么

| 能力 | 状态 |
|---|---|
| 中英混说流式识别（逐字上屏） | ✅ 默认开 |
| 自动标点（，。？！） | ✅ 默认开，中英通用 |
| 阿拉伯数字归一化（「三十四个」→「34个」） | ✅ 默认开 |
| 悬浮显示（4 选 1，默认屏幕边缘光） | ✅ 见 §4 |
| 静音计时断句（RMS 能量阈值，非语义 VAD） | ✅ 默认 15 秒，托盘可选 5/10/15/30/不断句 |
| 热词（人名 / 术语 / 缩写） | ✅ `hotwords.txt`（缺失不会报错） |
| 上下文偏置（记住光标前刚打的词） | ✅ 默认开，灵感来自本地输入法的通行做法 |
| 英文拆字合并（`A P I` → `API`）、英文大小写修复 | ✅ 默认开 |
| 标点转空格（中文标点 → 空格，问号保留） | ✅ 托盘 / `--punct-mode space` |
| 配置文件（不用改 bat） | ✅ `sainput.conf` |
| 按一下开始 / 再按一下结束 | ✅ `toggle = true`（**默认是按住说话**） |
| 单实例保护 / 托盘常驻 / 开机自启 | ✅ |
| 纯离线 | ✅ 装完就不再需要网络 |

**实测数据**（测试机，160ms 模型 + 标点，实时喂帧）：

| 指标 | 实测 |
|---|---|
| 采集帧节奏 | 期望 32ms，实测 p50=30.0ms / max=40.4ms，丢帧 0 |
| 识别吞吐 | RTF 0.063–0.076（远快于实时） |
| 标点 + ITN 渲染 | p50 5.3ms，80 字 21ms（一帧是 32ms） |
| 起说后首个字上屏 | 约 770–860ms（模型 chunk 决定，见 §7） |
| 上屏准确性 | 真注入 Notepad 后逐字读回，与 final 完全一致 |

---

## 2. 系统要求与安装

| 项 | 要求 |
|---|---|
| 系统 | Windows 10 / 11 **x64**（仅此二者；无 ARM64 / macOS / Linux 版本） |
| Rust | 1.91+（`rust-version = "1.91"`）。MSVC 工具链或 `x86_64-pc-windows-gnu` + w64devkit 均可 |
| Python | **3.11+，64 位**。`pip install -r requirements.txt`（`sherpa-onnx` 1.13.4 / `numpy` 2.x） |
| 模型 | 需自行下载（本仓库不含权重）：`powershell -File scripts/fetch_models.ps1` |
| 磁盘 | 约 250 MB（默认 ASR ≈163 MB + 标点 ≈76 MB） |

从零开始：

```powershell
git clone <repo-url> sainput
cd sainput

# 1) Python 依赖（ASR sidecar 用）
pip install -r requirements.txt

# 2) 拉模型（默认 ASR + 标点；不覆盖已有目录）
powershell -ExecutionPolicy Bypass -File scripts/fetch_models.ps1

# 3) 编译（GNU 工具链示例；用 MSVC 的话见 §9）
build.bat build --release

# 4) 跑起来
start.bat
```

`start.bat` 只是启动编译好的 `sainput.exe`，不会留窗口；托盘图标才是它在跑的标志。
想直接看启动参数，在 `target\<triple>\release\` 下跑 `sainput.exe run`。

### 配置从哪来

仓库根下的 `sainput.conf` **不随仓库分发**（它属于个人偏好）。第一次使用：

```powershell
copy sainput.conf.example sainput.conf
copy hotwords.example.txt hotwords.txt    # 可选
```

两个文件都不存在时也能跑，所有项走内置默认值。

跑离线打表 / 推理脚本（`tools/bench_*.py`、`tools/probe_stream.py`）另需：
`pip install -r requirements-dev.txt`（只多一个 `soundfile`，运行 sainput 本身不需要）。

更多文档：[模型选型与延迟实测](docs/model-selection.md) ·
[排查手册](docs/TROUBLESHOOTING.md) · [模型目录说明](models/README.md)。

---

## 3. 用起来

| 操作 | 做法 |
|---|---|
| 说话 | 按住热键（默认 **右 Ctrl**），说完松开 |
| 换热键 | 托盘 → 换热键：鼠标侧键 X1 / X2、右 Ctrl、右 Alt、F8、F9、F10；配置文件里还认左右 Shift / Ctrl / Alt、CapsLock、反引号、`0xXX` 虚拟键码与鼠标中键，例如 `hotkey = f9`、`hotkey = mousex1`、`hotkey = mousemiddle` |
| 开始/结束各按一下 | `toggle = true`，或 `--toggle` |
| 退出 | 右键托盘图标 → 退出 sainput |
| 键盘卡住（按键没弹起） | 托盘 → 「释放卡住的键」，或命令行 `sainput release-keys` |

托盘菜单里可实时切换（改完立刻生效，并写回 `sainput.conf`）：

- 换热键
- 标点转空格（保留问号）
- 英文连写（`A P I` → `API`）
- 上下文偏置 / 显示偏置提示
- 前面自动加空格
- 悬浮显示：不显示 / 细条 / 悬浮球 / 屏幕边缘光（**4 选 1**，默认屏幕边缘光），边缘光颜色 5 选 1
- 英文小写修复
- 开机自启
- 静音断句：5 / 10 / 15 / 30 秒 / 不断句（**默认 15 秒**）
- 释放卡住的键 / 显示日志窗口 / 退出

---

## 4. 悬浮显示

**位置固定**：细条 / 悬浮球在屏幕中下方、任务栏正上方；边缘光沿屏幕四边。
两者都**不跟随光标**（不挡正在看的插入符）。

| 模式 | 说明 |
|---|---|
| 不显示 | 完全不画（文字本来就逐字上屏到光标处） |
| 细条（`bar`） | 只显示实时文字 |
| 悬浮球（`orb`） | 录音状态 + 真实音量驱动的波形 |
| 屏幕边缘光（`edge`，**默认**） | 录音时屏幕四边微亮，松手淡出，不占中央 |

光色 5 选 1：白光（默认）/ 彩虹光 / 暖橙光 / 青蓝光 / 极光（青紫）。
配置写法：`overlay_mode = off|bar|orb|edge`、`edge_tint = white|rainbow|amber|cyan|aurora`。

预览（不用真录音）：

```powershell
sainput overlay-demo --mode bar --seconds 10
sainput overlay-demo --mode orb --seconds 6
sainput overlay-demo --mode edge --tint aurora
```

---

## 5. 配置

`sainput.conf` 一行一个 `key = value`，`#` / `;` 开头是注释。**命令行参数优先于配置文件**。

> ⚠️ 下面是**示例值，不是当前生效值**。真实生效值用 `sainput config` 看；仓库里的
> `sainput.conf.example` 同样是示例。改完重启 `run` 生效（托盘里的开关例外，点了就生效）。
> 注释必须**独占一行**：解析器不剥行内 `#`，`hotkey = f9  # 注释` 会把整串当值。
> `sainput.conf.example` 因而采用「说明写在上方，下一行是裸的 `# key = value`」的写法。

```ini
# 热键：rightctrl（默认）/ leftctrl / rightalt / leftalt / rightshift / leftshift /
#       capslock / f8 / f9 / f10 / backquote / 0xXX；鼠标：mousex1 / mousex2 / mousemiddle
hotkey = rightctrl

# false = 按住说话（默认）；true = 按一下开始、再按一下结束
toggle = false

model_dir = models/sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05

# 悬浮：off / bar / orb / edge；光色：white / rainbow / amber / cyan / aurora
overlay_mode = edge
edge_tint = white

# 自动标点 / 数字归一化 / 静音断句秒数（0 = 不断句）/ 推理线程
punct = true
itn = true
rule2 = 15
threads = 2
```

全部可用键：`hotkey` `model_dir` `punct_dir` `hotwords` `punct` `itn` `beam` `toggle` `tray`
`hide_console` `overlay_mode` `edge_tint` `stream` `lazy_mic` `max_backspace` `throttle_ms`
`threads` `device` `rule1` `rule2` `rule3` `punct_mode` `merge_terms` `fix_case` `ctx_bias`
`ctx_hotwords` `leading_space` `verbose_ctx` `log` `log_dir` `script` `tail_ms`
`tail_max_ms` `tail_quiet_ms` `tail_pad_ms` `tail_rms`。

`sainput config` 打印当前生效的主要配置（模型 / 标点 / ITN / 热词 / 尾巴参数等），不会回显整份文件。

### 热词

把常说但通用模型容易听错的词写进 `hotwords.txt`（一行一个，`#` 是注释）：

```
sainput
sherpa-onnx
大语言模型
```

非空时会自动切到 `modified_beam_search`（更准、稍慢）。只有注释（等于空表）时不会切，
热词文件缺失也只是少一层偏置，不会把语音输入搞挂。

仓库里的 `hotwords.example.txt` 全是示例注释（`# sainput` 这种），复制成 `hotwords.txt`
后需要**去掉行首 `#`** 才会生效。

---

## 6. 命令行

```
sainput devices                              列出输入设备
sainput record out.wav [--seconds N]         采一段存成 16k 单声道 WAV
sainput probe [--seconds N]                  只看采集节奏 / 抖动 / 丢帧
sainput replay in.wav [--dry-run]            把 WAV 走完整流水线（离线验收，不注入）
sainput render "一段文字"                     只跑 ITN + 标点 + 润色
sainput config                               打印当前生效的主要配置
sainput overlay-demo [--mode bar|orb|edge]   预览悬浮显示（默认 bar）
         [--seconds N] [--tint white|rainbow|amber|cyan|aurora]
sainput release-keys                         给所有修饰键补发 keyup（键盘卡住时的救命命令）
sainput run [options]                        常驻：按住热键说话，松开上屏
sainput selftest inject "文字"                只测上屏路径
```

### 常用开关

| 开关 | 作用 |
|---|---|
| `--hotkey <KEY>` | 热键（默认 `rightctrl`；也支持 `mousex1` / `mousex2` / `mousemiddle`） |
| `--toggle` / `--hold` | 按一下切换 / 按住说话（默认后者） |
| `--model-dir <DIR>` | ASR 模型目录 |
| `--punct-dir <DIR>` | 标点模型目录 |
| `--no-punct` / `--punct` | 关 / 开标点（默认开） |
| `--no-itn` / `--itn` | 关 / 开数字归一化（默认开） |
| `--punct-mode keep\|space` | 标点原样 / 中文标点转空格（问号保留） |
| `--hotwords <FILE>` | 热词表（默认根目录 `hotwords.txt`） |
| `--ctx-bias` / `--no-ctx-bias` | 上下文偏置开关（默认开） |
| `--ctx-hotwords <FILE>` | 偏置临时热词文件（默认 `.ctx_hotwords.txt`） |
| `--merge-terms` / `--no-merge-terms` | 英文拆字合并(默认开) |
| `--fix-case` / `--no-fix-case` | 英文大小写修复(默认开) |
| `--ime-wmchar auto\|on\|off\|<策略>:<exe>[,<exe>]` | 上屏方式(默认 `auto`):目标窗口挂着输入法时,注入的 ASCII 会被输入法收走,这时改走 `WM_CHAR`(汉字不受影响,仍走键盘)。`on` = 到处都用;`off` = 老行为;挂名单 = 只对这些程序生效(如 `on:pebrel.exe`)。`on` 的窗口走消息通道(字符 `WM_CHAR` + 退格 `WM_KEYDOWN/UP(VK_BACK)`,同一队列所以顺序有保证),**照常逐帧流式**;`nostream:` 才是整句一次投递;`pebrel` = 完全不发按键,整句先放进 Pebrel 自己的组合态缓冲层(光标处可改、定稿才提交,只对 `pebrel.exe`/`nebula.exe` 生效) |
| `--leading-space` / `--no-leading-space` | 光标前有字时补空格（默认开） |
| `--beam` / `--no-beam` | `modified_beam_search`（更准、稍慢；热词需要它） |
| `--threads N` | 推理线程数（默认 2） |
| `--throttle-ms N` | 流式渲染节拍（默认 80ms） |
| `--max-backspace N` | 流式修正的安全阀（默认 8） |
| `--device <NAME>` | 指定麦克风 |
| `--lazy-mic` / `--warm-mic` | 按需开麦 / 常驻预热（默认预热） |
| `--overlay-mode off\|bar\|orb\|edge` | 悬浮显示方式（默认 `edge`） |
| `--edge-tint <COLOR>` | 边缘光颜色 |
| `--tray` / `--no-tray` | 托盘图标（默认开） |
| `--hide-console` / `--show-console` | 收起控制台（`start.bat` 默认带上它）/ 保留控制台便于调试 |
| `--log` / `--no-log` / `--log-dir <DIR>` | 会话日志（默认开，写 `logs/`） |
| `--rule1 N` / `--rule2 N` / `--rule3 N` | 断句门槛（秒）：尚未出字时的静音 / 已出字后的静音 / 单句最长（默认 15 / 15 / 20） |
| `--tail-ms` `--tail-quiet-ms` `--tail-max-ms` `--tail-pad-ms` `--tail-rms` | 松手尾巴（见 §8） |
| `--python <EXE>` | 指定 Python 解释器（默认自动探测） |
| `--dry-run` | `replay` 时不真的注入，只打印文本 |
| `--feed <WAV>` + `--feed-release-at <秒>` | 测试用：把 WAV 当麦克风喂，到第 N 秒自动松键（吞字回归） |

> ⚠️ **`--no-stream` 不可用（未实现）**：它把流式上屏关掉后，当前版本不会退化成
> 「松手一次上屏」，而是**一个字都不打**。请勿使用；需要单次上屏请等后续版本。

---

## 7. 模型

**默认：`sherpa-onnx-x-asr-160ms-...-zh-en-int8-2026-06-05`**（16k 单声道流式 transducer，中英混说）。

模型下载：

- 通用 ASR：<https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models>
- 标点：<https://github.com/k2-fsa/sherpa-onnx/releases/tag/punctuation-models>
- `x-asr` 系列：<https://github.com/Gilgamesh-J/X-ASR>

`scripts/fetch_models.ps1` 会下默认 ASR + 标点两个包并解到 `models/`（已有目录默认跳过，
`-Force` 才覆盖）：

```powershell
powershell -File scripts/fetch_models.ps1                        # 默认 160ms + 标点
powershell -File scripts/fetch_models.ps1 -Variant 480ms         # 换档
powershell -File scripts/fetch_models.ps1 -Variant zh14m -NoPunct
powershell -File scripts/fetch_models.ps1 -DryRun                # 只打印将下载什么
```

可选 `-Variant`：`160ms`（默认）/ `480ms` / `960ms` / `nemotron` / `bilingual` / `zh14m` / `all`；
另有 `-KeepArchive` 保留 `.tar.bz2`。换模型也可以直接改 `model_dir`（一行 `sainput.conf`）。

- 选型与全部实测数据：[`docs/model-selection.md`](docs/model-selection.md)
- 每个模型的上游地址、体积、适用场景：[`models/README.md`](models/README.md)

### 跟手感：延迟卡在哪（实测）

结论：**卡在模型，不在 sainput**。延迟分三段：

| 段 | 实测 | 能不能再压 |
|---|---|---|
| 第一个字：说出口 → 屏幕上出现 | 语音起点之后 **770~860ms** | 只能换模型 |
| 之后每个字的刷新间隔 | p50 **320ms**，每次 1~4 个字 | 由模型 chunk 决定；实测最小间隔 160ms |
| 松手 → 最后一个字上屏 | **200~560ms**（含补底噪往返约 130ms） | 就是「尾巴」，见 §8 |

`throttle_ms` 实测（同一段真实语音、按真实 32ms 节拍喂帧）：

| throttle_ms | 文本更新次数 | 更新间隔 p50 | 每次新增字数 |
|---|---|---|---|
| 0 | 15 | 320ms | 1 2 2 3 1 1 2 4 0 4 2 1 1 1 |
| 40 | 15 | 320ms | 同上 |
| **80（默认）** | 15 | 288ms | 同上 |
| 160 | 14 | 320ms | 同上 |
| 320 | 10 | 320ms（每次跳 6 个字） | 3 2 4 1 6 6 1 1 1 |

**0 / 40 / 80 三档完全一样 → 80 已经等于不限流**；反过来 320 会明显变顿。
首个字位置在 0~160 三档里都是 992ms，说明它跟节流无关。

### 分块还能不能更小

翻完 sherpa-onnx 官方 `asr-models` 全部 **498 个 asset**：中文/中英流式模型只有
**160 / 480 / 960 / 1920ms** 四档，**没有比 160ms 更小的中文档**。

唯一 <160ms 的是多语种 **NVIDIA Nemotron-3.5 streaming 0.6B（80ms 分块）**。
已下载实测：中文能认对，首个字 **555ms**（比 160ms 档快约 250~300ms）、刷新间隔 p50 **192ms**；
但 **0.6B 参数在测试机（2 线程）跑不动** —— 按墙钟测更新间隔 337ms > 音频间隔 192ms，
也就是**比实时慢**（越说越落后）；开到 8 线程才勉强追平（204ms vs 192ms），CPU 基本吃满。

**结论：想更快只能换引擎 / 换硬件；sainput 这一侧已经压到底了。**

### 模型横评（同一批真实语音）

| 模型 | 真机中文重复标记 | 中英混说 | RTF |
|---|---|---|---|
| x-asr-480ms | **0** | `昨天是MONDAY TODAY IS礼拜二，THE DAY AFTER TOMORROW是。` | 0.10 |
| x-asr-160ms（默认） | 0 | `…TODAY IS 礼拜二 TODAY AFTER TOMORROW`（丢词） | 0.22 |
| x-asr-960ms | 0 | 同 480ms | 0.07 |
| streaming-zipformer-zh-int8-2025-06-30 | 0 | 英文全乱 | 0.15 |
| bilingual-zh-en-2023-02-20 | **4** ❌ | `LIBR THE DAY AFTER TOMORROW`（英文最好） | 0.08 |
| zh-14M-2023-02-23 | 0 | 英文差 | 0.07 |

旧的双语档在真机中文上会**重复打转**（`重点重点呢…动动动荡荡荡`）；x-asr-480ms 同一段
输出干净，并且 `MONDAY` / `THE DAY AFTER TOMORROW` / `ALWAYS` / `FREQUENTLY` 都认得出。

### 备选档

| 档 | 适合 |
|---|---|
| x-asr-480ms | 比 160ms 更准（英文更完整），但每次刷 3–4 个字、手感更顿 |
| x-asr-960ms | 最省 CPU（RTF 0.068），手感最差 |
| streaming-zipformer-zh-int8-2025-06-30 | 纯中文用词更准；**英文会听成乱码** |
| bilingual-zh-en-2023-02-20 | 英文识别最好；**真机中文会打转**，除非你几乎只讲英文 |
| zh-14M-2023-02-23 | 24MB 超轻量 |
| nemotron-3.5-asr-streaming-0.6b-80ms-int8 | 80ms 分块、首个字最快（555ms）、中文可用；**代价是 CPU**（627MB 模型，建议 `threads = 8`） |

许可状态见 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) —— 其中两个模型**许可待确认**，
确认前不建议随包分发。

**别再重复试的方向**（已证无效）：

- `beam = true`：更容易错（「昨天」→「昨天天」），耗时无优势。
- 空白惩罚（sidecar 的 `blank_penalty` 参数，sainput 不暴露配置键、也没有对应命令行开关）0 / 0.5 / 1.5：输出逐字相同。
- 把音频衰减到 -40dB：重复度基本不变 —— 重复不是电平问题。

### 重复修复兜底（`tools/repair.py`）

换模型消掉了绝大部分打转，但模型永远可能偶发，所以渲染链最前面还有一道保守修复：

- 同一个字连续 ≥3 次 → 压成 1 个（`叠叠叠叠叠叠` → `叠`）；笑声字压到 2 个（`哈哈哈` → `哈哈`）
- 含中文的 3 字以上片段原样重复 → 保留一次（`为什么为什么` → `为什么`）
- 「不叠字」的字连着两遍 → 一个（`识别别` → `识别`）

**不许动**的正常内容：`看看` / `刚刚` / `妈妈` / `天天` / `慢慢` / `很多很多` / `一步一步` /
`可以可以` / `ALWAYS ALWAYS`（22 项自测）。

---

## 8. 松手之后的尾巴（吞字就是这么修的）

### 现象

说完最后一个字就松手，那个字经常就没了 —— 渲染链还会给它补个句号，看起来像「说完了」。

### 原因（实测，不是猜的）

这个流式模型有**右上下文**：最后一个字要等它「听到」后面还有一段音频才肯吐出来。
把同一段音频（末尾两个字是「频繁」）在最后一个响帧处停喂（= 松手）再对比：

| 停喂后还喂多少 | 定稿文本 |
|---|---|
| 0ms（旧行为：立刻 finish） | `…FREQUENTLY。` ❌ 少了「频繁」 |
| 160ms 实时音频 | `…FREQUENTLY。` ❌ 还是少 |
| 320ms | `…FREQUENTLY频。` ⚠️ 只回来一个字 |
| 480ms | `…FREQUENTLY频。` ⚠️ |
| **640ms** | `…FREQUENTLY频繁。` ✅ |

也就是说：松手那一刻，最后一个字其实**已经录进麦克风了**（还在采集缓冲 / 无线麦缓冲里飞），
只是立刻 `finish` 把它扔了；就算不扔，模型也要再听约 500ms 音频才肯吐字。

### 现在的做法（松手 → 收尾 → 定稿）

1. 松手后**不立刻 finish**，先把麦克风里还在流的真音频喂进去：至少 `tail_ms`（默认 200ms），
   直到连续安静 `tail_quiet_ms`（默认 250ms）为止，最多等 `tail_max_ms`（默认 1200ms）。
2. 然后**一次性灌 `tail_pad_ms`（默认 480ms）的「房间底噪」**当静音，补上模型的右上下文，
   让它把最后一个字吐出来。这一步是瞬间的（约 130ms 往返），不需要真的再等 640ms 实时音频。
3. 再 `finish`。

实测（真按热键、真松开）：

| | 松手 → 定稿 | 定稿文本 |
|---|---|---|
| 旧行为 | 5ms | `就是频繁的啊，不认识记下来FREQ。` ❌ |
| 现在 | 560ms + 133ms | `就是频繁的啊，不认识记下来FREQUENTLY频繁。` ✅ |

屏幕上打出来的真实结果也验过（把记事本内容读回来比对）。

### 一个坑：静音补偿必须用「底噪」，不能用纯 0

- 补 **纯 0**：模型会多吐一个虚词 —— `…FREQUENTLY频繁的。`
- 补 **底噪**（实测房间底噪 RMS≈0.003 的白噪，或真实房间噪声）：`…FREQUENTLY频繁。` ✅

所以 `pad` 命令是把「尾巴里安静帧的平均 RMS」交给 sidecar，由它按同样量级生成噪声再喂给模型。

### 参数

| 键 | 默认 | 作用 |
|---|---|---|
| `tail_ms` | 200 | 松手后最少再收多久的真音频 |
| `tail_quiet_ms` | 250 | 尾巴里连续安静这么久就收工（说话中会被推迟）。太短会把「松手后补的半句」切掉 |
| `tail_max_ms` | 1200 | 尾巴硬上限，还在说话也不会无限等 |
| `tail_pad_ms` | 480 | 收工时一次性补多少毫秒底噪 |
| `tail_rms` | 0.012 | 安静判定阈值（帧 RMS）。测试用无线麦底噪 p50=0.0028 / p99=0.0102 |

嫌最后一个字上屏慢可以调小 `tail_pad_ms`，代价是又可能吞字（实测 320ms 就只剩一个字）。

### 回归测试（改完这几行必须跑）

```
powershell -File tools/test_tail_up.ps1 -Label new      # 真按下、真松开
powershell -File tools/test_tail_up.ps1 -Label old -ExtraArgs "--tail-ms 0 --tail-max-ms 0 --tail-pad-ms 0"
powershell -File tools/test_tail_screen.ps1 -Label new  # 连带屏幕上打出来的字一起验
python tools/probe_stream.py --wav <wav> --stop-at 3968 --pad-ms 480 --pad-kind noise
```

> 最后一条是 `tools/probe_stream.py` 的参数（离线探针），不是 `sainput` 的开关。

判定标准：旧行为**必须**吞掉末尾（`…FREQUENTLY。`），新行为**必须**留住（`…FREQUENTLY频繁。`）。

---

## 9. 构建

```powershell
build.bat build --release
```

`build.bat` 只是给 `cargo` 补上工具链 PATH 的薄壳。两条路都行：

| 工具链 | 做法 |
|---|---|
| **GNU**（仓库示例） | 把 `rust-toolchain.example.toml` 复制为 `rust-toolchain.toml`（锁 `1.91.1-x86_64-pc-windows-gnu`），`.cargo/config.toml`（由示例复制）指向 w64devkit 的 `gcc.exe` 作链接器 |
| **MSVC** | 装 VS Build Tools；删掉 `.cargo/config.toml` 里的 `[build] target`，把 `rust-toolchain.toml` 的 channel 改回 `1.91.1` |

仓库提供 `rust-toolchain.example.toml` 与 `.cargo/config.toml.example`，复制成正式文件名后按本机路径改。
两个已知的 GNU 坑：

- GCC 16 删了 `libgcc_eh.a` 但 rustc 仍传 `-lgcc_eh`（用空归档顶上）。
- `.bat` 里别写中文注释，cmd 的 OEM 代码页会吃成乱码（本仓库脚本已全部 ASCII）。

产物在 `target\<triple>\release\sainput.exe`。

---

## 10. 已知限制

- **某些窗口收不到注入**：以管理员身份运行的程序（UIPI）、部分游戏、某些终端会拒绝
  SendInput。这类窗口目前打不进去（TSF 上屏还没做）。
- **只认系统默认麦克风**，或 `device = <名字>` 指定的那一个。
- **松手之后要多等约 200~560ms** 才落句尾（标点 / 最后一个字的修正）：这是尾巴的代价，
  换来的是不吞字。真正的实时字在说话过程中就已经上屏了。
- **标点在流式阶段略保守**：句尾标点要等这一句定稿才落笔，避免屏幕上一闪一闪。
- **中文 ITN 是保守规则**，不是训练模型：「一点小事」「十分高兴」这类歧义词一律不动，
  宁可少改也不改错。
- **模型偶发重复循环**：小声、语速快、或一句话里突然切换语言时，zipformer 会在词尾打转
  （「输出出了」这种）。这是模型固有行为，参数调不掉；停下重说一遍通常就好。
- **它不是带主界面的 App**：托盘图标 + 热键就是全部交互。
- **句中停顿会被断句**：静音超过 `rule2`（**默认 15 秒**，托盘可选 5/10/15/30/不断句）
  就当一句说完，补句号并另起一句。注意这是 **RMS 能量计时**，不是语义 VAD。
- **`--no-stream` 不可用**（见 §6）。
- 平台只有 Windows 10/11 x64；macOS / Linux 不支持。

---

## 11. 隐私

- **识别与标点全程离线**：sherpa-onnx 在本机 CPU 上跑，运行期不发起任何网络请求。
- **日志含你的输入文本**：每次 `run` 写 `logs/sainput-<时间>.log`（partial / final / 尾巴信息）。
  不想留就 `log = false`，或用 `--log-dir` 指到临时目录。
- **`.ctx_hotwords.txt` 含光标前的上下文词**（上下文偏置的中间产物），只写本地、不进仓库。
- 个人配置 `sainput.conf`、`hotwords.txt`、`logs/`、`models/` 全部在 `.gitignore` 里，
  不会被误提交。
- 项目不上报任何遥测、不内置账号体系。

---

## 12. 排查用：会话日志

每条日志形如：

```
P   6543ms  对我            [raw] 对我            (2.9ms)   <- partial：渲染结果 / 识别原文 / 渲染耗时
F  12345ms  (endpoint)  对我做了介绍。   [raw] 对我做了介绍   <- final：定稿原因 / 最终文本 / 原文
T  12348ms  松键→收尾 268ms | 尾巴真音频 4 帧 | 连续安静 128ms | 底噪 RMS 0.0028 | 补 480ms 底噪 | ...
```

报「吞字 / 慢 / 识别错」时带上对应日志即可精确定位 —— 是模型没听出来（raw 里就没有），
还是渲染弄丢了（raw 有、text 没有）。

首次启动失败、Python 依赖、模型缺失、管理员窗口打不进字、键盘卡住、控制台残留等
逐项排查见 [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md)。

---

## 13. License

[MIT](LICENSE)。第三方组件与模型许可见 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)。

**本仓库不分发任何模型权重**，也不分发用户配置与日志；模型由用户自行下载。

贡献前请读 [`CONTRIBUTING.md`](CONTRIBUTING.md)；安全问题见 [`SECURITY.md`](SECURITY.md)。

---

## 14. 下一步（未做）

- TSF 上屏工程化（替换 SendInput，解决管理员窗口打不进去）
- 竞态防护：忽略已落定的流式结果、final 后丢弃迟到结果、焦点变更时丢弃提交
- 双通道整句重打（慢通道在定稿后回填纠错，可撤销）
- llama.cpp + 小模型异步回填 + 用户词学习闭环
- 尾巴余项：把「首个字 770~860ms」再压一档（候选：Nemotron 80ms + 8 线程，或等 sherpa-onnx 出 <160ms 的中文流式档）
