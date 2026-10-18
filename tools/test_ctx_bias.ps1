# 上下文偏置回归：跑两句，验第二句开始时偏置词文件已带上第一句的术语。
# 判定：跑完后 .ctx_hotwords.txt 里出现第一句里的英文术语（例如 FREQUENTLY）。
param([string]$Label = "ctx")
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
$bias = Join-Path $root ".ctx_hotwords.txt"
$dir = "$env:TEMP\ctxbias\$Label"
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Remove-Item -LiteralPath $bias -ErrorAction SilentlyContinue

$a = @("run","--feed",$wav,"--feed-restart","--no-tray","--no-overlay","--no-stream","--verbose-ctx","--log-dir",$dir)
$p = Start-Process -FilePath $exe -ArgumentList $a -PassThru -WindowStyle Hidden `
      -RedirectStandardOutput "$dir\out.txt" -RedirectStandardError "$dir\err.txt"
for ($i=0; $i -lt 120; $i++) { Start-Sleep -Milliseconds 500; if ((Get-Content "$dir\out.txt" -Raw -Encoding UTF8 -ErrorAction SilentlyContinue) -match "就绪") { break } }

Write-Output "----- $Label : 第 1 句（此时偏置文件应为空）-----"
[K]::Key(0xA3, $false)
Start-Sleep -Milliseconds 5200
[K]::Key(0xA3, $true)
Start-Sleep -Seconds 6
Write-Output ("  偏置文件第 1 句后: [" + ((Get-Content $bias -Encoding UTF8 -ErrorAction SilentlyContinue | Where-Object { $_ -notmatch '^#' }) -join ',') + "]")

Write-Output "----- $Label : 第 2 句（应带上第 1 句的术语 FREQUENTLY）-----"
[K]::Key(0xA3, $false)
Start-Sleep -Milliseconds 5200
[K]::Key(0xA3, $true)
Start-Sleep -Seconds 6

Write-Output "--- 偏置提示行（Rust 侧）---"
Get-Content "$dir\out.txt" -Encoding UTF8 | Select-String "上下文偏置"
Write-Output "--- sidecar 偏置行 + 热词错误（必须 0 条 skipping）---"
$errl = Get-Content "$dir\err.txt" -Encoding UTF8 -ErrorAction SilentlyContinue
($errl | Select-String "上下文偏置")
$skip = ($errl | Select-String "skipping|Cannot find ID").Count
Write-Output "  热词被跳过/编码失败的行数 = $skip （必须为 0）"
Write-Output "--- 偏置文件内容（第 2 句开始时抓拍）---"
if (Test-Path $bias) { Get-Content $bias -Encoding UTF8 } else { Write-Output "（文件不存在）" }
Write-Output "--- 本会话定稿 ---"
$log = Get-ChildItem "$dir\*.log" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($log) { Get-Content $log.FullName -Encoding UTF8 | Where-Object { $_ -match "^F " } }
# 先 release-keys 再按 PID 结束（见 _hygiene.ps1）；绝不按名字全局杀 sainput
Stop-SainputSafely -Id $p.Id
Write-Output "done $Label"
