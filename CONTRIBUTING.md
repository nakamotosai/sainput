# 参与 sainput

sainput 是 Windows 上的**全离线**语音输入法：按住热键说话，松开即上屏。声音不出本机，
识别在本地跑（Rust 主体 + Python `sherpa-onnx` sidecar）。

这份文档只讲「怎么把代码跑起来、改完怎么验」。功能、配置、模型选型见
[`README.md`](README.md)；安全问题见 [`SECURITY.md`](SECURITY.md)。

---

## 1. 搭开发环境

只支持 **Windows 10/11 x64**（钩子、`SendInput`、托盘都是 Win32 接口，没有跨平台计划）。

| 依赖 | 版本 | 说明 |
| --- | --- | --- |
| Rust | `1.91` 或更新 | 版本号写在根 `Cargo.toml` 的 `rust-version`；CI 用 stable + 默认 MSVC target |
| MSVC 工具链 | VS Build Tools「使用 C++ 的桌面开发」 | 最省事的一条路（CI 也走它）；本机没有 MSVC 时用 GNU + w64devkit，步骤见 [`README.md`](README.md) §9「构建」，配套模板是 `.cargo/config.toml.example` 与 `rust-toolchain.example.toml` |
| Python | 3.11+ **64 位** | sidecar 用；`sherpa-onnx` 不提供 32 位轮子 |
| 麦克风 | 系统默认设备，或 `sainput.conf` 里 `device = <名字>` | `sainput devices` 可列出全部 |

安装 Python 依赖：

```bat
python -m pip install -r requirements.txt
```

下载模型（压缩包约 200 MB，解压后约 250 MB；默认 ASR 160ms + 标点模型）：

```bat
powershell -ExecutionPolicy Bypass -File scripts\fetch_models.ps1
```

