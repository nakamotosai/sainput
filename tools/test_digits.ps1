# 纯数字句回归:读验证码那种「整句一个数字串」。
#
# 判据(两条都是用户要的):
#   1) 汉字数字串要变成阿拉伯数字:说「一二三四五」→ 屏幕上是 12345;
#   2) **两边都不许有空格**:光标前已经有字("X")也不补前置空格,句尾也不许留空格
#      —— 用户的原话是「我想直接读验证码能贴上去」。
# 同时跑一条**正控**:普通中文句子仍旧按老规矩补前置空格(X 后面要有空格),
# 免得「修数字」把补空格那条功能一起关掉。
#
# 怎么跑:powershell -File tools/test_digits.ps1
$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class DG {
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] p, int cb);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
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
if (-not (Test-Path -LiteralPath $exe)) { Write-Error "找不到 sainput.exe:请先在项目根执行 cargo build --release"; exit 1 }

# 素材 1:纯数字串(用 TTS 现合成;模型自带的 test_wavs 里没有「念一串数字」这种)
$wavDigits = Join-Path $root "tmp\digits.wav"
if (-not (Test-Path -LiteralPath $wavDigits)) {
  & powershell -NoProfile -File (Join-Path $PSScriptRoot "make_test_wav.ps1") -Out $wavDigits -Zh "一二三四五" -En " " | Out-Null
}
# 素材 2(正控):普通中文句子
$wavText = Join-Path $root "models\sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05\test_wavs\2.wav"
if (-not (Test-Path -LiteralPath $wavText)) { Write-Warning "正控素材不存在,跳过正控:$wavText" }

$dir = "$env:TEMP\digits"
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dir | Out-Null

$np = Start-TestNotepad
if (-not $np -or @($np).Count -eq 0) { throw "没能启动记事本,测试中止" }
$h = [IntPtr]::Zero
for ($i = 0; $i -lt 40 -and $h -eq [IntPtr]::Zero; $i++) {
  Start-Sleep -Milliseconds 250
  $h = [DG]::FindByPid([int[]]@($np))
}
if ($h -eq [IntPtr]::Zero) { throw "没找到本次启动的记事本窗口,测试中止" }

function Read-Screen([IntPtr]$hwnd) {
  # 读回:先 Ctrl+A / Ctrl+C 拷到剪贴板(记事本 Win11 是 RichEditD2DPT,不吃 WM_GETTEXT)
  [DG]::SetForegroundWindow($hwnd) | Out-Null
  Start-Sleep -Milliseconds 400
  [System.Windows.Forms.SendKeys]::SendWait("^a")
  Start-Sleep -Milliseconds 250
  [System.Windows.Forms.SendKeys]::SendWait("^c")
  Start-Sleep -Milliseconds 500
  return (Get-Clipboard -Raw)
}

function Run-Case([string]$label, [string]$wav, [string]$doc) {
  [DG]::SetForegroundWindow($h) | Out-Null
  Start-Sleep -Milliseconds 600
  [System.Windows.Forms.SendKeys]::SendWait("^a")
  Start-Sleep -Milliseconds 200
  [System.Windows.Forms.SendKeys]::SendWait("{DEL}")
  Start-Sleep -Milliseconds 300
  if ($doc) { [System.Windows.Forms.SendKeys]::SendWait($doc); Start-Sleep -Milliseconds 400 }

  $a = @("run","--feed",$wav,"--feed-restart","--hotkey","rightctrl","--no-tray","--no-overlay","--log-dir",$dir)
  $p = Start-Process -FilePath $exe -ArgumentList $a -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$dir\out.txt" -RedirectStandardError "$dir\err.txt"
  for ($i=0; $i -lt 120; $i++) { Start-Sleep -Milliseconds 500; if ((Get-Content "$dir\out.txt" -Raw -Encoding UTF8 -ErrorAction SilentlyContinue) -match "就绪") { break } }
  [DG]::SetForegroundWindow($h) | Out-Null
  Start-Sleep -Milliseconds 500

  Write-Output "----- $label -----"
  [DG]::Key(0xA3, $false)
  Start-Sleep -Milliseconds 4500
  [DG]::Key(0xA3, $true)
  Start-Sleep -Seconds 6
  $screen = Read-Screen $h
  Write-Output ("屏幕内容 = [" + $screen + "]")
  Stop-SainputSafely -Id $p.Id
  Start-Sleep -Milliseconds 800
  return $screen
}

# 1) 纯数字串:光标前有 "X",期望正好是 "X12345"(左边没空格、句尾也没空格)
$got = Run-Case "纯数字串(期望 X12345)" $wavDigits "X"
$ok1 = ($got -ceq "X12345")
Write-Output ("数字串判定: " + $(if ($ok1) { "PASS" } else { "FAIL" }))
if (-not $ok1) {
  Write-Output "  左/右有空格都会让这条失败:先看 [X 12345] 这种(左空格)、[X12345 ] 这种(右空格)"
}

# 2) 正控:普通中文句子,期望 "X" 后面**有**空格(补空格那条功能没被改坏)
$ok2 = $null
if (Test-Path -LiteralPath $wavText) {
  $got2 = Run-Case "普通中文句(期望 X 后面有空格)" $wavText "X"
  $ok2 = $got2.StartsWith("X ")
  Write-Output ("补空格正控: " + $(if ($ok2) { "PASS" } else { "FAIL" }))
}

Restore-TestHygiene
Stop-TestNotepad $np
if ($ok1 -and ($ok2 -ne $false)) { Write-Output "done digits: PASS" } else { Write-Output "done digits: FAIL"; exit 1 }
