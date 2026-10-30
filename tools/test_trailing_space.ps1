# 句尾空格回归:验证「句尾补一个空格」真的落到窗口里,而且**只补一个**。
#
# 为什么必须端到端测:这个空格是在 writer 的记账里加进去的(`want()`),
# 单测验证得了算术,验证不了「真的进了记事本」。这里用固定的 wav 喂给 sainput,
# 合成右 Ctrl 触发一次,再把记事本内容读回来判。
#
# 判据:
#   · 默认(--trailing-space,默认真) → 屏幕内容以**恰好一个**空格结尾
#   · --no-trailing-space            → 屏幕内容**不**以空格结尾
#   · 前置空格同时开着时 → 两边各一个(句首空格 + 句尾空格),中间不出现两个
#
# 用法:  powershell -File tools\test_trailing_space.ps1
# ⚠️ 参数名不能叫 $Exe:PowerShell 变量名**不分大小写**,而下面 `$exe = Join-Path ...`
# 会把同名参数直接覆盖掉(踩过:传进来的路径被默认值顶掉,测的还是旧 exe)。
param(
  [switch]$NoTail,
  [string]$ExePath = ""
)
$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class TS {
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] p, int cb);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  // 只按「本脚本自己起的 PID」找记事本窗口,避免误用用户已经开着的记事本
  public static IntPtr FindByPid(int[] pids) {
    IntPtr res = IntPtr.Zero;
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      StringBuilder sb = new StringBuilder(256);
      GetClassName(h, sb, 256);
      if (sb.ToString() != "Notepad") return true;
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (Array.IndexOf(pids, (int)pid) >= 0) { res = h; return false; }
      return true;
    }, IntPtr.Zero);
    return res;
  }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Explicit, Size = 32)] public struct IU { [FieldOffset(0)] public KEYBDINPUT ki; }
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public IU u; }
  public static void Key(ushort vk, bool up) {
    INPUT[] a = new INPUT[1]; a[0].type = 1;
    a[0].u.ki.wVk = vk; a[0].u.ki.dwFlags = (uint)(up ? 2 : 0);
    SendInput(1, a, Marshal.SizeOf(typeof(INPUT)));
  }
}
'@

. "$PSScriptRoot\_hygiene.ps1"
Initialize-TestHygiene
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\x86_64-pc-windows-gnu\release\sainput.exe"
if (-not (Test-Path -LiteralPath $exe)) { $exe = Join-Path $root "target\release\sainput.exe" }
# -ExePath 用来测「隔离构建」的产物(正在跑的 exe 被占用、换不了的时候)。
# 跑用户实例的同时跑本测试是安全的:本测试用 --feed(不占麦)、--no-tray,且热键不同。
if ($ExePath) { $exe = $ExePath }
if (-not (Test-Path -LiteralPath $exe)) { Write-Error "找不到 sainput.exe:请先在项目根执行 cargo build --release"; exit 1 }
$wav = Join-Path $root "models\sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05\test_wavs\2.wav"
$label = if ($NoTail) { "notail" } else { "tail" }
$dir = "$env:TEMP\trail\$label"
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dir | Out-Null

# 起一个只属于本脚本的记事本(绝不按名字杀用户的记事本)
$np = Start-TestNotepad
if (-not $np -or @($np).Count -eq 0) { throw "没能启动记事本,测试中止" }
$h = [IntPtr]::Zero
for ($i = 0; $i -lt 40 -and $h -eq [IntPtr]::Zero; $i++) {
  Start-Sleep -Milliseconds 250
  $h = [TS]::FindByPid([int[]]@($np))
}
if ($h -eq [IntPtr]::Zero) { throw "没找到本次启动的记事本窗口,测试中止" }
[TS]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 700

# 文档留空、光标在行首:这样句首那个"前置空格"不会掺进来,
# 判据只看结尾(前置空格另有 test_leading_space.ps1 管)。
[System.Windows.Forms.SendKeys]::SendWait("^a")
Start-Sleep -Milliseconds 200
[System.Windows.Forms.SendKeys]::SendWait("{DEL}")
Start-Sleep -Milliseconds 300

# 必须显式指定 --hotkey rightctrl:用户的 conf 里是鼠标侧键,而这里合成的是右 Ctrl。
$tailFlag = if ($NoTail) { "--no-trailing-space" } else { "--trailing-space" }
# ⚠️ 参数写成**一个数组字面量**,不要 `$a = @(...)` 再 `$a += x` ——
# 本文件如果丢了 UTF-8 BOM(PS 5.1 会按 GBK 解中文注释,一个双字节字符就把换行吃掉),
# 赋值那行会被静默吞进上一行注释里,结果只把末尾这一个 flag 传给了 exe(踩过)。
$argList = @("run","--feed",$wav,"--feed-restart","--hotkey","rightctrl","--no-tray","--no-overlay","--log-dir",$dir,$tailFlag)
$p = Start-Process -FilePath $exe -ArgumentList $argList -PassThru -WindowStyle Hidden `
      -RedirectStandardOutput "$dir\out.txt" -RedirectStandardError "$dir\err.txt"
for ($i=0; $i -lt 120; $i++) { Start-Sleep -Milliseconds 500; if ((Get-Content "$dir\out.txt" -Raw -Encoding UTF8 -ErrorAction SilentlyContinue) -match "就绪") { break } }
[TS]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 500

Write-Output "----- $label : 按住右Ctrl 说话 5s -----"
[TS]::Key(0xA3, $false)
Start-Sleep -Milliseconds 5000
[TS]::Key(0xA3, $true)
Start-Sleep -Seconds 6

# 读回记事本内容(Ctrl+A / Ctrl+C + 剪贴板)
$saved = $null
try { $saved = Get-Clipboard -Raw -ErrorAction Stop } catch {}
[TS]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 400
[System.Windows.Forms.SendKeys]::SendWait("^a")
Start-Sleep -Milliseconds 250
[System.Windows.Forms.SendKeys]::SendWait("^c")
Start-Sleep -Milliseconds 500
$screen = Get-Clipboard -Raw
Write-Output "--- 屏幕内容(两侧加了括号,看得见结尾那个空格) ---"
Write-Output ("[" + $screen + "]")

$endOne = $screen -and $screen.EndsWith(" ")
$endTwo = $screen -and $screen.EndsWith("  ")
$endAnyWs = $screen -and ($screen -match "\s$")
Write-Output "以空格结尾: $endOne   以两个空格结尾: $endTwo"
if ($NoTail) {
  $ok = -not $endAnyWs
  Write-Output ("判据(--no-trailing-space):不以空格结尾 → " + $(if ($ok) { "通过" } else { "不通过" }))
} else {
  $ok = $endOne -and (-not $endTwo)
  Write-Output ("判据(--trailing-space):恰好一个空格结尾 → " + $(if ($ok) { "通过" } else { "不通过" }))
}

# 还原剪贴板
if ($null -ne $saved) { try { Set-Clipboard -Value $saved } catch {} }
# 先 release-keys 再按 PID 结束(见 _hygiene.ps1);绝不按名字全局杀 sainput
Stop-SainputSafely -Id $p.Id
Stop-TestNotepad $np
Write-Output "done $label $ok"
exit $(if ($ok) { 0 } else { 1 })
