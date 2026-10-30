# 排查手册

出问题按下面的顺序查：**日志 → 前台跑一遍 → 配置 → 依赖 → 模型 → 权限**。
sainput 的报错都带 `[sainput]` 前缀，几乎每条都直接告诉你去修什么。

---

## 0. 先看日志

每次 `run` 都会写 `logs\sainput-<本地时间>.log`（`log = false` 可关）。三行样例：

```
P   6543ms  对我            [raw] 对我            (2.9ms)
F  12345ms  (endpoint)  对我做了介绍。   [raw] 对我做了介绍
T  12348ms  松键→收尾 268ms | 尾巴真音频 4 帧 | 连续安静 128ms | 底噪 RMS 0.0028 | 补 480ms 底噪
```

- `P` = partial，`F` = final（括号里是定稿原因），`T` = 松手后的收尾过程。
- `[raw]` 是模型原文，前面那条是渲染后的文本。**raw 里就没有** → 模型/音频问题；
  **raw 有、渲染后没有** → 渲染链问题。

想看实时输出：

```bat
run.bat run --show-console
```

`run.bat` 是前台运行（窗口留着，方便读输出）；`start.bat` 是后台启动（窗口会消失）。

查当前生效的配置（命令行 > `sainput.conf` > 默认）：

```bat
run.bat config
```

---

## 1. 首次启动就失败

按这个顺序过一遍，每一步都能单独验证：

