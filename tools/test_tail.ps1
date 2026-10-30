# 尾巴回归测试：同一段语音、同一个「松手位置」，比较
#   ① 旧行为（松手立刻收尾，最后一个字丢掉）
#   ② 新行为（松手后收尾巴 + 补底噪，最后一个字还在）
# 2.wav 的末尾两个字是「频繁」，最后一个响帧在 3.968s。
param([string]$Release = "3.968")
$ErrorActionPreference = "Continue"
Add-Type @"
using System; using System.Runtime.InteropServices;
[StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
[StructLayout(LayoutKind.Explicit, Size = 32)] public struct INPUTUNION { [FieldOffset(0)] public KEYBDINPUT ki; }
[StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION u; }
public class K {
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] p, int cb);
  public static void Key(ushort vk, bool up) {
    INPUT[] a = new INPUT[1];
    a[0].type = 1; a[0].u.ki.wVk = vk; a[0].u.ki.dwFlags = (uint)(up ? 2 : 0);
    SendInput(1, a, Marshal.SizeOf(typeof(INPUT)));
  }
}
"@
. "$PSScriptRoot\_hygiene.ps1"
Initialize-TestHygiene
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\x86_64-pc-windows-gnu\release\sainput.exe"
if (-not (Test-Path -LiteralPath $exe)) { $exe = Join-Path $root "target\release\sainput.exe" }
if (-not (Test-Path -LiteralPath $exe)) { Write-Error "找不到 sainput.exe：请先在项目根执行 cargo build --release"; exit 1 }
$wav = Join-Path $root "models\sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05\test_wavs\2.wav"

function RunOne($label, $extra) {
  $dir = "$env:TEMP\tail\$label"
  Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $a = @("run","--feed",$wav,"--feed-release-at",$Release,"--no-tray","--no-overlay","--dry-run","--log-dir",$dir) + $extra
  $p = Start-Process -FilePath $exe -ArgumentList $a -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$dir\out.txt" -RedirectStandardError "$dir\err.txt"
  for ($i=0; $i -lt 120; $i++) {
    Start-Sleep -Milliseconds 500
    if ((Get-Content "$dir\out.txt" -Raw -Encoding UTF8 -ErrorAction SilentlyContinue) -match "就绪") { break }
  }
  [K]::Key(0xA3, $false)      # 按下右 Ctrl 开始喂；松手由 --feed-release-at 模拟
  Start-Sleep -Seconds 11
  # 先 release-keys 再按 PID 结束（见 _hygiene.ps1）；绝不按名字全局杀 sainput
  Stop-SainputSafely -Id $p.Id
  Start-Sleep -Milliseconds 600
  $log = Get-ChildItem "$dir\*.log" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1
  Write-Output "================ $label ================"
  if ($log) {
    $lines = Get-Content $log.FullName -Encoding UTF8
    $lines | Where-Object { $_ -match "^T " } | ForEach-Object { Write-Output "  $_" }
    $lines | Where-Object { $_ -match "^F " } | ForEach-Object { Write-Output "  $_" }
  } else { Write-Output "  (没有日志)" }
}

Write-Output "语音: 2.wav   松手位置: $($Release)s   末尾两字应为「频繁」"
RunOne "a-old" @("--tail-ms","0","--tail-max-ms","0","--tail-pad-ms","0")
RunOne "b-new" @()
Write-Output "done"
