# 鼠标延迟回归：确认装了鼠标钩子之后，鼠标**没有**被拖慢。
#
# 为什么必须有这条：低级鼠标钩子会让每一个鼠标事件等待钩子过程返回，
# 而钩子回调跑在钩子线程的消息循环里 —— 那条循环里任何 sleep 都会变成鼠标延迟。
# 我踩过：sainput 的消息循环里写了 sleep(40ms)，用户的鼠标立刻卡成幻灯片（p50 31ms），
# 而这件事**在任何日志里都看不出来**。所以必须有一条能把它量出来的回归。
#
# 判据：p50 < 5ms。
param([string]$Label = "lag")
$ErrorActionPreference = "Continue"
$root = Split-Path -Parent $PSScriptRoot
$py = $env:PYTHON
if (-not $py) { $py = (Get-Command python -ErrorAction SilentlyContinue).Source }
if (-not $py) { Write-Error '找不到 python：请把它放进 PATH，或设置 $env:PYTHON'; exit 1 }
$probe = Join-Path $root "tools\probe_mouse_lag.py"

Write-Output "=== 鼠标事件延迟（$Label）==="
# 只读检查：本脚本不杀任何进程
$running = [bool](Get-Process -Name 'sainput' -ErrorAction SilentlyContinue)
Write-Output "  sainput 在跑吗: $running"
$out = (& $py $probe 2>&1 | Out-String).Trim()
$code = $LASTEXITCODE
Write-Output "  测量: $out"

if ($code -eq 0) {
  Write-Output "  ✅ 正常（钩子没有拖住鼠标）"
} elseif ($code -eq 2) {
  Write-Output "  ❌ p50 >= 5ms —— 钩子消息循环里有人在 sleep 或干重活"
} else {
  Write-Output "  ⚠ 测量没成功（code=$code）"
}
Write-Output "done $Label"
exit $code
