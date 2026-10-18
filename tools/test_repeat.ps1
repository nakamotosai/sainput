$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System; using System.Runtime.InteropServices; using System.Text;
public class W {
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Explicit, Size = 32)] public struct INPUTUNION { [FieldOffset(0)] public KEYBDINPUT ki; }
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION u; }
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
$log = "$env:TEMP\rep.log"
$np = $null
$p = $null
try {
Remove-Item -LiteralPath $log -ErrorAction SilentlyContinue
Remove-Item -LiteralPath "$env:TEMP\rep.err" -ErrorAction SilentlyContinue

$np = Start-TestNotepad
$h = [W]::FindWindowW("Notepad", $null)
Write-Output "notepad hwnd=$h"
[W]::Force($h) | Out-Null
Start-Sleep -Milliseconds 700
$fg = [W]::GetForegroundWindow()
Write-Output "前台=$fg  期望=$h  聚焦成功=$($fg -eq $h)"
[System.Windows.Forms.SendKeys]::SendWait("^a")
Start-Sleep -Milliseconds 200
[System.Windows.Forms.SendKeys]::SendWait("{DEL}")
Start-Sleep -Milliseconds 400

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\x86_64-pc-windows-gnu\release\sainput.exe"
if (-not (Test-Path -LiteralPath $exe)) { $exe = Join-Path $root "target\release\sainput.exe" }
if (-not (Test-Path -LiteralPath $exe)) { Write-Error "找不到 sainput.exe：请先在项目根执行 cargo build --release"; exit 1 }
# 相对路径：cargo 起进程时的工作目录不可靠，这里显式拼到项目根
$wav = Join-Path $root "models\sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20\test_wavs\0.wav"
$p = Start-Process -FilePath $exe -ArgumentList @("run","--feed",$wav,"--hotkey","rightctrl","--show-console","--log-dir","$env:TEMP\rep_logs") -PassThru -WindowStyle Hidden -RedirectStandardOutput $log -RedirectStandardError "$env:TEMP\rep.err"
for ($i=0; $i -lt 90; $i++) { Start-Sleep -Milliseconds 500; if ((Get-Content $log -Raw -Encoding UTF8 -ErrorAction SilentlyContinue) -match "就绪") { break } }
Write-Output "--- 启动完成，开始模拟「按住不放」（1 次真实按下 + 12 次自动重复）---"
[W]::Key(0xA3, $false)
for ($i=0; $i -lt 12; $i++) { Start-Sleep -Milliseconds 160; [W]::Key(0xA3, $false) }
Start-Sleep -Seconds 13
[W]::Key(0xA3, $true)
Start-Sleep -Seconds 6

Write-Output "--- run 日志 ---"
Get-Content $log -Encoding UTF8
$e = Get-Content "$env:TEMP\rep.err" -Raw -Encoding UTF8 -ErrorAction SilentlyContinue
if ($e -and $e.Trim()) { Write-Output "--- stderr ---"; Write-Output $e }

$n = (Get-Content $log -Encoding UTF8 | Select-String -Pattern "第 \d+ 句").Count
Write-Output "=== 「开始采集」出现次数 = $n （自动重复被正确丢弃时应为 1）==="

[W]::Force($h) | Out-Null
Start-Sleep -Milliseconds 600
[System.Windows.Forms.SendKeys]::SendWait("^a")
Start-Sleep -Milliseconds 300
[System.Windows.Forms.SendKeys]::SendWait("^c")
Start-Sleep -Milliseconds 600
Write-Output "--- Notepad 读回 ---"
Write-Output ("[" + (Get-Clipboard -Raw) + "]")
} finally {
  # 先 release-keys 再按 PID 结束（见 _hygiene.ps1）；绝不按名字全局杀 sainput
  if ($p) { Stop-SainputSafely -Id $p.Id }
  Stop-TestNotepad $np
  Restore-TestHygiene
}
Write-Output "done"