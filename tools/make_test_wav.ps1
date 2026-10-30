# 合成一段「中文 + 英文单词」的 16k 单声道 WAV,给上屏回归当素材用。
#   powershell -NoProfile -File tools/make_test_wav.ps1 -Out tmp/zh-en.wav
#
# 为什么需要:模型自带的 test_wavs 里没有「中文夹英文单词」的素材,而「说到英文
# 整句就没了」这类 bug 只有带英文的音频才复现(见 FROZEN-v0.2.5.md)。
# ⚠️ 存成 UTF-8 **带 BOM**:PowerShell 5.1 按 ANSI 读 .ps1,没有 BOM 时
# 脚本里的中文会变成乱码并报「意外的标记」(HANDOFF 红线 5.3 的同类坑)。
# 做法:中文用 Huihui、英文单词用 Zira,各自合成成**无头 PCM**再拼起来,
# 最后手工写一个标准 44 字节 WAV 头(SAPI 的 SetOutputToAudioStream 不写 RIFF 头)。
param(
  [string]$Out = "tmp/zh-en.wav",
  [string]$Zh = "我们现在就在微信里面说一句英文的 TOKEN 看看它还在不在",
  [string]$En = "TOKEN"   # 留空 = 不加英文段
)
Add-Type -AssemblyName System.Speech
$fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(
  16000,
  [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,
  [System.Speech.AudioFormat.AudioChannel]::Mono)
$s = New-Object System.Speech.Synthesis.SpeechSynthesizer

function Say([string]$voice, [string]$text) {
  $ms = New-Object System.IO.MemoryStream
  $s.SelectVoice($voice)
  $s.SetOutputToAudioStream($ms, $fmt)
  $s.Speak($text)
  $s.SetOutputToNull()
  return $ms.ToArray()
}

# 英文段留空(或只给空白)就只出中文那一段 —— 纯数字串的回归要的是一段干净中文
$parts = @(Say "Microsoft Huihui Desktop" $Zh)
if ($En -and $En.Trim()) { $parts += Say "Microsoft Zira Desktop" $En }
$pcm = [byte[]]@($parts)

# $Out 允许绝对路径或相对仓库根:Join-Path 遇到绝对子路径会拼出畸形路径(PowerShell 5.1),
$full = if ([IO.Path]::IsPathRooted($Out)) { [IO.Path]::GetFullPath($Out) } else { [IO.Path]::GetFullPath((Join-Path (Get-Location) $Out)) }
$dir = Split-Path -Parent $full
if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
$w = New-Object System.IO.BinaryWriter([IO.File]::Create($full))
$w.Write([Text.Encoding]::ASCII.GetBytes("RIFF"))
$w.Write([int](36 + $pcm.Length))
$w.Write([Text.Encoding]::ASCII.GetBytes("WAVEfmt "))
$w.Write([int]16)
$w.Write([int16]1)      # PCM
$w.Write([int16]1)      # mono
$w.Write([int]16000)    # sample rate
$w.Write([int](16000 * 2))
$w.Write([int16]2)
$w.Write([int16]16)
$w.Write([Text.Encoding]::ASCII.GetBytes("data"))
$w.Write([int]$pcm.Length)
$w.Write([byte[]]$pcm, 0, $pcm.Length)
$w.Close()
Write-Output ("saved {0}  {1} bytes  {2:N2}s" -f $full, $pcm.Length, ($pcm.Length / 32000.0))
