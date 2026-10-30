# Build the green package: unpack and run, models and Python runtime included.
#
# Outputs (under dist\, already gitignored):
#   dist\sainput-<ver>-win64\          folder form
#   dist\sainput-<ver>-win64.zip       the same folder zipped
#
# Prerequisites (run once):
#   powershell -ExecutionPolicy Bypass -File scripts\fetch-models.ps1     # models
#   powershell -ExecutionPolicy Bypass -File scripts\fetch-runtime.ps1   # python runtime
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\make-portable.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\make-portable.ps1 -Overwrite
#
# The package is gated on `cargo test --release -p sainput-core` (the same command CI
# runs, see CONTRIBUTING.md 3.1). `cargo build` never compiles test targets, so without
# that gate a release can be tagged while its unit tests do not even compile.
#
# Version always derives from Cargo.toml; never hardcode it here to avoid drift.
# Keep this file pure ASCII (see build.bat header for the reason).

param(
    [string]$Version = "",      # empty = derive from Cargo.toml
    [switch]$Overwrite,
    # Defaults are Apache-2.0 and match the repo default model_dir.
    [string[]]$AsrModels = @("sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05"),
    [string]$PunctModel = "sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8"
)

$ErrorActionPreference = "Stop"
$Root   = Split-Path -Parent $PSScriptRoot
Set-Location $Root

# ---------------------------------------------------------------- version ----
$Cargo = Get-Content (Join-Path $Root "Cargo.toml") -Raw
$m = [regex]::Match($Cargo, '(?m)^version\s*=\s*"([^"]+)"')
if (-not $m.Success) { throw "Cannot find workspace version in Cargo.toml" }
$CargoVersion = $m.Groups[1].Value
if ([string]::IsNullOrWhiteSpace($Version)) { $Version = $CargoVersion }
elseif ($CargoVersion -ne $Version) { throw "Version mismatch: Cargo.toml=$CargoVersion, -Version=$Version" }
Write-Host "[pack] version = $Version (from Cargo.toml)"

$CargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if ((Test-Path $CargoBin) -and ($env:PATH -notlike "*$CargoBin*")) { $env:PATH = "$CargoBin;$env:PATH" }

# ---------------------------------------------------------------- tests -------
# Gate the package on the unit tests. Run it HERE, before the layout block below
# deletes or rewrites anything under dist\, so a failing gate leaves the previous
# package (folder and zip) untouched.
Write-Host "[pack] cargo test --release -p sainput-core"
& cargo test --release -p sainput-core
if ($LASTEXITCODE -ne 0) { throw "cargo test --release -p sainput-core failed - refusing to package" }

# ---------------------------------------------------------------- layout -----
$Dist = Join-Path $Root "dist\sainput-$Version-win64"
$Zip  = Join-Path $Root "dist\sainput-$Version-win64.zip"
if (Test-Path $Dist) {
    if (-not $Overwrite) { throw "Dist exists: $Dist (pass -Overwrite)" }
    Remove-Item -Recurse -Force $Dist
}
New-Item -ItemType Directory -Force $Dist | Out-Null

# ---------------------------------------------------------------- exe --------
# Reuse an existing build; build fresh if none. Accepts both MSVC and GNU layouts.
$Exe = $null
foreach ($c in @("target\release\sainput.exe", "target\x86_64-pc-windows-gnu\release\sainput.exe")) {
    $p = Join-Path $Root $c
    if (Test-Path $p) { $Exe = $p; break }
}
if (-not $Exe) {
    Write-Host "[pack] no existing build found, running cargo build --release"
    & cargo build --release --workspace
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    $Exe = Join-Path $Root "target\release\sainput.exe"
    if (-not (Test-Path $Exe)) { $Exe = Join-Path $Root "target\x86_64-pc-windows-gnu\release\sainput.exe" }
}
Write-Host "[pack] binary = $Exe"
Copy-Item $Exe (Join-Path $Dist "sainput.exe")

# ---------------------------------------------------------------- docs -------
foreach ($f in @("LICENSE", "THIRD_PARTY_NOTICES.md", "sainput.conf.example")) {
    $src = Join-Path $Root $f
    if (Test-Path $src) { Copy-Item $src $Dist }
    else { Write-Warning "missing $f" }
}

