# tsf_register.ps1 -- register / unregister the sainput TSF text service.
#
# ASCII-ONLY on purpose: Windows PowerShell 5 decodes .ps1 as ANSI (GBK here) and
# silently mangles non-ASCII text, which then eats newlines and breaks parsing.
#
# Why a script and not an installer: TIP registration is a handful of registry
# writes plus a COM in-proc server entry. Doing it in the open means it can be
# audited and, more importantly, cleanly undone (`-Action unregister`).
#
# Registry shape was taken from a working keyboard TIP on this machine
# (Metasequoia IME) -- measured, not guessed:
#   HKLM\SOFTWARE\Microsoft\CTF\TIP\{CLSID}
#     Category\Category\{catGuid}\{CLSID}      (empty subkey per category)
#     Category\Item\{CLSID}\{catGuid}          (empty subkey per category)
#     LanguageProfile\0x00000804\{profileGuid}
#        Description (SZ), IconIndex (DWORD), Enable (DWORD=1)
#   HKLM\SOFTWARE\Classes\CLSID\{CLSID}
#     (default)=display name
#     InProcServer32: (default)=dll path, ThreadingModel=Apartment
#
# We register ONLY GUID_TFCAT_TIP_KEYBOARD. The other seven categories that IME
# claims are capability claims (secure mode, COM-less, immersive, tray support,
# input-mode compartment) and we do not implement them -- claiming a capability
# we lack is worse than not having it.
param(
  [Parameter(Mandatory = $true)][ValidateSet('register','unregister','verify')][string]$Action,
  [string]$Dll = '',
  [string]$Name = 'sainput voice input',
  [string]$Clsid = '{FCC45A5E-09C4-410A-B696-BDD2DA66A21F}',
  [string]$ProfileGuid = '{6A1F2B7C-3D4E-4F50-9A1B-2C3D4E5F6071}',
  [string]$LangId = '0x00000804',
  [switch]$Apply
)

$ErrorActionPreference = 'Stop'
$TIP = "HKLM:\SOFTWARE\Microsoft\CTF\TIP\$Clsid"
$COMS = "HKLM:\SOFTWARE\Classes\CLSID\$Clsid"
$KEYBOARD_CAT = '{34745C63-B2F0-4784-8B67-5E12C8701A31}'   # GUID_TFCAT_TIP_KEYBOARD

function Test-Admin {
  $id = [Security.Principal.WindowsIdentity]::GetCurrent()
  (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Show-Plan {
  Write-Output "plan (CLSID $Clsid):"
  Write-Output "  1) $TIP\Category\Category\$KEYBOARD_CAT\$Clsid"
  Write-Output "  2) $TIP\Category\Item\$Clsid\$KEYBOARD_CAT"
  Write-Output "  3) $TIP\LanguageProfile\$LangId\$ProfileGuid  (Description='$Name', Enable=1)"
  Write-Output "  4) $COMS  (default)='$Name'"
  Write-Output "  5) $COMS\InProcServer32  (default)='$Dll'  ThreadingModel='Apartment'"
}

function Show-State {
  $found = @()
  foreach ($p in @($TIP, $COMS, "$COMS\InProcServer32")) {
    if (Test-Path $p) { $found += $p }
  }
  if ($found.Count -eq 0) {
    Write-Output "state: NOT registered"
    return
  }
  Write-Output "state: registered ($($found.Count) key(s))"
  foreach ($p in $found) { Write-Output "  $p" }
  $dllPath = (Get-ItemProperty -Path "$COMS\InProcServer32" -ErrorAction SilentlyContinue).'(default)'
  if ($dllPath) { Write-Output "  dll: $dllPath" }
  $prof = "$TIP\LanguageProfile\$LangId\$ProfileGuid"
  if (Test-Path $prof) {
    $d = (Get-ItemProperty -Path $prof).Description
    Write-Output "  profile: $ProfileGuid  description='$d'"
  } else {
    Write-Output "  profile: MISSING ($prof)"
  }
}

if ($Action -eq 'verify') { Show-State; exit 0 }

if ($Action -eq 'register' -and -not $Dll) {
  Write-Output "ERROR: -Dll <path to sainput_tsf.dll> is required for register"
  exit 2
}
if ($Action -eq 'register' -and -not (Test-Path -LiteralPath $Dll)) {
  Write-Output "ERROR: dll not found: $Dll"
  exit 2
}

Show-Plan
if (-not $Apply) {
  Write-Output ""
  Write-Output "dry run -- nothing was changed. Re-run with -Apply (needs an elevated shell)."
  Show-State
  exit 0
}
if (-not (Test-Admin)) {
  Write-Output "ERROR: -Apply needs an elevated shell (HKLM). Nothing was changed."
  exit 3
}

if ($Action -eq 'register') {
  $dllFull = (Resolve-Path -LiteralPath $Dll).Path
  New-Item -Path "$TIP\Category\Category\$KEYBOARD_CAT" -Force | Out-Null
  New-Item -Path "$TIP\Category\Category\$KEYBOARD_CAT\$Clsid" -Force | Out-Null
  New-Item -Path "$TIP\Category\Item\$Clsid" -Force | Out-Null
  New-Item -Path "$TIP\Category\Item\$Clsid\$KEYBOARD_CAT" -Force | Out-Null
  $prof = "$TIP\LanguageProfile\$LangId\$ProfileGuid"
  New-Item -Path $prof -Force | Out-Null
  New-ItemProperty -Path $prof -Name 'Description' -Value $Name -PropertyType String -Force | Out-Null
  New-ItemProperty -Path $prof -Name 'IconIndex' -Value 0 -PropertyType DWord -Force | Out-Null
  New-ItemProperty -Path $prof -Name 'Enable' -Value 1 -PropertyType DWord -Force | Out-Null
  New-Item -Path $COMS -Force | Out-Null
  Set-ItemProperty -Path $COMS -Name '(default)' -Value $Name
  New-Item -Path "$COMS\InProcServer32" -Force | Out-Null
  Set-ItemProperty -Path "$COMS\InProcServer32" -Name '(default)' -Value $dllFull
  New-ItemProperty -Path "$COMS\InProcServer32" -Name 'ThreadingModel' -Value 'Apartment' -PropertyType String -Force | Out-Null
  Write-Output "registered."
  Show-State
  Write-Output ""
  Write-Output "next: the TIP only loads where it is ACTIVE. Use tools/test_tsf_activate.py"
  Write-Output "      (or the language bar) to activate it per process, then check"
  Write-Output "      \\\\.\\pipe\\sainput-tsf-<pid> appears."
  exit 0
}

# unregister: remove exactly what we wrote, then prove nothing is left behind.
if (Test-Path "$COMS\InProcServer32") { Remove-Item -Path "$COMS\InProcServer32" -Recurse -Force }
if (Test-Path $COMS) { Remove-Item -Path $COMS -Recurse -Force }
if (Test-Path $TIP) { Remove-Item -Path $TIP -Recurse -Force }
Write-Output "unregistered."
Show-State
exit 0
