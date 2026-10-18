# 热词/上下文偏置回归：确认 sherpa-onnx **没有跳过**热词。
#
# 为什么必须有这条：热词失效是**静默**的 —— sherpa 只在 C 层 stderr 打
#   "Encode hotwords failed, skipping"
# 然后照常识别，Python 侧不抛异常、不返回错误。也就是说热词可以坏掉很久而没人发现。
# （2026-10-17 实测：默认模型 x-asr-160ms 因为 bpe.vocab 不存在而退回 cjkchar，
#   hotwords.txt 一直是无效的，日志里连一行提示都没有。）
#
# 判定：启动一次 sidecar（带热词），stderr 里 "skipping"/"Cannot find ID" 必须为 0 条。
param([string]$Label = "hw")
$ErrorActionPreference = "Continue"
$root = Split-Path -Parent $PSScriptRoot
$py = $env:PYTHON
if (-not $py) { $py = (Get-Command python -ErrorAction SilentlyContinue).Source }
if (-not $py) { Write-Error '找不到 python：请把它放进 PATH，或设置 $env:PYTHON'; exit 1 }
$md = "$root\models\sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05"
$out = "$env:TEMP\hw_$Label.err"

# 写一个"一定有词"的临时热词表（不碰生产 hotwords.txt）
$hwf = "$env:TEMP\hw_$Label.txt"
[System.IO.File]::WriteAllText($hwf, "FREQUENTLY`n语音输入`nsainput`n", (New-Object System.Text.UTF8Encoding($false)))

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $py
$psi.Arguments = "`"$root\tools\asr_sidecar.py`" --model-dir `"$md`" --threads 2 --hotwords `"$hwf`" --hotwords-extra `"$hwf`""
$psi.RedirectStandardInput = $true
$psi.RedirectStandardError = $true
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute = $false
$psi.StandardErrorEncoding = [System.Text.Encoding]::UTF8
$proc = [System.Diagnostics.Process]::Start($psi)

# 等 ready
$deadline = (Get-Date).AddSeconds(300)
while ((Get-Date) -lt $deadline) {
  if ($proc.HasExited) { break }
  Start-Sleep -Milliseconds 500
  if ($proc.StandardOutput.Peek() -ge 0) { break }
}
Start-Sleep -Seconds 3
$proc.StandardInput.WriteLine('{"cmd":"quit"}')
$proc.StandardInput.Flush()
if (-not $proc.WaitForExit(20000)) { $proc.Kill() }
$err = $proc.StandardError.ReadToEnd()
[System.IO.File]::WriteAllText($out, $err, (New-Object System.Text.UTF8Encoding($false)))

$skip = ([regex]::Matches($err, 'skipping|Cannot find ID')).Count
Write-Output "=== 热词回归 ($Label) ==="
Write-Output "  stderr 落到: $out"
Write-Output "  热词被跳过/编码失败的行数 = $skip （必须为 0）"
if ($skip -eq 0) { Write-Output "  ✅ 热词被 sherpa 接受" } else { Write-Output "  ❌ 热词被静默跳过 —— 检查 modeling_unit / bpe_vocab 配对" }
Write-Output "done $Label"