脚本会校验文件大小与必需文件清单后解压到 `models\`；换档用 `-Variant 480ms|960ms|nemotron|bilingual|zh14m|all`。
模型目录本身在 `.gitignore` 里（体积），`models\README.md` 是唯一入库的说明文件。
完整的从零安装步骤见 [`README.md`](README.md) §2「系统要求与安装」。

### 首次跑起来

```bat
copy sainput.conf.example sainput.conf
copy hotwords.example.txt hotwords.txt
build.bat build --release
.\target\<triple>\release\sainput.exe config
.\target\<triple>\release\sainput.exe run
```

- `build.bat` 只是给 `cargo` 补工具链 PATH 的薄壳，等价于 `cargo build --release`；本机用 GNU 时靠它，
  用 MSVC 时直接 `cargo build --release` 即可。两条工具链的取舍见 [`README.md`](README.md) §9「构建」。
- `sainput config` 会打印它**实际**解析到的配置来源、Python、sidecar 脚本、各模型路径、热词表 —— 路径不对先看这里。
- `run` 是常驻模式：控制台 + 托盘图标。双击 `start.bat` 等价（还能自动收起控制台）。
- 光标放进任意可输入的窗口 → 按住热键（默认右 Ctrl）→ 松开上屏。
- 退出：**右键托盘图标 → 退出**。

---

## 2. 代码在哪

```
crates/sainput-core/   钩子、注入、文本整形、光标上下文、托盘、悬浮层（库）
crates/sainput-cli/    `sainput` 可执行文件：参数解析、子命令、主循环
tools/asr_sidecar.py   Python sidecar：麦克风 → sherpa-onnx 流式识别（stdin/stdout 各一行 JSON）
tools/*.py             探针/基准/后处理工具（会被 CI 做语法检查）
tools/*.ps1            真机回归脚本（要麦克风、会注入按键，CI 不跑）
```

---

## 3. 改完怎么验

### 3.1 单元测试（无需麦克风）

```bat
cargo test --release -p sainput-core
```

`--release` 不是顺手加的：跟手感与延迟相关的实测都在 release 产物上做，debug 构建更慢，
流式回调的时序没有参考价值。单测只能在 Windows 上跑（用例直接落在 Win32 接口上）。
现有单测覆盖光标上下文（句首/有无前缀时的空格判定）、上下文偏置（英文术语提取、去重、历史上限）
和注入（公共前缀回退）。

### 3.2 真机回归脚本（需要麦克风和真实桌面）

```bat
powershell -ExecutionPolicy Bypass -File tools\test_hotkey.ps1      # 热键按下/松开
powershell -ExecutionPolicy Bypass -File tools\test_e2e.ps1         # 说一句，看是否上屏
powershell -ExecutionPolicy Bypass -File tools\test_tail_up.ps1 -Label new   # 松手尾巴有没有吞字
```

这类脚本会**注入全局按键**，所以：

- 先把你日常在用的 sainput 从托盘退出，否则两边会抢同一个低级键盘钩子，测试结果不可信；
- 别在远程桌面 / 别人正在打字的机器上跑；
- 脚本会自己起一个 sainput 实例当被测对象（可执行文件路径写在脚本顶部的 `$exe`，用 MSVC 构建时先确认它指向你的产物）；
- 只清理**自己启动**的进程，并会还原剪贴板（公共逻辑在 `tools\_hygiene.ps1`），不会替你去杀一个正在跑的实例；
- 涉及吞字/慢的改动，判定标准写在 `README.md` 的回归测试小节，别靠「感觉快了」。

### 3.3 CI 做什么、不做什么

`.github/workflows/ci.yml` 在 `windows-latest` 上跑：

1. `cargo build --release --workspace`
2. `cargo test --release -p sainput-core`
3. `python -m compileall -q tools`（只查 Python 语法）

**不在 CI 里**下载模型、也不跑 `tools\*.ps1` —— 它们需要真实麦克风、真实前台窗口和 `SendInput`，
在无头 runner 上只会误报。

---

## 4. 红线（每条都踩过坑）

### 4.1 不要用 `Stop-Process -Force` 杀 sainput

> **红线：不要用 `Stop-Process -Force` 杀 sainput，退出走托盘，卡住用 `sainput.exe release-keys`。**

热键的 `keydown` 和 `keyup` 走同一个低级钩子。强杀进程会让 `keyup` 永远丢失，
系统认为修饰键一直按着 —— 表现为空格打不出、滚轮变缩放，**整块键盘像坏了一样**。

- 正常退出：**右键托盘 → 退出**（会先释放按键）。
- 已经卡住：托盘菜单的「释放卡住的键」，或命令行 `sainput.exe release-keys`。
- 测试脚本要结束自己启动的实例时，先 `release-keys` 再按 **PID** 杀（`tools\_hygiene.ps1` 已封装），
  永远不要 `Get-Process sainput | Stop-Process`。

### 4.2 `.bat` 只写 ASCII，别写中文注释

`cmd.exe` 按 OEM 代码页读批处理，源文件里的中文会成为乱码，甚至把命令吃掉。
`.gitattributes` 固定 `*.bat text eol=crlf`，不要改成 LF。

### 4.3 个人文件不进仓库

`sainput.conf`、`hotwords.txt`、`.ctx_hotwords.txt`、`.cargo/config.toml`、`rust-toolchain.toml`、
`logs/`、`models/`、`*.wav`、`*.pcm` 都在 `.gitignore` 里。要新增可分享的默认值，
改对应的 `*.example` 模板，**不要**把真实配置改名提交。

### 4.4 不要提交模型、音频、日志

`logs\sainput-*.log` 里逐条记录了你说出的原文，属于个人数据；模型是几百 MB 的上游二进制。
两者都不入库，也不要贴进 issue。

---

## 5. 提 PR

- **一个 PR 一件事**：修 bug 就别顺手重构别的模块；格式化的单独提。
- **说明白**：改了什么、为什么这么改、验证方式（用了哪个回归脚本、看到了什么结果）。
- **测试**：至少 `cargo build --release --workspace` + `cargo test --release -p sainput-core` 通过；
  动了热键 / 注入 / 尾巴 / 断句的，再跑对应的 `tools\test_*.ps1` 并贴输出。
- **`CHANGELOG.md`**：面向用户的改动加到 `[Unreleased]`；纯内部重构可以不写，但 PR 里要说明。
- **依赖**：新增 Rust/Python 依赖要说明理由；优先用已有的（`anyhow` / `serde_json` / `cpal` / `windows`）。
- **文档**：改了 `sainput.conf` 的键、子命令或默认值，同步更新 `sainput.conf.example` 与 `README.md`。
- **风格**：`cargo fmt` 跑一遍；命名和注释用中文或英文都行，但同一文件里保持一致。
- **不要**把 `Cargo.lock` 的无关变动、编辑器配置、个人脚本混进来。

提 PR 时按 `.github/PULL_REQUEST_TEMPLATE.md` 填。安全相关问题请走私密渠道（见 `SECURITY.md`），
不要开公开 issue。

---

## 6. 许可

提交代码即表示同意以 [`LICENSE`](LICENSE)（MIT）授权。
