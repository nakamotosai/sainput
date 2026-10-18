# 前置空格回归：验证「光标前有字 → 补一个空格」真的落到窗口里。
#
# 为什么必须端到端测：这个功能要读目标窗口的光标位置，而读法依赖控件类型
# （RichEdit / Edit）。单测只能验证算术，验证不了"真窗口里读得到、打得进"。
#
# 判据：
#   · 记事本里已有 "ABC"，光标在末尾 → 上屏结果应以 " ABC" 开头（前面多一个空格）
#   · 记事本里是空的、光标在行首 → 上屏结果不应以空格开头
param(
  [string]$Label = "lead",
  [switch]$EmptyDoc
)
$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class LS {
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] p, int cb);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string c, string w);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  // 只按「本脚本自己起的 PID」找记事本窗口，避免误用用户已经开着的记事本
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
if (-not (Test-Path -LiteralPath $exe)) { Write-Error "找不到 sainput.exe：请先在项目根执行 cargo build --release"; exit 1 }
$wav = Join-Path $root "models\sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05\test_wavs\2.wav"
$dir = "$env:TEMP\lead\$Label"
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dir | Out-Null

# 起一个只属于本脚本的记事本。
# 以前这里按名字杀所有记事本 —— 会连用户没保存的内容一起杀掉，绝不允许。
$np = Start-TestNotepad
if (-not $np -or @($np).Count -eq 0) { throw "没能启动记事本，测试中止" }
$h = [IntPtr]::Zero
for ($i = 0; $i -lt 40 -and $h -eq [IntPtr]::Zero; $i++) {
  Start-Sleep -Milliseconds 250
  $h = [LS]::FindByPid([int[]]@($np))
}
if ($h -eq [IntPtr]::Zero) { throw "没找到本次启动的记事本窗口，测试中止" }
[LS]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 700

# 准备文档内容
[System.Windows.Forms.SendKeys]::SendWait("^a")
Start-Sleep -Milliseconds 200
[System.Windows.Forms.SendKeys]::SendWait("{DEL}")
Start-Sleep -Milliseconds 300
if (-not $EmptyDoc) {
  # 光标前放两个字，并把光标留在末尾
  [System.Windows.Forms.SendKeys]::SendWait("ABC")
  Start-Sleep -Milliseconds 400
}

# 必须显式指定 --hotkey rightctrl：用户的 conf 里可能是鼠标侧键，
# 而本测试合成的是右 Ctrl（不指定就会用 conf 里的键 -> 永远触发不了，我踩过一次）。
$a = @("run","--feed",$wav,"--feed-restart","--hotkey","rightctrl","--no-tray","--no-overlay","--log-dir",$dir)
$p = Start-Process -FilePath $exe -ArgumentList $a -PassThru -WindowStyle Hidden `
      -RedirectStandardOutput "$dir\out.txt" -RedirectStandardError "$dir\err.txt"
for ($i=0; $i -lt 120; $i++) { Start-Sleep -Milliseconds 500; if ((Get-Content "$dir\out.txt" -Raw -Encoding UTF8 -ErrorAction SilentlyContinue) -match "就绪") { break } }
[LS]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 500

Write-Output "----- $Label : 按住右Ctrl 说话 5s -----"
[LS]::Key(0xA3, $false)
Start-Sleep -Milliseconds 5000
[LS]::Key(0xA3, $true)
Start-Sleep -Seconds 6

# 读回记事本内容（Ctrl+A / Ctrl+C + 剪贴板）
$saved = $null
try { $saved = Get-Clipboard -Raw -ErrorAction Stop } catch {}
[LS]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 400
[System.Windows.Forms.SendKeys]::SendWait("^a")
Start-Sleep -Milliseconds 250
[System.Windows.Forms.SendKeys]::SendWait("^c")
Start-Sleep -Milliseconds 500
$screen = Get-Clipboard -Raw
Write-Output "--- 屏幕内容 ---"
Write-Output ("[" + $screen + "]")
$lead = if ($screen) { $screen.StartsWith(" ") } else { $false }
Write-Output "以空格开头: $lead"

# 还原剪贴板
if ($null -ne $saved) { try { Set-Clipboard -Value $saved } catch {} }
# 先 release-keys 再按 PID 结束（见 _hygiene.ps1）；绝不按名字全局杀 sainput
Stop-SainputSafely -Id $p.Id
Stop-TestNotepad $np
Write-Output "done $Label"
