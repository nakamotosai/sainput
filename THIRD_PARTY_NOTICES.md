# 第三方组件与模型许可

`sainput` 本体以 MIT 许可发布（见 [`LICENSE`](LICENSE)）。本文列出运行期依赖的第三方组件
与可配合使用的模型，以及各自的许可状态。

> **本仓库不分发任何模型权重。** `models/` 在 `.gitignore` 中，模型由用户用
> `scripts/fetch_models.ps1` 或自行从上游下载。本仓库也不分发 `sherpa-onnx` 的二进制包，
> 它通过 `requirements.txt` 从 PyPI 安装。

## 1. 运行时依赖（Rust）

版本取自 `Cargo.lock`，许可取自各 crate 的 `Cargo.toml`。

| Crate | 版本 | 许可 | 用途 |
|---|---|---|---|
| [`anyhow`](https://crates.io/crates/anyhow) | 1.0.104 | MIT OR Apache-2.0 | 错误上下文 |
| [`base64`](https://crates.io/crates/base64) | 0.22.1 | MIT OR Apache-2.0 | sidecar 协议里传音频帧 |
| [`cpal`](https://crates.io/crates/cpal) | 0.15.3 | Apache-2.0 | WASAPI 采集 |
| [`serde_json`](https://crates.io/crates/serde_json) | 1.0.154 | MIT OR Apache-2.0 | 与 sidecar 的 JSON over stdio |
| [`windows`](https://crates.io/crates/windows) | 0.54.0 | MIT OR Apache-2.0 | Win32 / Direct2D / DirectWrite 绑定 |

选择了 MIT OR Apache-2.0 双许可的组件，本项目按 **MIT** 使用。

## 2. 运行时依赖（Python，ASR sidecar）

| 包 | 版本（实测） | 许可 | 用途 |
|---|---|---|---|
| [`sherpa-onnx`](https://pypi.org/project/sherpa-onnx/) | 1.13.x | Apache-2.0 | 流式识别 + 标点推理 |
| [`numpy`](https://pypi.org/project/numpy/) | 2.x | BSD-3-Clause | 音频帧与特征数组 |

`sherpa-onnx` 的推理后端为 [onnxruntime](https://github.com/microsoft/onnxruntime)（MIT），
随其 wheel 一起分发。

## 3. 语音引擎

| 组件 | 上游 | 许可 |
|---|---|---|
| sherpa-onnx | <https://github.com/k2-fsa/sherpa-onnx> | Apache-2.0 |

## 4. 模型

下表是 `models/` 里可能出现、以及文档中提到的模型。**许可状态以上游发布页为准**；
标「待确认」的模型，在确认前不建议随任何安装包分发。目录结构与本机放置要求见
[`models/README.md`](models/README.md)。

| 模型目录 | 上游 | 许可状态 |
|---|---|---|
| `sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20` | k2-fsa/sherpa-onnx releases（`asr-models`） | Apache-2.0 |
| `sherpa-onnx-streaming-zipformer-zh-14M-2023-02-23` | k2-fsa/sherpa-onnx releases（`asr-models`） | Apache-2.0 |
| `sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-80ms-int8-2026-06-11` | NVIDIA 模型卡 / 经 sherpa-onnx 转换发布 | 见 Hugging Face 上 NVIDIA 模型卡（含使用条款），以模型卡为准 |
| `sherpa-onnx-x-asr-160ms/480ms/960ms-...-2026-06-05` | <https://github.com/Gilgamesh-J/X-ASR>（经 sherpa-onnx 转换发布） | **许可待确认** |
| `sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8` | 上游 modelscope / FunASR，经 sherpa-onnx 转换为 onnx | 以 FunASR 上游发布页为准 |
| `sherpa-onnx-streaming-zipformer-zh-int8-2025-06-30` | k2-fsa/sherpa-onnx releases（`asr-models`） | **许可待确认** |

### 分发建议

- 本仓库**不随包分发任何模型**；`scripts/fetch_models.ps1` 只从上游下载。
- 若将来要发预编译包：`x-asr` 系列与 `streaming-zipformer-zh-int8-2025-06-30`
  在许可确认前**不建议**随包分发；其余模型请保留上游许可文件与出处。
- 再分发 `sherpa-onnx` wheel 时需同时附带其 Apache-2.0 许可与 NOTICE 要求。

## 5. 参考

- MIT 全文：[`LICENSE`](LICENSE)
- Apache-2.0：<https://www.apache.org/licenses/LICENSE-2.0>
- BSD-3-Clause：<https://opensource.org/license/bsd-3-clause>

发现许可标注有误或有遗漏，请开 issue 或在 PR 中直接修正本文件。
