# 把新编出来的引擎装到正在跑的那个路径上 —— 不杀进程、不猜路径。
#
# 为什么需要它:sainput.exe 正在跑的时候 Windows 锁住镜像,`cargo build --release`
# 会以 `failed to remove file ...sainput.exe`(os error 5)失败 —— 那句容易被当成
# "代码没编过" ✗,其实**编译早就过了,只是最后一步换不上去**。
#
# 用法:
#   powershell -File tools/swap_engine.ps1                 # 用 target-test 里的新引擎
#   powershell -File tools/swap_engine.ps1 -From <exe路径> # 指定源
#   powershell -File tools/swap_engine.ps1 -Relaunch       # 换完顺手启动
# 前置:sainput 必须已经退出(托盘 → 右键 → 退出)。
param(
    [string]$From = "",
    [string]$To = "target\x86_64-pc-windows-gnu\release\sainput.exe",
    [switch]$Relaunch
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

if (-not $From) { $From = "target-test\x86_64-pc-windows-gnu\release\sainput.exe" }
$src = Join-Path $root $From
$dst = Join-Path $root $To

# 1) 还有在跑的直接退出:不猜、不杀(杀掉会把用户正在说的那句丢掉)
$running = @(Get-CimInstance Win32_Process -Filter "Name='sainput.exe'" -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    Write-Host "还有 $($running.Count) 个 sainput.exe 在跑(pid $($running.ProcessId -join ', '))" -ForegroundColor Yellow
    Write-Host "  先在托盘右键 → 退出,然后重跑本脚本。" -ForegroundColor Yellow
    exit 2
}
if (-not (Test-Path $src)) { Write-Host "找不到新引擎:$src" -ForegroundColor Red; exit 3 }
if (-not (Test-Path $dst)) { Write-Host "找不到目标:$dst" -ForegroundColor Red; exit 4 }

# 2) 留一份旧的再换(换错了能一句话退回去)
$bak = "$dst.bak"
Copy-Item $dst $bak -Force
Copy-Item $src $dst -Force
if (-not (Compare-Object (Get-FileHash $src).Hash (Get-FileHash $dst).Hash)) {
    Write-Host "已换上新引擎:$dst" -ForegroundColor Green
    Write-Host "旧的可退回:$bak"
} else {
    Write-Host "⚠ 换完内容对不上,别启动,先查一下" -ForegroundColor Yellow
    exit 5
}
if ($Relaunch) { Start-Process $dst; Write-Host "已启动。" -ForegroundColor Green }
exit 0
