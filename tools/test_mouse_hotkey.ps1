# 鼠标热键回归：验证侧键 X1 真的能当「按住说话」用。
#
# 为什么必须单独测：键盘钩子（WH_KEYBOARD_LL）**看不见鼠标事件**，
# 鼠标热键靠的是另一条 WH_MOUSE_LL。这条链路没测过就等于没做。
# 用 SendInput 合成 XBUTTON 事件（带 MOUSEEVENTF_XDOWN/XUP + XBUTTON1）。
param(
  [string]$Label = "mouse",
  [string]$Hotkey = "mousex1",
  # 合成哪个键：X1 / X2 / M（中键）
  [string]$XBtn = "X1"
)
$ErrorActionPreference = "Continue"
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
[StructLayout(LayoutKind.Sequential)]
public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
[StructLayout(LayoutKind.Explicit)]
public struct INPUTUNION { [FieldOffset(0)] public MOUSEINPUT mi; }
[StructLayout(LayoutKind.Sequential)]
public struct INPUT { public uint type; public INPUTUNION u; }
public class M {
  [DllImport("user32.dll", SetLastError=true)]
  public static extern uint SendInput(uint n, INPUT[] p, int cb);
  public const uint MOUSEEVENTF_XDOWN = 0x0080;
  public const uint MOUSEEVENTF_XUP   = 0x0100;
  public const uint XBUTTON1 = 0x0001;
  public const uint XBUTTON2 = 0x0002;
  public static void X(bool down, uint which) {
    INPUT[] a = new INPUT[1];
    a[0].type = 0; // INPUT_MOUSE
    a[0].u.mi.dx = 0; a[0].u.mi.dy = 0;
    a[0].u.mi.mouseData = which;
    a[0].u.mi.dwFlags = down ? MOUSEEVENTF_XDOWN : MOUSEEVENTF_XUP;
    uint n = SendInput(1, a, Marshal.SizeOf(typeof(INPUT)));
    if (n != 1) Console.WriteLine("SendInput failed, err=" + Marshal.GetLastWin32Error());
  }
}
'@

. "$PSScriptRoot\_hygiene.ps1"
Initialize-TestHygiene
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\x86_64-pc-windows-gnu\release\sainput.exe"
if (-not (Test-Path -LiteralPath $exe)) { $exe = Join-Path $root "target\release\sainput.exe" }
if (-not (Test-Path -LiteralPath $exe)) { Write-Error "找不到 sainput.exe：请先在项目根执行 cargo build --release"; exit 1 }
$wav = Join-Path $root "models\sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05\test_wavs\2.wav"
$dir = "$env:TEMP\mousehk\$Label"
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dir | Out-Null

$a = @("run","--feed",$wav,"--feed-restart","--hotkey",$Hotkey,"--no-tray","--no-overlay","--log-dir",$dir)
$p = Start-Process -FilePath $exe -ArgumentList $a -PassThru -WindowStyle Hidden `
      -RedirectStandardOutput "$dir\out.txt" -RedirectStandardError "$dir\err.txt"
for ($i=0; $i -lt 120; $i++) { Start-Sleep -Milliseconds 500; if ((Get-Content "$dir\out.txt" -Raw -Encoding UTF8 -ErrorAction SilentlyContinue) -match "就绪") { break } }

Write-Output "----- $Label : 合成 $Hotkey 按下 → 保持 4.8s → 抬起 -----"
$which = if ($XBtn -eq "X2") { [M]::XBUTTON2 } else { [M]::XBUTTON1 }
if ($XBtn -eq "M") {
  # 中键：没有 XDOWN，走 MOUSEEVENTF_MIDDLEDOWN/UP
  Write-Output "  (中键合成未实现，跳过)"
  [M]::X($true, $which)
} else {
  [M]::X($true, $which)
}
Start-Sleep -Milliseconds 4800
[M]::X($false, $which)
Start-Sleep -Seconds 6

Write-Output "--- 控制台（应看到「开始采集」和上屏回执）---"
Get-Content "$dir\out.txt" -Encoding UTF8 | Select-String "开始采集|✔|松开"
Write-Output "--- 日志 T/F ---"
$log = Get-ChildItem "$dir\*.log" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($log) { Get-Content $log.FullName -Encoding UTF8 | Where-Object { $_ -match "^[TF] " } }
# 先 release-keys 再按 PID 结束（见 _hygiene.ps1）；绝不按名字全局杀 sainput
Stop-SainputSafely -Id $p.Id
Write-Output "done $Label"
