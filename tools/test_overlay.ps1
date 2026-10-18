$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
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

function Shot($path) {
  $v = [System.Windows.Forms.SystemInformation]::VirtualScreen
  $bmp = New-Object System.Drawing.Bitmap($v.Width, $v.Height)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($v.Left, $v.Top, 0, 0, $bmp.Size)
  $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
  Write-Output "shot -> $path ($($v.Width)x$($v.Height))"
}

. "$PSScriptRoot\_hygiene.ps1"
Initialize-TestHygiene
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\x86_64-pc-windows-gnu\release\sainput.exe"
if (-not (Test-Path -LiteralPath $exe)) { $exe = Join-Path $root "target\release\sainput.exe" }
if (-not (Test-Path -LiteralPath $exe)) { Write-Error "找不到 sainput.exe：请先在项目根执行 cargo build --release"; exit 1 }
$log = "$env:TEMP\sainput_overlay.log"
# 截图与日志都落在临时目录：以前把 t_ov_*.png 直接吐在项目根，属于测试残渣
$shotDir = "$env:TEMP\sainput_overlay_shots"
$runDir = "$env:TEMP\sainput_overlay_logs"
$np = $null
$p = $null
try {
Remove-Item -LiteralPath $log -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force $shotDir, $runDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $shotDir, $runDir | Out-Null

$np = Start-TestNotepad
Write-Output "notepad up"

$p = Start-Process -FilePath $exe -ArgumentList @("run","--log-dir",$runDir) -PassThru -WindowStyle Hidden -RedirectStandardOutput $log -RedirectStandardError "$env:TEMP\sainput_overlay.err"
Write-Output "sainput run pid=$($p.Id)"

for ($i = 0; $i -lt 90; $i++) {
  Start-Sleep -Milliseconds 500
  if (-not (Get-Process -Id $p.Id -ErrorAction SilentlyContinue)) { Write-Output "PROCESS EXITED EARLY"; break }
  $txt = Get-Content $log -Raw -Encoding UTF8 -ErrorAction SilentlyContinue
  if ($txt -match "就绪") { break }
}
Write-Output "--- 启动日志 ---"
Get-Content $log -Encoding UTF8 -ErrorAction SilentlyContinue

Start-Sleep -Seconds 2
Shot "$shotDir\t_ov_idle.png"

Write-Output "--- 按下 右Ctrl ---"
[K]::Key(0xA3, $false)
Start-Sleep -Milliseconds 2500
Shot "$shotDir\t_ov_rec.png"

Write-Output "--- 抬起 右Ctrl ---"
[K]::Key(0xA3, $true)
Start-Sleep -Seconds 5
Shot "$shotDir\t_ov_after.png"

Write-Output "--- 运行日志 ---"
Get-Content $log -Encoding UTF8 -ErrorAction SilentlyContinue
$e = Get-Content "$env:TEMP\sainput_overlay.err" -Raw -Encoding UTF8 -ErrorAction SilentlyContinue
if ($e -and $e.Trim()) { Write-Output "--- stderr ---"; Write-Output $e }

} finally {
  # 先 release-keys 再按 PID 结束（见 _hygiene.ps1）；绝不按名字全局杀 sainput
  if ($p) { Stop-SainputSafely -Id $p.Id }
  Stop-TestNotepad $np
  Restore-TestHygiene
}
Write-Output "done"