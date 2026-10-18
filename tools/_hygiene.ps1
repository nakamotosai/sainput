# 测试卫生工具（供 tools\test_*.ps1 点源使用）
#
# 为什么需要：
#  1) 老脚本用 `Stop-Process -Name notepad` —— 会把**机器上所有记事本**杀掉，
#     包括用户没保存的工作。改成只杀「本脚本自己启动的那一个 PID」。
#  2) 老脚本用 Ctrl+A/Ctrl+C 读回屏幕，**覆写用户的真实剪贴板且不还原**；
#     有的只在成功路径还原，中途异常就丢了。
#  3) 测试注入的是全局右 Ctrl，会和正在跑的 sainput 抢同一个钩子事件。
#
# 用法：
#   . "$PSScriptRoot\_hygiene.ps1"
#   Initialize-TestHygiene           # 存剪贴板 + 检查生产实例
#   $np = Start-TestNotepad          # 起一个记事本，拿到它的 PID
#   ... 跑测试 ...
#   Restore-TestHygiene              # 还原剪贴板（finally 里必须调）
#   Stop-TestNotepad $np             # 只杀我们自己起的那个
#   Stop-SainputSafely -Id $p.Id     # 结束自己起的 sainput：先 release-keys，再按 PID 杀

$script:__savedClip = $null
$script:__savedClipOk = $false
$script:__testHygieneInited = $false

# 按窗口找记事本真实 PID 用（见 Start-TestNotepad 的注释：Win11 记事本是打包应用）
if (-not ('TestNotepadWin' -as [type])) {
    Add-Type @"
using System; using System.Runtime.InteropServices;
public class TestNotepadWin {
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string cls, string win);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
"@
}

function Initialize-TestHygiene {
    if ($script:__testHygieneInited) { return }
    $script:__testHygieneInited = $true

    Add-Type -AssemblyName System.Windows.Forms -ErrorAction SilentlyContinue

    # 1) 存剪贴板。
    #    局限（照实说）：只存**文本**。图片/富文本/HTML 等非文本格式这里救不回来，
    #    要保全格式得走 EnumClipboardFormats + GetClipboardData（本项目暂未实现）。
    try {
        $script:__savedClip = Get-Clipboard -Raw -ErrorAction Stop
        $script:__savedClipOk = $true
    } catch {
        Write-Warning "剪贴板读不出来（可能被别的进程占着），本次不还原"
    }

    # 2) 生产实例还开着的话，测试的全局右 Ctrl 会和它抢事件 —— 先提醒，别静默出错。
    #    这里只做「只读」检查：绝不按名字全局杀（强杀会丢 keyup，整块键盘会失灵）。
    $live = @(Get-Process -Name 'sainput' -ErrorAction SilentlyContinue)
    if ($live.Count -gt 0) {
        Write-Warning ("检测到正在运行的 sainput (PID " + ($live.Id -join ',') + ")。" +
                       '测试会注入全局右 Ctrl，两者会互抢钩子事件；' +
                       '请先右键托盘退出 sainput 再跑；卡住时用 sainput.exe release-keys。')
        Write-Warning "本脚本不会替你杀掉它（那是用户的日常实例）。"
    }
}

function Restore-TestHygiene {
    if (-not $script:__testHygieneInited) { return }
    if ($script:__savedClipOk) {
        try {
            Set-Clipboard -Value $script:__savedClip -ErrorAction Stop
            Write-Output "[hygiene] 用户剪贴板已还原"
        } catch {
            Write-Warning "剪贴板还原失败: $_"
        }
    } else {
        Write-Warning "[hygiene] 本次没有可还原的剪贴板（运行前没读到）"
    }
}

function Start-TestNotepad {
    # 用绝对路径起记事本：PATH 里若有别的 `notepad`（例如 Git for Windows 的
    # C:\Program Files\Git\usr\bin\notepad），裸名 `notepad` 在 PowerShell 里会
    # 报「无法完全运行此命令，因为系统找不到包含全部信息」。
    $exe = Join-Path $env:SystemRoot "System32\notepad.exe"
    if (-not (Test-Path $exe)) { $exe = "notepad.exe" }

    # 关键：Win11 的记事本是「打包应用」——System32\notepad.exe 只是个转发器，
    # 真正承载窗口的是 WindowsApps 下的 Notepad.exe，**PID 不一样**。
    # 所以 Start-Process -PassThru 拿到的 PID 杀不掉真身（会留残渣）。
    # 可靠做法：记录启动前后记事本 PID 的差集，只认新冒出来的那些。
    $before = @(Get-Process notepad -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
    Start-Process $exe | Out-Null

    $new = @()
    for ($i = 0; $i -lt 30 -and $new.Count -eq 0; $i++) {
        Start-Sleep -Milliseconds 200
        $now = @(Get-Process notepad -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
        $new = @($now | Where-Object { $before -notcontains $_ })
    }
    if ($new.Count -eq 0) {
        Write-Warning "没能识别出新起的记事本进程；本次不做清理，避免误杀别的记事本"
        return @()
    }
    return $new
}

function Stop-TestNotepad($pids) {
    # 只杀本次新起的那些 PID；不要再写 Stop-Process -Name notepad
    if ($null -ne $pids) {
        foreach ($id in @($pids)) {
            if ($id) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
        }
    }
}

function Stop-SainputSafely {
    param([int]$Id, [string]$Exe)
    # 结束「本脚本自己起的」sainput。两条铁律：
    #  1) 先补发 keyup：强杀会丢 keyup，系统以为修饰键一直按着，表现为整块键盘失灵。
    #  2) 只杀这个 PID。永远不要按进程名批量结束 sainput ——
    #     那会连用户日常在跑的实例一起杀掉。
    if (-not $Exe) { $Exe = $exe }
    if ($Exe -and (Test-Path -LiteralPath $Exe)) {
        & $Exe release-keys 2>$null | Out-Null
    }
    if ($Id -gt 0) {
        $proc = Get-Process -Id $Id -ErrorAction SilentlyContinue
        if ($proc -and $proc.ProcessName -eq 'sainput') {
            Stop-Process -Id $Id -ErrorAction SilentlyContinue
        }
    }
}
