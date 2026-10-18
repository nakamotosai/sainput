# models/ —— 模型放这里（不进版本库）

本目录被 `.gitignore` 忽略。sainput 的识别、标点全部跑在本地模型上，
仓库本身**不带**任何模型权重，clone 之后必须自己下载一次。

## 怎么下载

```powershell
powershell -ExecutionPolicy Bypass -File scripts\fetch_models.ps1              # 默认档：160ms + 标点
powershell -ExecutionPolicy Bypass -File scripts\fetch_models.ps1 -Variant 480ms
powershell -ExecutionPolicy Bypass -File scripts\fetch_models.ps1 -Variant all -NoPunct
powershell -ExecutionPolicy Bypass -File scripts\fetch_models.ps1 -DryRun      # 只看要下什么，不联网
```

脚本会下载到 `models\<asset 名>\`，校验归档大小与必要文件，已存在且完整就跳过。
`-Variant` 可选：`160ms`（默认）/ `480ms` / `960ms` / `nemotron` / `bilingual` / `zh14m` / `all`。

不想用脚本就手动下：`https://github.com/k2-fsa/sherpa-onnx/releases`
（ASR 在 `asr-models` tag，标点在 `punctuation-models` tag），解压后保持
`models\<asset 名>\` 这种一层目录结构即可。用别的目录也行，改 `sainput.conf` 的
`model_dir` / `punct_dir`。

## 模型清单

| 用途 | 目录（asset 名） | 归档大小 | 适合 |
|---|---|---|---|
| ASR 默认 | `sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05` | 134,370,008 B | 最跟手（每 160ms 刷一次）；中英混说偶尔丢虚词 |
| ASR | `sherpa-onnx-x-asr-480ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05` | 未核实 | 英文更完整，但每次刷 3–4 个字、手感更顿 |
| ASR | `sherpa-onnx-x-asr-960ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05` | 未核实 | 最省 CPU，手感最差 |
| ASR | `sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-80ms-int8-2026-06-11` | 未核实（解压后约 653 MB） | 80ms 分块、首个字最快；0.6B 比较吃 CPU，需多线程（`threads = 8`） |
| ASR（旧档） | `sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20` | 511,274,346 B | 英文最好；中文长句容易重复打转，除非你几乎只讲英文 |
| ASR（轻量） | `sherpa-onnx-streaming-zipformer-zh-14M-2023-02-23` | 74,004,050 B | 体积最小、CPU 最省；英文差 |
| 标点 | `sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8` | 64,717,756 B | 中英标点，默认开启 |

> 归档大小中，160ms 与标点两条是对着上游 release 逐字节核实的；bilingual / zh14m
> 取自本地下载副本。标「未核实」的仅跑了必要文件校验，没有硬编码大小。

模型选型、延迟实测和「哪些方向已经证伪」见 [`../docs/model-selection.md`](../docs/model-selection.md)。

## 每个目录里必须有什么

- ASR：`tokens.txt` + `encoder*.onnx` + `decoder*.onnx` + `joiner*.onnx`
  （`bpe.model` 有热词时会用到；`test_wavs/` 可删）
- 标点：`model*.onnx`（外加 `tokens.json` / `config.yaml`，解压自带）
- 缺文件时 `sainput` 会直接报「找不到模型目录」或「标点模型不在 …，本次不加标点」。

## 许可

- **本仓库的 MIT 许可不覆盖模型权重。** 每个模型有各自的许可，分发/商用前必须
  自己看清楚：解压目录里的 `README.md`、上游 release 页、以及模型来源仓库。
- 已核实：`bilingual-zh-en-2023-02-20` 与 `zh-14M-2023-02-23` 的上游标注为 `apache-2.0`。
- `x-asr-160/480/960ms` 转换自 <https://github.com/Gilgamesh-J/X-ASR>，
  `punct-ct-transformer` 转换自 FunASR/ModelScope 上对应的中文标点模型；
  两者在归档内**都没有**写明许可，请到上游确认后再随产品分发。
- 因为许可与体积两方面的原因，这些权重一律不进 git。
