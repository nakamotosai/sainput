# sainput — low-latency, fully local voice input for Windows

`sainput` turns speech into text at the caret of whatever window you are typing in.
Hold a hotkey, speak, release — words appear as you talk, with automatic punctuation
and digit normalization. Speech recognition and punctuation run entirely on your CPU;
**nothing is sent over the network at runtime**.

- Platform: Windows 10 / 11 **x64** only
- License: **MIT**
- Stack: Rust (hotkey hook, audio capture, streaming injection, overlay, tray) +
  Python sidecar (sherpa-onnx streaming ASR + CT-Transformer punctuation)

## Why local

Voice input is the most intimate text stream you produce: messages, notes, code comments,
credentials read aloud. A local pipeline means no audio ever leaves the machine, no account,
no quota, no per-word cost, and no dependency on a vendor's endpoint staying up.
The trade-off is explicit: you download model weights yourself, and first-token latency
(~0.8 s) is bounded by model chunk size — currently the smallest Chinese streaming chunk in
sherpa-onnx is 160 ms.

## Requirements

| Item | Requirement |
|---|---|
| OS | Windows 10 / 11 x64 |
| Rust | 1.91+ (MSVC, or `x86_64-pc-windows-gnu` with w64devkit) |
| Python | 3.11+, **64-bit**; `pip install -r requirements.txt` (`requirements-dev.txt` adds `soundfile` for offline bench scripts only) |
| Models | Not bundled: `powershell -File scripts/fetch_models.ps1` (`-Variant 160ms\|480ms\|960ms\|nemotron\|bilingual\|zh14m\|all`) |

## Build & run

```powershell
git clone <repo-url> sainput
cd sainput
pip install -r requirements.txt
powershell -ExecutionPolicy Bypass -File scripts/fetch_models.ps1
build.bat build --release
start.bat
```

`start.bat` launches the built binary, the console hides itself, and a tray icon becomes the
only visible sign that it is running. Exit via **right-click tray → Exit**. Put the caret in a
window that accepts typing (Notepad, browser, WeChat, VS Code), hold **right Ctrl**, speak,
release. Use `sainput run --show-console` if you want to watch the debug console.

Copy `sainput.conf.example` to `sainput.conf` (and optionally `hotwords.example.txt` to
`hotwords.txt`); both are optional, defaults work without them.

## Key features

- **Streaming Chinese/English transcription**, word-by-word insertion via `SendInput`
- **Automatic punctuation** (，。？！) and conservative Arabic-digit normalization
- **Hotwords** for names/jargon (`hotwords.txt`), plus **context biasing** from the text
  before the caret
- **Silence-based sentence splitting** (RMS energy timing, not semantic VAD): 5 / 10 / 15 / 30 s
  or off, default **15 s**
- **Overlay, 4 mutually exclusive modes**: off / thin bar / floating orb with a live level
  waveform / **screen-edge glow (default)**, with five glow colors
- **Tray-resident**: switch hotkey (including mouse side buttons X1/X2), punctuation-to-space,
  `A P I` → `API` merging, case fix, leading space, autostart — all applied live
- **Single instance**, autostart, per-session logs, and a `release-keys` rescue command that
  re-sends key-up for stuck modifier keys
- Release-key tail handling so the last word is not swallowed when you let go

## Command line

```
sainput devices | record | probe | replay | render | config | overlay-demo
        release-keys | run | selftest inject
```

Run `sainput run` with no arguments for defaults; `sainput --help` lists every switch
(`--hotkey`, `--toggle`, `--model-dir`, `--punct-mode`, `--ctx-bias`, `--merge-terms`,
`--fix-case`, `--rule1`/`--rule2`/`--rule3`, `--throttle-ms`, `--tail-ms`,
`--tail-quiet-ms`, `--tail-max-ms`, `--tail-pad-ms`, `--tail-rms`, `--overlay-mode`,
`--edge-tint`, `--log-dir`, `--show-console`, …).

Note: `--no-stream` is **not implemented** — passing it currently types nothing. Do not use it.

## Models

Default is a 160 ms-chunk streaming zipformer transducer (Chinese + English, int8).
`scripts/fetch_models.ps1` downloads it plus the punctuation model and verifies sizes;
other variants are selectable with `-Variant`. `sainput` never downloads models at runtime,
and this repository ships **no model weights**.

- Model selection, measurements and per-model trade-offs: [`docs/model-selection.md`](docs/model-selection.md)
- Upstream URLs, sizes and license status: [`models/README.md`](models/README.md),
  [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)

## Known limitations

- Windows only; x64 only.
- Some windows reject injection: elevated (UIPI) apps, some games, some terminals.
- ~200–560 ms between releasing the key and the final sentence settling (tail padding).
- Punctuation during streaming is deliberately conservative; sentence-final marks land on finalize.
- Streaming chunk size (≥160 ms for Chinese) sets a floor on first-token latency.
- Rare repetition loops are a model behavior and cannot be tuned away entirely.

Troubleshooting (first run, Python deps, missing models, UIPI windows, stuck keys,
console residue): [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md).

## Privacy

ASR and punctuation run offline. Logs (`logs/`) and `.ctx_hotwords.txt` contain your input
text and stay local; both are git-ignored, as are `sainput.conf`, `hotwords.txt`, and `models/`.
No telemetry, no accounts.

## License

MIT — see `LICENSE`. Third-party components and model licenses: `THIRD_PARTY_NOTICES.md`.
