# 托盘开关的**行为**回归:直接给托盘窗口发菜单命令,验证开关真的生效 + 落盘。
#
# 为什么不用截图:截图只能看长相,验证不了「点了之后配置文件有没有改、控制台有没有响应」。
# 这里模拟用户点菜单项:给 SainputTrayWnd 发 WM_COMMAND(id)。
#   ID_PUNCT_SPACE=10 / ID_MERGE_TERMS=11 / ID_CTX_BIAS=12 / ID_CTX_VERBOSE=13
#   ID_LEADING_SPACE=14 / ID_FIX_CASE=15 / ID_TRAILING_SPACE=16
# 每个开关都点**两次**(切走再切回),这样不管用户原本怎么设的,跑完都回到原值。
param([string]$Conf = "")
$ErrorActionPreference = "Continue"
$root = Split-Path -Parent $PSScriptRoot
if (-not $Conf) { $Conf = Join-Path $root "sainput.conf" }
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class TW {
  public delegate bool Cb(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(Cb cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  public static IntPtr Find() {
    IntPtr r = IntPtr.Zero;
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      StringBuilder sb = new StringBuilder(256);
      GetClassName(h, sb, 256);
      if (sb.ToString() == "SainputTrayWnd") { r = h; return false; }
      return true;
    }, IntPtr.Zero);
    return r;
  }
}
'@

$h = [TW]::Find()
if ($h -eq [IntPtr]::Zero) { Write-Output "❌ 找不到托盘窗口（sainput 在跑吗？）"; exit 1 }
Write-Output "托盘窗口 hwnd = $h"

function Get-ConfValue($key) {
  $txt = [System.IO.File]::ReadAllText($Conf, [System.Text.Encoding]::UTF8)
  foreach ($ln in $txt -split "`n") {
    $t = $ln.Trim()
    if ($t.StartsWith('#') -or $t.StartsWith(';')) { continue }
    $i = $t.IndexOf('=')
    if ($i -gt 0 -and $t.Substring(0, $i).Trim() -eq $key) {
      return $t.Substring($i + 1).Trim()
    }
  }
  return "(未设置)"
}

function Click-Item($id, $name) {
  # WM_COMMAND = 0x0111；wParam 低位 = 菜单 id
  [void][TW]::SendMessageW($h, 0x0111, [IntPtr]$id, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 700
  Write-Output "  点了「$name」"
}

Write-Output "--- 点之前 ---"
foreach ($k in @('punct_mode','merge_terms','ctx_bias')) {
  Write-Output ("  {0} = {1}" -f $k, (Get-ConfValue $k))
}

Click-Item 10 "标点转空格"
Write-Output ("  之后 punct_mode = " + (Get-ConfValue 'punct_mode') + "  （期望 space）")

Click-Item 11 "英文连写"
Write-Output ("  之后 merge_terms = " + (Get-ConfValue 'merge_terms') + "  （期望 false，因为默认 true 被切了）")

Click-Item 12 "上下文偏置"
Write-Output ("  之后 ctx_bias = " + (Get-ConfValue 'ctx_bias') + "  (期望 false)")

# 句尾空格:本来没这个键 → 点一下写 false,再点一下写 true(等于默认值)
Click-Item 16 "后面自动加空格"
Write-Output ("  之后 trailing_space = " + (Get-ConfValue 'trailing_space') + "  (期望 false)")
Click-Item 16 "后面自动加空格(切回)"
Write-Output ("  之后 trailing_space = " + (Get-ConfValue 'trailing_space') + "  (期望 true)")

# 切回默认,别把用户配置留在被改动状态
Click-Item 10 "标点转空格(切回)"
Click-Item 11 "英文连写(切回)"
Click-Item 12 "上下文偏置(切回)"
Write-Output "--- 切回后 ---"
foreach ($k in @('punct_mode','merge_terms','ctx_bias','trailing_space')) {
  Write-Output ("  {0} = {1}" -f $k, (Get-ConfValue $k))
}
Write-Output "done"