| 检查 | 命令 | 期望 |
|---|---|---|
| 编译产物 | `start.bat` 的提示 | 找 `target\release\` 或 `target\x86_64-pc-windows-gnu\release\` |
| 没编译 | `build.bat` | 成功产出 `sainput.exe` |
| sidecar 脚本 | 看 `tools\asr_sidecar.py` 是否存在 | 必须在仓库里 |
| Python | `run.bat` 启动后看有没有 python 相关报错 | 见第 2 节 |
| 模型 | `models\sherpa-onnx-x-asr-160ms-...\` 是否存在 | 见第 3 节 |

### 常见报错对照

| 报错 | 原因 | 处理 |
|---|---|---|
| `找不到模型目录: ...` | 没下模型，或 `model_dir` 写错 | 见第 3 节 |
| `找不到 sidecar 脚本: ...` | 把 exe 拷到别处跑了 | 从仓库内启动；或 `script = <绝对路径>` |
| `标点模型不在 ... —— 本次不加标点` | 只是警告，不是故障 | 需要标点就下标点模型 |
| `热词文件不存在: ...` | `hotwords = <路径>` 指向不存在的文件 | 删掉该行，或改成实际路径 |
| 启动后进程立刻退出 | 旧实例退不掉（常见于它是以管理员权限启动的） | 用同样权限结束旧进程，或右键托盘退出后重开 |
| 卡在等 sidecar | 模型首次加载慢 / Python 依赖缺 | 前台跑 `run.bat run`，看真实报错 |

> `find_root` 的规则：从 exe 所在目录往上最多 6 层找 `tools\asr_sidecar.py`，
> 找不到就退回当前工作目录。所以**别把 sainput.exe 单独拷出去用**。

---

## 2. Python 依赖缺

sidecar 只需要两个包：

```bat
python -m pip install -r requirements.txt
```

验证：

```bat
python -c "import sys, struct; print(sys.version); print(struct.calcsize('P')*8, 'bit')"
python -c "import numpy, sherpa_onnx; print(sherpa_onnx.__version__)"
```

- 必须是 **64 位 Python 3.11+**。上面第二行打印 `32 bit` 就是装错了 —— `sherpa-onnx`
  没有 32 位轮子，装上了也只会在 import 时报 `DLL load failed`。
- `ModuleNotFoundError: No module named 'sherpa_onnx'` → 包没装，或者装到了另一个 Python。
  用 `python -m pip`（带 `-m`）能保证装进当前解释器。
- 用虚拟环境时，把解释器写进配置，别依赖 PATH：

  ```ini
  python = C:\path\to\venv\Scripts\python.exe
  ```

- sainput 找 Python 的顺序：环境变量 `SAINPUT_PYTHON`（最高优先）→ PATH 里的 `python`
  → `python3` → `py`。双击 `start.bat` 时 PATH 可能不含 Python，所以直接配
  `python = ...`（或设 `SAINPUT_PYTHON`）最省事。
- 控制台里中文乱码：`start.bat` / `run.bat` 已经设了 `PYTHONIOENCODING=utf-8`。

---

## 3. 模型缺

```powershell
powershell -ExecutionPolicy Bypass -File scripts\fetch_models.ps1 -DryRun   # 只看要下什么
powershell -ExecutionPolicy Bypass -File scripts\fetch_models.ps1           # 默认档
```

脚本会校验大小与必要文件，已完整就跳过。手动下载见 [`../models/README.md`](../models/README.md)。

- 目录结构必须是一层：`models\<asset 名>\tokens.txt`、`encoder*.onnx` 等。
  多套一层（`models\x\sherpa-onnx-...\`）会找不到。
- `model_dir` / `punct_dir` 的**相对路径是相对仓库根**，不是相对 exe。
- 换档之后记得清掉 `.ctx_hotwords.txt` 之类临时文件不太必要，但一定要确认
  `run.bat config` 打印的模型名是你想要的。

---

## 4. 管理员窗口打不进字

上屏走 `SendInput`，Windows 的 **UIPI** 会拦住「普通权限进程 → 管理员权限窗口」的注入。

- 验证方法：记事本里正常、以管理员运行的程序里没反应 → 就是 UIPI，不是 bug。
- 处理：以**同等权限**运行 sainput（右键 → 以管理员身份运行 `start.bat`），
  或者把目标程序降到普通权限。
- 彻底解法是 TSF 输入法（路线图 M3），当前版本没做。

另外这些窗口也可能收不到：部分全屏游戏、某些自绘终端（用 DirectX 独占的）、
带反注入保护的程序。

---

## 5. 键盘卡住（最需要小心的一条）

**症状**：空格打不出来、按着 Ctrl 滚轮在缩放、Alt 弹菜单 —— 系统认为某个修饰键一直按着。

**原因**：热键钩子会吃掉这个键的 keydown，配套的 keyup 必须由同一个钩子送达。
如果进程被强杀，keyup 就永远丢了。

**处理**：

1. 右键托盘 → **释放卡住的键**；
2. 托盘不在了就命令行：

   ```bat
   run.bat release-keys
   ```

   这个命令只是补发一次 keyup，**不需要 sainput 正在跑**，也不会启动常驻进程。

**绝对不要**：

- `Stop-Process -Force`、任务管理器「结束任务」、`taskkill /F` 杀 sainput。
- 正确退出永远是:右键托盘 → 退出 sainput。

### 5.1 鼠标侧键卡住(2026-10-28 真实事故,本机持续约 20 分钟)

**症状**:鼠标**能移动**,但**点哪都没反应** —— 点击其实送到了系统(能看到 `L=D` / `L=up` 事件、
`GetLastInputInfo` 也在刷新),却没有任何窗口被激活;`GetAsyncKeyState(VK_XBUTTON1)` 恒为 DOWN。
键盘完全不受影响。

**机制(本机实测,不是推测)**:

| 实验 | 结果 |
|---|---|
| 不装钩子:注入 `MOUSEEVENTF_XDOWN` → `MOUSEEVENTF_XUP` | 状态 DOWN → up,**可逆** |
| 装一个「吃掉 X1」的低级鼠标钩子,注入同样两个事件 | 钩子记到吞掉 2 次,`GetAsyncKeyState` **始终 up** |

结论:**被低级钩子 `return 1` 吃掉的事件根本不进系统的按键状态机**。由此两条推论:

1. 闩锁只能来自「**没被吃掉的那一次 down**」——另一个进程注入 XDOWN 后没配对 XUP(当时的真凶是
   水杉输入法的语音热键 + `tools/x1_to_ralt.ps1` 这条链),或者钩子安装的间隙;
2. 一旦闩住,**之后所有 XUP(包括别人注入的)都会被钩子吃掉** → 状态自己永远解不开。
   (推论 2 的「抬起方向」是由同一机制外推的,未单独实测。)

**处理(按代价从小到大)**:

1. 先杀掉「和 sainput 抢同一个侧键」的钩子拥有者(查 `MetasequoiaImeServer` / 变量:任何装了
   WH_MOUSE_LL 又在处理 X1 的东西)。杀掉后 `GetAsyncKeyState(VK_XBUTTON1)` 常常自己回到 up。
2. **物理按一下侧键再松开** —— 一次真实的 down/up 对能解开(本机验证有效)。前提同上:当时没有吞键钩子。
3. `run.bat release-keys`(补发 X1/X2/中键 + 修饰键抬起)。
   ⚠️ **sainput 正跑着的时候这条对热键本身无效**:命令是另一个进程,但它注入的 XUP 仍会经过
   sainput 自己的全局鼠标钩子并被吃掉(见上面推论 2)。所以要么先退出 sainput,要么用托盘菜单里
   在进程内的那条(实现上必须先卸钩子再注入)。

**为什么这条要写进 sainput 自己的代码**:见 `HANDOFF.md` §7 已知限制 #3。

---

## 6. 终端 / 控制台残留

- **双击 `start.bat` 后不该留下窗口。** 如果留了，先确认残留的是不是你自己的终端
  （cmd / Windows Terminal 本来就有一个窗口）。
- `start.bat` 用 `start /b` 让 sainput 继承脚本的控制台，脚本随即退出，
  控制台跟着消失；sainput 起来后自己 `FreeConsole`，所以不受影响。
  改成 `start ""`（新建窗口）或直接双击 exe 都会留下黑窗口 —— 这是已踩过的坑。
- 想在启动后立刻收起控制台：`sainput.conf` 里 `hide_console = true`，
  或命令行 `--hide-console`（`start.bat` 已经带上了）。
- 只看进程在不在（**只查，不要杀**）：

  ```bat
  tasklist | findstr /i sainput
  ```

- 托盘图标才是「它在跑」的可靠标志。图标不见但进程还在：
  - **Explorer 崩溃 / 更新 / 重启会把通知区域整个重建**，所有托盘图标一起消失。
    sainput 收到系统的 `TaskbarCreated` 广播后会自动把图标重新挂回去。
  - 图标只是收进了「隐藏的图标」区域时，点任务栏的 `^` 找一下。
  - 图标确实找不到了，或者想强制切到最新构建：**再双击一次桌面快捷方式**。新实例
    会让旧实例正常退出（退不掉就强制结束，连 sidecar 一起）然后自己接管 ——
    始终只有一个进程，且跑的一定是最新二进制。

---

## 7. 悬浮显示不出来

- 默认 `overlay_mode = edge`（屏幕边缘光）；`bar` 是细条，`orb` 是悬浮球，`off` 全关。
- 停在托盘图标上看不到预览时，用：

  ```bat
  run.bat overlay-demo --mode bar --seconds 10
  ```

- 边缘光在纯白 / 高亮背景上不明显，换 `edge_tint`（`white` / `rainbow` / `amber` / `cyan` / `aurora`）。
- 远程桌面、部分虚拟机里没有桌面合成，悬浮层可能不绘制。

---

## 8. 识别质量

| 现象 | 先看 | 处理 |
|---|---|---|
| 吞最后一个字 | 日志里的 `T` 行 | 调大 `tail_pad_ms`（默认 480），或 `tail_quiet_ms` |
| 松手后半天不落句 | `T` 行的「松键→收尾」 | 调小 `tail_pad_ms` / `tail_max_ms` |
| 句子被切碎 | 日志里 `F` 行原因是 endpoint | 调大 `rule2`（默认 15 秒） |
| 词总被听错 | `[raw]` 里就是错的 | 加进 `hotwords.txt`;见 [`../hotwords.example.txt`](../hotwords.example.txt) |
| 说到英文时整句被跳过 / 删掉 | `P` 行的**渲染文本**比上一行短、`[raw]` 却是完整的 | 已有过:标点模型吃掉正文(2026-10-28 修,`tools/test_punct_guard.py`);控制台/日志里会有一行 `标点模型丢了正文`。还在发生就把那行 `[raw]` 贴进 issue |
| 说英文时弹出输入法的候选面板、字进不去(微信聊天框最常见) | 目标窗口所在线程在用输入法(水杉 / 微软拼音 / MS-IME) | 输入法把注入的字母当成它自己的输入了(2026-10-28 加 `ime_wmchar` 兜底,2026-10-30 扩到 Qt 窗口 —— 微信 4.x 就是 `Qt51514QWindowIcon`)。先看日志有没有 `上屏改用 WM_CHAR`;没有就检查 `ime_wmchar`:写成 `pebrel` 或 `on:xxx.exe` 时,**名单外的窗口仍按 `auto` 判**(v0.2.5 起;v0.2.4 及以前会一律退回键盘,那就是微信丢英文的原因)。还不行就临时把那个窗口的输入法切到「直接输入 / 英文」,或按 M3 的 TSF 上屏来做 |
| 说一串数字出来的是汉字数字(一二三四五)、还带空格 | 日志 `F` 行的渲染文本 | 2026-10-30 起:整句就是一个数字串会逐位转成 `12345`,并且**不加标点、不补前置空格**(要能直接贴进验证码框);夹在句子中间的数字串要带提示词(`验证码/电话/身份证号…`)才转。见 `FROZEN-v0.2.5.md` |
| 数字句后面多了一个句号 / 空格(`12345.`、`12345 `) | 日志 `F` 行结尾 | 已修(2026-10-30):纯数字句跳过标点模型,渲染末尾统一 `rstrip()`。还有就贴那行日志 |
| 说英文时字进不去,想确认这个窗口到底吃哪种 | — | 跑 `python tools/probe_wechat_inject.py keys "helloTOKENabc"` 再 `wmchar "helloTOKENabc"`,两条各读回一次;`drive` 子命令则会走**生产代码**打完再读回。换窗口(=换判据)之前先拿到这份数据 |
| 走消息通道的窗口里字被插到中间 / 同一句反复叠加 | 该窗口是不是混了两条通道(SendInput + PostMessage),或者退格被忽略 | 2026-10-28 已修:消息通道**整段一条路**(字符 WM_CHAR、退格 WM_KEYDOWN/UP VK_BACK)且不再混键盘注入。还乱的话把该程序换成 `ime_wmchar = nostream:<exe>`(整句一次投递),并把那段日志给我 |
| Pebrel 里说英文还是被输入法收走 / 想「边说边改」 | 注入按键这条路在终端里只能逐字上屏+退格 | 换 `ime_wmchar = pebrel`(需要带 `pane.compose` 的 Pebrel:上游还没发布,本机用补丁版,见下面一节):一个按键都不发,整句先落在它自己的组合态缓冲层里反复改写,定稿才提交。日志里会有 `上屏改用 Pebrel 组合态缓冲层`;失败会写明原因(前台不是 Pebrel / 读不到 runtime.port / 目标 pane 找不到) |
| 长句重复打转 | `[raw]` 里就重复 | 模型固有行为，换档见 [`model-selection.md`](model-selection.md) |
| 中英混说丢词 | — | 试 `-480ms` 档 |

`--no-stream`（`stream = false`）**流式阶段一个字都不打**，松手定稿时整句一次性打字。
想边说边出字就保持默认 `stream = true`。

### 组合态上屏(`ime_wmchar = pebrel`)依赖的是打过补丁的 Pebrel

`pane.compose` 是我们给 Pebrel 加的协议方法(和 `pane.paste` 同一族):文本先进应用
自己的输入缓冲,可以无限次**整段改写**,定稿才作为**一次写入**提交给 PTY —— 所以终端
和 shell 历史里看不到任何中间态,也不需要退格算术。**上游还没发布**这个方法,本机用的是
打过补丁的构建,维护说明在 [`../tools/pebrel/README.md`](../tools/pebrel/README.md)。

要知道两件事:

- 生效时会有一行 `[sainput] 上屏改用 Pebrel 组合态缓冲层:…`
- **Pebrel 被官方更新覆盖后这条能力就没了**:说一句会在日志/控制台看到
  `这台 Pebrel 还没有组合态缓冲层(pane.compose)`。这时把 `ime_wmchar` 改回
  `on:pebrel.exe` 就立刻回到老路;要补回组合态按 `tools/pebrel/README.md` 的三步
  (关掉 Pebrel → 覆盖 `pebrel.exe` → 重开),sainput 这边一个字都不用改。

---

## 9. 一次完整的自检

```bat
run.bat devices                        :: 有没有麦克风
run.bat record out.wav --seconds 5     :: 能不能录
run.bat replay out.wav --dry-run       :: 能不能识别（不上屏）
run.bat render "三十四个苹果。"         :: ITN + 标点 + 润色
run.bat selftest inject "测试"          :: 能不能上屏（光标先放进记事本）
run.bat run                            :: 全流程
```

哪一步先失败，就查对应的章节。

---

## 10. 水杉输入法(Doubao 代理)这条路上的尾巴

> 🔚 **这条路 2026-10-28 已退役(保留本节作为历史记录)**。
> 侧键语音现在直接跑在 sainput 原生路径上(`hotkey = mousex1` + 本仓 `tools/asr_sidecar.py`),
> 水杉不再参与:它的 `[voice_input]` 已关(`voice_input = false`、`hotkey_ralt = false`),
> `asr_endpoint` 也从本机代理还原为默认。
> 退役的胶水件(`tools/x1_to_ralt.ps1`、`voice_stack_supervisor.ps1`、`check_voice_stack.ps1`、
> 启动文件夹里的 `Sainput Voice Stack.lnk`)已搬进 `F:\archive\20261028-075641-sainput-x1-glue\`。
> 退役原因:两套前端抢同一个侧键,且那条链的吞键钩子会让侧键闩在按下 → 整个鼠标点不动
> (见 §5.1)。下面这些「尾巴」分析对 `asr_sidecar.py` 本身仍然成立。

水杉输入法自带豆包流式客户端,配置里把 `asr_endpoint` 指到本机
`wss://127.0.0.1:19000/api/v3/sauc/bigmodel_async`
(`tools/sainput_doubao_proxy.py`),由它把音频喂给这个 sidecar。
这条路上「松手后最后一个词没了」有两个来源,先分清是哪一个。

