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
$log = "$env:TEMP\sainput_run.log"
Remove-Item -LiteralPath $log -ErrorAction SilentlyContinue

$p = Start-Process -FilePath $exe -ArgumentList @("run","--log-dir","$env:TEMP\hotkey_logs") -PassThru -WindowStyle Hidden -RedirectStandardOutput $log -RedirectStandardError "$env:TEMP\sainput_run.err"
Write-Output "started run pid=$($p.Id)"

for ($i = 0; $i -lt 80; $i++) {
  Start-Sleep -Milliseconds 500
  if (-not (Get-Process -Id $p.Id -ErrorAction SilentlyContinue)) { Write-Output "进程已退出"; break }
  $txt = Get-Content $log -Raw -Encoding UTF8 -ErrorAction SilentlyContinue
  if ($txt -match "就绪") { break }
}
Write-Output "--- 启动日志 ---"
Get-Content $log -Encoding UTF8 -ErrorAction SilentlyContinue | Select-Object -First 6

Write-Output "--- 合成 右Ctrl 按下 (0xA3) ---"
[K]::Key(0xA3, $false)
Start-Sleep -Milliseconds 1500
Write-Output "--- 合成 右Ctrl 抬起 ---"
[K]::Key(0xA3, $true)
Start-Sleep -Seconds 3

Write-Output "--- run 运行日志（全部）---"
Get-Content $log -Encoding UTF8 -ErrorAction SilentlyContinue
$err = Get-Content "$env:TEMP\sainput_run.err" -Raw -Encoding UTF8 -ErrorAction SilentlyContinue
if ($err -and $err.Trim()) { Write-Output "--- stderr ---"; Write-Output $err }

# 先 release-keys 再按 PID 结束（见 _hygiene.ps1）；绝不按名字全局杀 sainput
Stop-SainputSafely -Id $p.Id
Write-Output "stopped"