# ---------------------------------------------------------------- tools ------
# Only the modules the sidecar imports plus the caret-context probe.
$Tools = Join-Path $Dist "tools"
New-Item -ItemType Directory -Force $Tools | Out-Null
foreach ($f in @("asr_sidecar.py", "itn.py", "polish.py", "repair.py", "textfix.py", "ctx_probe.py")) {
    $src = Join-Path $Root "tools\$f"
    if (Test-Path $src) { Copy-Item $src $Tools }
    else { Write-Warning "missing tools\$f" }
}

# ---------------------------------------------------------------- models -----
# Copy only the files a model needs: *.onnx / tokens.* / config / bpe (hotwords need bpe).
# Test files (test_onnx.py, test.py, test_wavs) are never shipped.
$Models = Join-Path $Dist "models"
New-Item -ItemType Directory -Force $Models | Out-Null

function Copy-Model([string]$Name) {
    $src = Join-Path $Root "models\$Name"
    $dst = Join-Path $Dist "models\$Name"
    if (-not (Test-Path $src)) { throw "Missing model dir: $src (run scripts\fetch-models.ps1)" }
    New-Item -ItemType Directory -Force $dst | Out-Null
    Get-ChildItem $src -File | ForEach-Object {
        $keep = ($_.Extension -eq ".onnx") -or
                ($_.Name -like "tokens.*") -or
                ($_.Name -in @("config.yaml", "bpe.model"))
        if ($keep) { Copy-Item $_.FullName $dst -Force }
    }
    Get-ChildItem $dst -File | ForEach-Object {
        Write-Host ("[pack]   {0,-14} {1,8:N1} MB" -f $Name, "")
    } | Out-Null
    $sz = (Get-ChildItem $dst -Recurse -File | Measure-Object -Property Length -Sum).Sum
    Write-Host ("[pack] model {0} = {1:N1} MB" -f $Name, ($sz / 1MB))
}

foreach ($a in $AsrModels)  { Copy-Model $a }
if ($PunctModel)           { Copy-Model $PunctModel }

# ---------------------------------------------------------------- runtime ----
# Embeddable Python plus an unpacked site-packages. Without it the green package is unusable.
$PyHome = Get-ChildItem (Join-Path $Root ".runtime-src") -Directory -Filter "python-*-embed-amd64" |
          Select-Object -First 1
if (-not $PyHome) { throw "embedded python not found. Run: scripts\fetch-runtime.ps1" }
$Wh = Join-Path $Root ".runtime-src\wheels"
if (-not (Test-Path $Wh) -or -not (Get-ChildItem $Wh -Filter "*.whl")) {
    throw "wheels not found. Run: scripts\fetch-runtime.ps1"
}

$Rt = Join-Path $Dist "runtime"
Copy-Item $PyHome.FullName $Rt -Recurse -Force

# The embeddable python312._pth disables site by default; enable it so site-packages imports work.
$PyDll = Get-ChildItem $Rt -Filter "python3*.dll" |
         Where-Object { $_.Name -match "^python3\d+\.dll$" } |
         Select-Object -First 1
if (-not $PyDll) { throw "python3xx.dll not found in runtime - packaging is broken" }
$LibTag = [regex]::Match($PyDll.Name, "^(python3\d+)").Groups[1].Value
$Site   = Join-Path $Rt "Lib\site-packages"
New-Item -ItemType Directory -Force $Site | Out-Null
$Pth = Join-Path $Rt "$LibTag._pth"
Set-Content -Path $Pth -Value "$LibTag.zip", ".", "Lib\site-packages", "import site" -Encoding ASCII
Write-Host "[pack] wrote $LibTag._pth -> $Site"

# A wheel is a zip: unpack it into site-packages; no pip install needed.
Add-Type -AssemblyName System.IO.Compression.FileSystem
foreach ($w in (Get-ChildItem $Wh -Filter "*.whl" | Sort-Object Name)) {
    $tmp = Join-Path $Root ".runtime-src\_unzip"
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
    [System.IO.Compression.ZipFile]::ExtractToDirectory($w.FullName, $tmp)
    # Flatten wheel contents; .data dirs only carry packaging metadata we skip.
    Get-ChildItem $tmp | ForEach-Object {
        if ($_.PSIsContainer) {
            if ($_.Name -like "*.data") { continue }
            Copy-Item $_.FullName $Site -Recurse -Force
        } else { Copy-Item $_.FullName $Site -Force }
    }
    Write-Host ("[pack] wheel -> site-packages: {0}" -f $w.Name)
}
Remove-Item -Recurse -Force (Join-Path $Root ".runtime-src\_unzip") -ErrorAction SilentlyContinue