**一、定稿时解码落后 → sidecar 丢尾巴(已修,2026-10-25)**

`pump()` 每轮最多解 4 帧,那是为了流式画面不被解码突刺卡住;以前这个限量
也套在 `finish` / VAD endpoint 上。只要解码落后过(长句、机器忙、别的 agent 抢 CPU),
`get_result()` 拿到的就是**没喂完**的音频,最后一个词直接消失。现在收尾走
`drain_all=True`(一次解完),`final` 消息里多一个 `tail_chunks`,表示定稿时
额外补解了多少帧:

- `tail_chunks = 0` → **定稿那一刻**没有积压(收尾不用补解);中途的突刺有没有顶到过
  4 帧限量,现场日志看不出来,只能拿探针在同样的音频上重放(`probe_tail.py` 的
  `pumps_at_cap`)。
- `tail_chunks > 4` → 定稿时积压超过了修复前的 4 帧上限,修复前这种情况尾巴必丢。

回归测试 `tools/test_tail_drain.py` 故意一次灌 1.9s 音频制造积压:修好后
`tail_chunks=16` 且尾巴还在;把限量塞回去,输出就变成
`就是频繁的啊,不认识记下来。FRE。`(尾巴没了)。

**二、音频里就没有尾巴 → 不在 sidecar 这边**

