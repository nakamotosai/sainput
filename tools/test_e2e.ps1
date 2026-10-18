$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName Microsoft.VisualBasic
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
function ReadNote {
  Start-Sleep -Milliseconds 500
  [System.Windows.Forms.SendKeys]::SendWait("^a")
  Start-Sleep -Milliseconds 300
  [System.Windows.Forms.SendKeys]::SendWait("^c")
  Start-Sleep -Milliseconds 600
  return (Get-Clipboard -Raw)
}
. "$PSScriptRoot\_hygiene.ps1"
Initialize-TestHygiene
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\x86_64-pc-windows-gnu\release\sainput.exe"
if (-not (Test-Path -LiteralPath $exe)) { $exe = Join-Path $root "target\release\sainput.exe" }
if (-not (Test-Path -LiteralPath $exe)) { Write-Error "找不到 sainput.exe：请先在项目根执行 cargo build --release"; exit 1 }
$wav = Join-Path $root "models\sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20\test_wavs\0.wav"
$log = "$env:TEMP\run_e2e.log"
$np = $null
$p = $null
try {
Remove-Item -LiteralPath $log -ErrorAction SilentlyContinue
Remove-Item -LiteralPath "$env:TEMP\run_e2e.err" -ErrorAction SilentlyContinue
$np = Start-TestNotepad
[System.Windows.Forms.SendKeys]::SendWait("^a")
Start-Sleep -Milliseconds 300
[System.Windows.Forms.SendKeys]::SendWait("{DEL}")
Start-Sleep -Milliseconds 500
Write-Output "=== 空文档读回（应为空）: [$(ReadNote)]"
$p = Start-Process -FilePath $exe -ArgumentList @("run","--log-dir","$env:TEMP\e2e_logs") -PassThru -WindowStyle Hidden -RedirectStandardOutput $log -RedirectStandardError "$env:TEMP\run_e2e.err"
for ($i = 0; $i -lt 90; $i++) { Start-Sleep -Milliseconds 500; if ((Get-Content $log -Raw -Encoding UTF8 -ErrorAction SilentlyContinue) -match "就绪") { break } }
Write-Output "--- 按下右Ctrl + 播放 0.wav ---"
[K]::Key(0xA3, $false)
Start-Sleep -Milliseconds 700
$sp = New-Object System.Media.SoundPlayer $wav
$sp.PlaySync()
Start-Sleep -Milliseconds 900
[K]::Key(0xA3, $true)
Start-Sleep -Seconds 6
Write-Output "--- run 日志 ---"
Get-Content $log -Encoding UTF8
$e = Get-Content "$env:TEMP\run_e2e.err" -Raw -Encoding UTF8 -ErrorAction SilentlyContinue
if ($e -and $e.Trim()) { Write-Output "--- stderr ---"; Write-Output $e }
Write-Output "--- Notepad 读回 ---"
Write-Output ("[" + (ReadNote) + "]")
} finally {
  # 无论中途出什么错都要收敛：还原用户剪贴板、只杀自己起的记事本与 sainput
  # 先 release-keys 再按 PID 结束（见 _hygiene.ps1）；绝不按名字全局杀 sainput
  if ($p) { Stop-SainputSafely -Id $p.Id }
  Stop-TestNotepad $np
  Restore-TestHygiene
}
Write-Output "done"