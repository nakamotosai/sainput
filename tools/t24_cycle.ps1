# T2.4 后半段:升级 / 降级 / 重复注册 / 残留 —— **一次升权跑完**。
#
# 为什么是一个脚本而不是几条命令:每一步都要写 HKLM,分次跑就等于让用户点好几遍 UAC。
# 这里把「装旧版 → 升级 → 再装一次(不许重复)」串起来,每步都 verify 出结论。
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/t24_cycle.ps1            # 干跑
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/t24_cycle.ps1 -Apply     # 需要管理员
#
# 判据(全部可机检):
#   1) 装旧版不炸:注册成功且版本号如实写下(升级/降级一眼看得出装的是哪份)
#   2) 升级:版本号与 DLL 路径都换成新的,`verify` 判 OK(不是 STALE)
#   3) 重复注册**不产生第二份**:LanguageProfile 下仍然只有一个子键
#   4) 收尾:系统里留的是**正式构建**(不是 target-test 里那份)
param(
  [string]$Dll = '',
  [string]$OldVersion = '0.0.1-old',
  [string]$NewVersion = '0.2.5',
  [string]$Clsid = '{FCC45A5E-09C4-410A-B696-BDD2DA66A21F}',
  [string]$ExpectDll = '',
  # 升权跑的时候 stdout 拿不到(另开一个高完整性进程),所以结论落盘 —— 给调用方读。
  [string]$Log = '',
  [switch]$Apply
)

$ErrorActionPreference = 'Stop'
if ($Log) { Start-Transcript -Path $Log -Force | Out-Null }
$reg = Join-Path $PSScriptRoot 'tsf_register.ps1'
if (-not $Dll) {
  $Dll = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\x86_64-pc-windows-gnu\release\sainput_tsf.dll'
}
if (-not $ExpectDll) { $ExpectDll = $Dll }
$TIP = "HKLM:\SOFTWARE\Microsoft\CTF\TIP\$Clsid"

$script:fail = 0
function Say([string]$m) { Write-Output $m }
function Verdict([string]$what, [bool]$ok, [string]$detail) {
  if (-not $ok) { $script:fail++ }
  Say ("[{0}] {1}{2}" -f ($(if ($ok) { 'PASS' } else { 'FAIL' })), $what, $(if ($detail) { " —— $detail" } else { '' }))
}

function Invoke-Reg([string]$action, [string]$version) {
  $args = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $reg, '-Action', $action, '-Apply',
            '-Dll', $Dll, '-ExpectDll', $ExpectDll)
  if ($version) { $args += @('-Version', $version) }
  $out = & powershell @args 2>&1 | Out-String
  return $out
}

function State-Version {
  if (-not (Test-Path $TIP)) { return '' }
  (Get-ItemProperty -Path $TIP -ErrorAction SilentlyContinue).SainputVersion
}
function State-Dll {
  $k = "HKLM:\SOFTWARE\Classes\CLSID\$Clsid\InProcServer32"
  if (-not (Test-Path $k)) { return '' }
  (Get-ItemProperty -Path $k -ErrorAction SilentlyContinue).'(default)'
}
function Profile-Count {
  $p = "$TIP\LanguageProfile"
  if (-not (Test-Path $p)) { return -1 }
  @(Get-ChildItem -Path $p -Recurse | Where-Object { $_.PSChildName -match '^\{' }).Count
}

if (-not (Test-Path -LiteralPath $Dll)) {
  Say "找不到 DLL:$Dll"
  exit 2
}
Say "DLL   = $Dll"
Say "版本  = $OldVersion -> $NewVersion"
Say "模式  = $(if ($Apply) { '实弹(写 HKLM)' } else { '干跑' })"
if (-not $Apply) { Say "`n干跑到此为止 —— 加 -Apply(且管理员)才会真的动注册表。"; exit 0 }

Say ''
Say '== 1) 装旧版(不该炸,版本号要如实写下)'
$out = Invoke-Reg 'register' $OldVersion
Verdict '装旧版成功' ($LASTEXITCODE -eq 0 -or (State-Dll)) "版本=$((State-Version))"
Verdict '旧版版本号如实' ((State-Version) -eq $OldVersion) "读到=$((State-Version))"

Say ''
Say '== 2) 升级(版本号与 DLL 路径都换新)'
$out = Invoke-Reg 'register' $NewVersion
Verdict '升级后版本号是新的' ((State-Version) -eq $NewVersion) "读到=$((State-Version))"
$got = (State-Dll) -replace '/', '\'
$want = $ExpectDll -replace '/', '\'
Verdict 'InProcServer32 指向正式构建' ($got -eq $want) "读到=$got"

Say ''
Say '== 3) 再装一次同名同版(不许产生第二份)'
$before = Profile-Count
$out = Invoke-Reg 'register' $NewVersion
$after = Profile-Count
Verdict '重复注册不新增 profile 条目' ($before -eq $after -and $after -eq 1) "前=$before 后=$after"

Say ''
Say '== 4) verify(装完自检:不该是 STALE)'
$out = Invoke-Reg 'verify' ''
$stale = ($out -match 'STALE')
Verdict 'verify 不是 STALE' (-not $stale) ""
Say ($out.Trim().Split("`n") | Select-Object -Last 3 | Out-String).Trim()

Say ''
Say "== 结论:$(if ($script:fail -eq 0) { 'PASS(4 组判据全过)' } else { "FAIL($($script:fail) 条不过)" })"
if ($Log) { Stop-Transcript | Out-Null }
exit $(if ($script:fail -eq 0) { 0 } else { 1 })