代理每段会话都写一行带时间戳的汇总,拿它和 `%TEMP%\sainput_x1_ralt.log`
(侧键按下/抬起的绝对时间)对一下:

```
[doubao-proxy] 08:37:27.440 session audio_ms=3940 pkts=20 tail_bytes=4480 first_audio=+225ms speech_span=3760ms pad=152ms finish=7ms listen=4239ms
```

(上面是一次真实按住说话的记录,新版还会多 `tail_chunks` 与 `dump_ms` 两个字段:
`tail_chunks=0 dump_ms=3940`)

- `audio_ms` ≈ 按住时长 → 音频全送到了,尾巴只能丢在识别/上屏那一段。
- `audio_ms` 明显小于按住时长 → 音频在输入法那边就少了(起录延迟记在
  `first_audio`,抬键到设备真正停止的那几十毫秒算在末尾)。
- `dump_ms < audio_ms` → 这段会话超过 60 秒,dump 只留了前 60 秒,别拿它当整段重放。

要坐实第二种,用 `--dump-dir` 存下每段的原始音频 + 最终文本,再离线重放同一段音频:

```bat
pythonw tools\sainput_doubao_proxy.py --dump-dir "%TEMP%\sainput_sessions"
python  tools\probe_tail_oracle.py --dir "%TEMP%\sainput_sessions"
```

