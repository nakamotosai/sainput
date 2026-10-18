# Changelog

本项目的所有重要变更都记在这里。

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added

- `LICENSE`（MIT）与第三方组件 / 模型许可清单 `THIRD_PARTY_NOTICES.md`。
- `README.en.md`（英文摘要）、`CONTRIBUTING.md`、`SECURITY.md`、`.github/` 仓库模板。
- `scripts/fetch_models.ps1`：按需下载默认 ASR 与标点模型到 `models/`（不覆盖已有目录）。
- `requirements.txt`：声明 sidecar 的 Python 依赖。
- `sainput.conf.example`、`hotwords.example.txt`、`.cargo/config.toml.example`、
  `rust-toolchain.example.toml`：可直接复制的示例配置。

### Changed

- `README.md` 面向公开用户重写：补齐安装 / 系统要求 / 隐私 / License 章节，
  按实际实现修正默认值（热键 `rightctrl`、`toggle = false`、`rule2` 默认 15 秒、
  悬浮默认屏幕边缘光）、补全命令行子命令与开关表。
- 文档中的平台、许可、构建步骤以「克隆后即可构建」为准，不再依赖任何单机环境。

### Fixed

- 麦克风流断开（设备被拔 / 被独占 / 驱动报错）时按「松手」收尾：淡出边缘光、
  丢弃失效的采集会话，下一次按下热键会重新开麦。修复了无输入时彩虹边缘光
  永久挂在屏幕上（60fps 全屏重绘白烧一个核）、之后每次按下都瞬间断流、
  输入法看起来「彻底卡死」的问题。采集流报过错但没断开时也会主动释放。
- 明确标注 `--no-stream` 未实现（当前传它会不打字），避免被当成可用开关。

### Removed

- 仓库不再跟踪个人配置、热词、日志、模型权重、内部开发记录与构建产物（仅加入
  `.gitignore`，本地文件保留）。

## [0.2.2] - 2026-10-18

### Added

- **悬浮显示三形态 + 边缘光**：细条 / 悬浮球（真实音量波形）/ 屏幕边缘光，四选一互斥
  （含「不显示」），默认屏幕边缘光；边缘光提供 5 种光色（白 / 彩虹 / 暖橙 / 青蓝 / 极光），
  并支持沿屏幕四周流动的彩色光。
- **托盘菜单**：换热键（含鼠标侧键 X1/X2）、标点转空格、英文拆字合并、上下文偏置、
  前置空格、悬浮显示方式与光色、英文小写修复、开机自启、静音断句门槛、释放卡住的键、
  显示日志窗口、退出。开关点击即生效并写回配置。
- **静音断句做成托盘开关**：5 / 10 / 15 / 30 秒 / 不断句，默认 15 秒。
- **`sainput release-keys`**：给所有修饰键补发 key-up 的安全网；托盘同款入口。
  配合按键状态闩，进程异常退出后不再留下「按键卡住、键盘失灵」的状态。
- 单实例保护、开机自启、`--log-dir`、`--hide-console` / `--show-console`。
- 新开关：`--punct-mode`、`--merge-terms`、`--fix-case`、`--ctx-bias`、
  `--leading-space`、`--rule1/2/3`、`--edge-tint`、`--ctx-hotwords`、`--python`。
- `tools/_hygiene.ps1`：测试前保存并在结束时还原剪贴板，只清理测试自己启动的进程。

### Changed

- 悬浮条固定到屏幕中下方、任务栏正上方（不再跟随插入符）。
- 默认模型为 160ms 分块的 x-asr 中英流式档。
- 标点在流式阶段保持保守，句尾标点等定稿再落笔。

### Fixed

- **上下文偏置 / 热词表静默失效**：热词从未真正传给识别器；现在按空表/非空表分流，
  并在需要时切到 `modified_beam_search`。
- **悬浮条 DPI 缩放**：高缩放比例下文字过小 —— 改为按窗口 DPI 换算尺寸。
- **长句卡顿**：主循环每轮只取一帧音频，长句时输入队列积压；改为每轮排空可用帧。
- **边缘光卡顿**：重绘频率过低（约 4fps）；改为按帧推进的缓动与流动。
- **边缘光残留**：关闭时留下一块空面板；空文本不再画面板。
- **打断路径吞字**：上一句尾巴未收完就再次按下时，补底噪步骤被跳过；统一走
  `finish_with_pad()`。
- 控制台收起后终端残留窗口。
- 流式上屏的安全阀（`max_backspace`）与空结果保护，避免误删已上屏内容。

## [0.2.1] - 2026-10-16

### Added

- 会话日志 T 行新增**累计丢帧**字段，便于区分「模型没听到」与「音频被丢」。
- 测试脚本统一接 `--log-dir`，不再污染生产日志目录。
- `tools/probe_latency.py` 新增 `--out`，默认写临时目录，不再覆写冻结产物。

### Fixed

- **打断路径补底噪**：尾巴未收完又被按下时，现在与正常路径一致地灌 480ms 底噪，
  最后一个字不再被吞。
- 测试脚本会误杀机器上所有同名进程、并覆盖用户剪贴板的问题（改由 `_hygiene.ps1` 兜底）。

## [0.2.0] - 2026-10-16

### Added

- 自动标点（CT-Transformer int8，中英通用）。
- 阿拉伯数字归一化（ITN）与轻量润色、重复循环修复。
- 热词表（`hotwords.txt`），非空时自动使用 `modified_beam_search`。
- 配置文件 `sainput.conf`（`key = value`），命令行优先。
- 悬浮状态条（当时为贴插入符的细条）。
- **松手尾巴**：松手后继续收真音频 → 补底噪 → 定稿；参数
  `tail_ms` / `tail_quiet_ms` / `tail_max_ms` / `tail_pad_ms` / `tail_rms`。
- 会话日志（partial / final / 尾巴三段），`replay`、`render`、`selftest` 子命令。
- 渲染节流 `throttle_ms`（默认 80ms）。

### Changed

- 默认模型从 2023 双语档换成 160ms 分块的中英流式档（旧档在真实中文上会重复打转）。

### Fixed

- 松手即定稿导致末字被吞（尾巴机制接管）。
- 空 final 不再清屏；改动过大时交给 final 处理。

## [0.1.0] - 2026-10

### Added

- 首个可用版本：全局低级键盘钩子取热键、cpal 采集、16kHz 定帧、
  Python sidecar 流式识别（JSON over stdio）、按公共前缀退格重打的逐字上屏。
- `devices` / `record` / `probe` / `run` 子命令，托盘图标与单实例运行。
- 按住热键说话、松开上屏；逐字出字的输入体验在 Windows 上落地。
