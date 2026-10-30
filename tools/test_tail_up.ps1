# 真实热键路径回归：真按右 Ctrl、真松开（走 HookEvent::Up），看尾巴有没有收回来。
param([string]$Label = "new", [string]$Hold = "3900", [string]$ExtraArgs = "")
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
$dir = "$env:TEMP\up\$Label"
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$extra = @(); if ($ExtraArgs.Trim()) { $extra = $ExtraArgs.Trim() -split " " | Where-Object { $_ } }
$a = @("run","--feed",$wav,"--hotkey","rightctrl","--no-tray","--no-overlay","--dry-run","--log-dir",$dir) + $extra
$p = Start-Process -FilePath $exe -ArgumentList $a -PassThru -WindowStyle Hidden `
      -RedirectStandardOutput "$dir\out.txt" -RedirectStandardError "$dir\err.txt"
for ($i=0; $i -lt 120; $i++) { Start-Sleep -Milliseconds 500; if ((Get-Content "$dir\out.txt" -Raw -Encoding UTF8 -ErrorAction SilentlyContinue) -match "就绪") { break } }
Write-Output "----- $Label : 真按下 → 保持 $Hold ms → 真松开 -----"
[K]::Key(0xA3, $false)
Start-Sleep -Milliseconds ([int]$Hold)
[K]::Key(0xA3, $true)
Start-Sleep -Seconds 4
$log = Get-ChildItem "$dir\*.log" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($log) {
  $lines = Get-Content $log.FullName -Encoding UTF8
  $lines | Where-Object { $_ -match "^T " } | ForEach-Object { Write-Output "  $_" }
  $lines | Where-Object { $_ -match "^F " } | ForEach-Object { Write-Output "  $_" }
  Write-Output ("  控制台: " + ((Get-Content "$dir\out.txt" -Encoding UTF8 | Where-Object { $_ -match "松开|尾巴" }) -join ' | '))
} else { Write-Output "  (没有日志)" }
# 先 release-keys 再按 PID 结束（见 _hygiene.ps1）；绝不按名字全局杀 sainput
Stop-SainputSafely -Id $p.Id
Write-Output "done $Label"