`live` 与 `oracle` 一致 → 定稿路径没问题,问题在音频或模型本身;
不一致 → 尾巴就是在这段路径里丢的。dump 文件名里带 `live<N>ms-dump<M>ms-t<K>`,
`dump < live` 或 `t0`(现场没拿到文本)时脚本会直接标 SKIP / NOTE,不算尾巴丢失。
`--dump-dir` 默认关(会把语音原文写到磁盘),排查完记得去掉。

## 11. 侧键、波形、提示音都正常,但一个字都不出

> 🔚 **本节写于「水杉 + Doubao 代理」时代(2026-10-28 那条前端已退役)**。侧键语音现在直接跑
> sainput 原生路径,判据换成应用自己的输出:
> - 识别器子进程死了 → 控制台/日志一行
>   `[sainput] worker 退出: sidecar 提前退出(检查模型路径与 Python 依赖)`
>   (`crates/sainput-core/src/asr.rs` 用 stdout EOF 判活)。**原生路径只报错,不会自动换一个识别器**
>   —— 代理时代那条自动重生逻辑随代理一起退役了,重启 sainput 即可。
> - 逐帧细节看项目根的 `logs\sainput-*.log`,不再是 `%TEMP%\sainput_doubao_proxy.log`。
>
> 下面的「对号入座」保留作历史参考。

