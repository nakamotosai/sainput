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
| 启动后进程立刻退出 | 已经有一个 sainput 在跑（单实例锁） | 右键托盘 → 退出旧的 |
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
- 正确退出永远是：右键托盘 → 退出 sainput。

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

- 托盘图标才是「它在跑」的可靠标志。图标不见但进程还在：Windows 会把新图标收进
  「隐藏的图标」区域，点任务栏的 `^` 找一下。

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
| 词总被听错 | `[raw]` 里就是错的 | 加进 `hotwords.txt`；见 [`../hotwords.example.txt`](../hotwords.example.txt) |
| 长句重复打转 | `[raw]` 里就重复 | 模型固有行为，换档见 [`model-selection.md`](model-selection.md) |
| 中英混说丢词 | — | 试 `-480ms` 档 |

`--no-stream`（`stream = false`）**当前版本不可用**：它实际一个字都不打。
需要逐字上屏就保持默认 `stream = true`。

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
