# 上屏端到端：--feed 喂 2.wav，3.872s 处模拟松手（此时最后一个字还在路上），
# 打开记事本、焦点对准它，看屏幕上**真实打出来**的字。
param([string]$Label = "new", [string]$ExtraArgs = "", [string]$Release = "3.872")
$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System; using System.Runtime.InteropServices;
[StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
[StructLayout(LayoutKind.Explicit, Size = 32)] public struct INPUTUNION { [FieldOffset(0)] public KEYBDINPUT ki; }
[StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION u; }
public class K {
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] p, int cb);
  [DllImport("user32.dll")] public static extern IntPtr FindWindowW(string cls, string win);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, IntPtr pid);
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool f);
  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
  public static void Key(ushort vk, bool up) {
    INPUT[] a = new INPUT[1];
    a[0].type = 1; a[0].u.ki.wVk = vk; a[0].u.ki.dwFlags = (uint)(up ? 2 : 0);
    SendInput(1, a, Marshal.SizeOf(typeof(INPUT)));
  }
  public static bool Force(IntPtr h) {
    uint target = GetWindowThreadProcessId(h, IntPtr.Zero);
    uint me = GetCurrentThreadId();
    AttachThreadInput(me, target, true);
    ShowWindow(h, 9);
    bool ok = SetForegroundWindow(h);
    AttachThreadInput(me, target, false);
    return ok;
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
$dir = "$env:TEMP\screen\$Label"
$np = $null
$p = $null
try {
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$np = Start-TestNotepad
$h = [K]::FindWindowW("Notepad", $null)
[K]::Force($h) | Out-Null
Start-Sleep -Milliseconds 700
Write-Output "记事本 hwnd=$h  前台=$([K]::GetForegroundWindow())  聚焦成功=$([K]::GetForegroundWindow() -eq $h)"
[System.Windows.Forms.SendKeys]::SendWait("^a")
Start-Sleep -Milliseconds 200
[System.Windows.Forms.SendKeys]::SendWait("{DEL}")
Start-Sleep -Milliseconds 400
$extra = @(); if ($ExtraArgs.Trim()) { $extra = $ExtraArgs.Trim() -split " " | Where-Object { $_ } }
$a = @("run","--feed",$wav,"--hotkey","rightctrl","--feed-release-at",$Release,"--no-tray","--no-overlay","--show-console","--log-dir",$dir) + $extra
$p = Start-Process -FilePath $exe -ArgumentList $a -PassThru -WindowStyle Hidden `
      -RedirectStandardOutput "$dir\out.txt" -RedirectStandardError "$dir\err.txt"
for ($i=0; $i -lt 120; $i++) { Start-Sleep -Milliseconds 500; if ((Get-Content "$dir\out.txt" -Raw -Encoding UTF8 -ErrorAction SilentlyContinue) -match "就绪") { break } }
[K]::Force($h) | Out-Null      # 重新抢回焦点（启动子进程可能抢走）
Start-Sleep -Milliseconds 400
Write-Output "----- $Label : 喂到 $Release s 自动松手 -----"
[K]::Key(0xA3, $false)
Start-Sleep -Seconds 9
[K]::Force($h) | Out-Null
Start-Sleep -Milliseconds 400
[System.Windows.Forms.SendKeys]::SendWait("^a")
Start-Sleep -Milliseconds 250
[System.Windows.Forms.SendKeys]::SendWait("^c")
Start-Sleep -Milliseconds 500
Write-Output ("屏幕内容: [" + (Get-Clipboard -Raw) + "]")
$log = Get-ChildItem "$dir\*.log" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($log) {
  $lines = Get-Content $log.FullName -Encoding UTF8
  $lines | Where-Object { $_ -match "^T " } | ForEach-Object { Write-Output "  $_" }
  $lines | Where-Object { $_ -match "^F " } | ForEach-Object { Write-Output "  $_" }
}
} finally {
  # 先 release-keys 再按 PID 结束（见 _hygiene.ps1）；绝不按名字全局杀 sainput
  if ($p) { Stop-SainputSafely -Id $p.Id }
  Stop-TestNotepad $np
  Restore-TestHygiene
}
Write-Output "done $Label"