先看代理日志最近的几行:

```bat
powershell -NoProfile -Command "Get-Content $env:TEMP\sainput_doubao_proxy.log -Tail 15"
```

按日志里的说法对号入座:

- `session error: ... exited (code=N)` → 识别器子进程死了。`exit=` 后面是死法:
  `-1 (0xFFFFFFFF)` 是被外部强杀(`taskkill`、任务管理器、清理脚本),
  `-1073741819 (0xC0000005)` 之类是它自己崩了。原因写在
  `%TEMP%\sainput_sidecar.log`(子进程的 stderr,以前这里进的是 DEVNULL,
  所以死因查不到)。
- `sidecar died ... respawning` 后跟着 `sidecar ready (pid=... 7.1s)` → 代理已经
  自动换了一个新的识别器,等这行出现再按一次侧键就行(换的那 7 秒里按下的
  那一次不会有文字)。不需要重启代理,更不需要重启输入法。
- `already running; exit` → 这个端口已经有代理在跑(单实例互斥量按端口区分,
  19000 的名字仍是 `Local\SainputDoubaoProxy`),先关掉旧的。
- `model load failed: ...` → 模型路径或文件有问题,代理主动退出并把原因写进日志。

想确认此刻有几个识别器、挂在谁下面:

```bat
powershell -NoProfile -Command "Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -match 'asr_sidecar' } | Select-Object ProcessId,ParentProcessId,CreationDate"
```

正常只有一个,`ParentProcessId` 是代理的 pid。

### 识别器为什么会死

识别器(`asr_sidecar.py`)是一个独立子进程,跑在 `dist/.../runtime/python.exe` 上,
只从管道读 PCM:不碰麦克风、不出网、不读输入法配置。所以能弄死它的只有进程层面的事:

- 被外部强杀:PID 清理、"清残留 python"之类的脚本、任务管理器收尾。
  它平时看起来就是一个 `python.exe`,最容易被顺手清掉。
- 自己崩:内存、模型文件损坏、onnxruntime 异常 —— 这时退出码会给出类别。

麦克风插拔不会直接杀它(它不碰音频设备)。设备变化只会让输入法重开一段会话,
所以「刚插拔完麦克风就不出字」通常是巧合:识别器更早之前就已经死了,而
死掉之后每一次按键都只是安静地失败,直到有人去看日志。

代理现在会把死掉的识别器自己换掉(约 7 秒),所以这类故障的代价是一次按键的
白等 + 十秒内自动恢复,而不是「从此永远不出字」。回归测试 `tools/test_proxy_respawn.py`
(用 `tools/stub_sidecar.py` 假识别器,3 秒跑完,不需要模型)。