# Nothing that only exists after a run belongs in the package.
Get-ChildItem $Dist -Recurse -Directory -Filter "__pycache__" -ErrorAction SilentlyContinue |
    Remove-Item -Recurse -Force

# ---------------------------------------------------------------- launchers --
# The green package differs from the source tree: the exe sits at the package root,
# so these launchers set SAINPUT_PYTHON to the bundled interpreter and never touch the system Python.
$L1 = Join-Path $Dist "start.bat"
$L2 = Join-Path $Dist "run.bat"

Set-Content $L1 @"
@echo off
setlocal EnableExtensions
REM  sainput - local voice input method. Double-click to start.
REM  console-hiding trick: see scripts\start.bat in the source repo for the long
REM  version. Do not "simplify" this - start /b + sainput FreeConsole is what
REM  removes the window.
cd /d "%~dp0"
set "PYTHONIOENCODING=utf-8"
if defined SAINPUT_PYTHON set "PY=%SAINPUT_PYTHON%" else set "PY=%~dp0runtime\python.exe"
if not exist "%PY%" (
    echo.
    echo   [sainput] bundled python not found: %PY%
    echo   [sainput] re-download the package, or set SAINPUT_PYTHON to your own python 3.11+.
    echo.
    pause
    exit /b 1
)
set "SAINPUT_PYTHON=%PY%"
echo.
echo   sainput - starting. Tray icon = running.
echo   Quit: right-click the tray icon, then Exit.
echo.
start "" /b "%~dp0sainput.exe" run --hide-console
endlocal
exit /b 0
"@ -Encoding ASCII

Set-Content $L2 @"
@echo off
setlocal EnableExtensions
REM  run.bat - foreground runner with arguments passed through.
cd /d "%~dp0"
set "PYTHONIOENCODING=utf-8"
if defined SAINPUT_PYTHON set "PY=%SAINPUT_PYTHON%" else set "PY=%~dp0runtime\python.exe"
if not exist "%PY%" (
    echo   [sainput] bundled python not found: %PY%
    echo   [sainput] set SAINPUT_PYTHON to your own python 3.11+ and retry.
    exit /b 1
)
set "SAINPUT_PYTHON=%PY%"
"%~dp0sainput.exe" %*
exit /b %ERRORLEVEL%
"@ -Encoding ASCII

# ---------------------------------------------------------------- README -----
Set-Content (Join-Path $Dist "README.txt") @"
sainput $Version - local Windows voice input (no network at runtime)

  1. Unzip anywhere. No installer needed.
  2. Double-click start.bat.
  3. Put the caret somewhere that accepts text (Notepad, browser, WeChat...).
  4. Hold Right Ctrl, speak, release - text streams in as you speak.
  5. Quit: right-click the tray icon -> Exit.

The ASR model (~160 MB) and a private Python runtime are bundled, so nothing
is downloaded on first run and nothing leaves the machine.

Models: x-asr-160ms and punc_ct-transformer (both Apache-2.0). Model weights
are NOT covered by the application's MIT license - see
THIRD_PARTY_NOTICES.md.

If you prefer your own Python (3.11+) with sherpa-onnx installed, set
SAINPUT_PYTHON=C:\path\to\python.exe before starting; the bundled
runtime\ folder is then ignored.

Docs / issues: https://github.com/nakamotosai/sainput
"@ -Encoding ASCII

# ---------------------------------------------------------------- zip --------
$RelTotal = 0
Get-ChildItem $Dist -Recurse -File | ForEach-Object { $RelTotal += $_.Length }
Write-Host ("[pack] payload = {0:N1} MB" -f ($RelTotal / 1MB))

New-Item -ItemType Directory -Force (Split-Path -Parent $Zip) | Out-Null
if (Test-Path $Zip) { Remove-Item -Force $Zip }
Compress-Archive -Path $Dist -DestinationPath $Zip -Force
$ZipSize = (Get-Item $Zip).Length
Write-Host ("[pack] zip = {0} ({1:N1} MB)" -f $Zip, ($ZipSize / 1MB))
Write-Host ("[pack] sha256 = {0}" -f (Get-FileHash $Zip -Algorithm SHA256).Hash.ToLower())
Write-Host ""
Write-Host "[pack] done."
