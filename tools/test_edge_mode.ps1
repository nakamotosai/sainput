# 屏幕边缘光模式的完整回归：
#   ① 录音时窗口铺满屏幕（不是细条尺寸）
#   ② 松手后边缘光淡出，窗口最终隐藏
#   ③ 全程**不能**出现残留的空细条（用户报过的 bug）
param([string]$Wav = "")
$ErrorActionPreference = "Continue"
Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
[StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
[StructLayout(LayoutKind.Explicit, Size = 32)] public struct INPUTUNION { [FieldOffset(0)] public KEYBDINPUT ki; }
[StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION u; }
[StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
public class E {
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] p, int cb);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr c);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  public static void Key(ushort vk, bool up) {
    INPUT[] a = new INPUT[1];
    a[0].type = 1; a[0].u.ki.wVk = vk; a[0].u.ki.dwFlags = (uint)(up ? 2 : 0);
    SendInput(1, a, Marshal.SizeOf(typeof(INPUT)));
  }
  public static string Overlay() {
    string res = "(no window)";
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      StringBuilder sb = new StringBuilder(256); GetClassName(h, sb, 256);
      if (sb.ToString() == "SainputOverlayWnd") {
        RECT r; GetWindowRect(h, out r);
        res = "vis=" + IsWindowVisible(h) + " " + (r.R-r.L) + "x" + (r.B-r.T) + "@" + r.L + "," + r.T;
        return false;
      }
      return true;
    }, IntPtr.Zero);
    return res;
  }
}
"@
[void][E]::SetProcessDpiAwarenessContext([IntPtr](-4))

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
. "$PSScriptRoot\_hygiene.ps1"

$exe = Join-Path $root "target\x86_64-pc-windows-gnu\release\sainput.exe"
if (-not (Test-Path -LiteralPath $exe)) { $exe = Join-Path $root "target\release\sainput.exe" }
if (-not (Test-Path -LiteralPath $exe)) { Write-Error "找不到 sainput.exe：请先在项目根执行 cargo build --release"; exit 1 }

# 红线：绝不按名字全局杀 sainput（会连用户日常实例一起杀掉，而且强杀会丢 keyup）。
# 本测试要独占键盘钩子，所以要求用户先自己从托盘退出。
$live = @(Get-Process -Name 'sainput' -ErrorAction SilentlyContinue)
if ($live.Count -gt 0) {
  Write-Warning ("检测到正在运行的 sainput (PID " + ($live.Id -join ',') + ")。")
  Write-Warning "请先右键托盘退出 sainput，再重跑本脚本；卡住时用 sainput.exe release-keys。"
  exit 2
}

if ([string]::IsNullOrWhiteSpace($Wav)) {
  $parts = @('models','sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05','test_wavs','0.wav')
  $Wav = Join-Path $root ($parts -join [IO.Path]::DirectorySeparatorChar)
}
Write-Output "WAV: $Wav"

$conf = Join-Path $root "sainput.conf"
$orig = [IO.File]::ReadAllText($conf, [Text.UTF8Encoding]::new($false))
$lines = $orig -split "`n"
for ($i=0; $i -lt $lines.Count; $i++) {
  if ($lines[$i] -match '^\s*overlay_mode\s*=') { $lines[$i] = 'overlay_mode = edge' }
}
[IO.File]::WriteAllText($conf, ($lines -join "`n"), [Text.UTF8Encoding]::new($false))

$argline = 'run --hide-console --no-tray --hotkey rightctrl --feed "' + $Wav + '"'
Write-Output ("启动: " + $argline)
$p = Start-Process -FilePath $exe -ArgumentList $argline -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 12

Write-Output "--- 按下前 ---"
Write-Output ("  " + [E]::Overlay())

[E]::Key(0xA3, $false)
Start-Sleep -Milliseconds 1200
Write-Output "--- 录音中（应为全屏）---"
Write-Output ("  " + [E]::Overlay())

[E]::Key(0xA3, $true)
foreach ($ms in @(600, 1400, 2200, 3200, 4600, 6000)) {
  Start-Sleep -Milliseconds 800
  Write-Output ("  松手+" + $ms + "ms: " + [E]::Overlay())
}

# 先 release-keys 再按 PID 结束（见 _hygiene.ps1）；绝不按名字全局杀 sainput
Stop-SainputSafely -Id $p.Id
[IO.File]::WriteAllText($conf, $orig, [Text.UTF8Encoding]::new($false))
Write-Output "config restored"
