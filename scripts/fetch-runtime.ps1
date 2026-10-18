# Prepare the private Python runtime for sainput.
#
# Why this exists: sainput.exe handles audio / hotkey / text injection only.
# Recognition runs in the Python sidecar (tools/asr_sidecar.py) and needs
# sherpa-onnx + numpy. To ship a green package that runs without any setup,
# the packaging script bundles an embeddable Python plus those two packages.
#
# Outputs (never committed; consumed by make-portable.ps1):
#   .runtime-src\python-<ver>-embed-amd64\     unpacked embeddable Python
#   .runtime-src\wheels\*.whl                 the two packages
#
# Usage (PowerShell, from the repo root):
#   powershell -ExecutionPolicy Bypass -File scripts\fetch-runtime.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\fetch-runtime.ps1 -Version 3.12.10
#
# Keep this file pure ASCII (see build.bat header for the reason).

param(
    [string]$Version = "3.12.10"
)

$ErrorActionPreference = "Stop"
$Root   = Split-Path -Parent $PSScriptRoot
$Out    = Join-Path $Root ".runtime-src"
$Wh     = Join-Path $Out "wheels"
$PyName = "python-$Version-embed-amd64"
$PyHome = Join-Path $Out $PyName

New-Item -ItemType Directory -Force $Wh | Out-Null

if (Test-Path (Join-Path $PyHome "python.exe")) {
    Write-Host "[runtime] embedded python already present: $PyHome"
} else {
    $ZipName = "python-$Version-embed-amd64.zip"
    $Zip     = Join-Path $Out $ZipName
    $Url     = "https://www.python.org/ftp/python/$Version/$ZipName"
    Write-Host "[runtime] downloading $Url"
    $ProgressPreference = "SilentlyContinue"
    Invoke-WebRequest -Uri $Url -OutFile $Zip -UseBasicParsing
    Write-Host ("[runtime] downloaded {0:N1} MB" -f ((Get-Item $Zip).Length / 1MB))

    # Extract in .NET (Expand-Archive is slow on 11k files) and unblock the
    # payload afterwards so Windows does not treat it as an Internet download.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::ExtractToDirectory($Zip, $PyHome)
    Get-ChildItem $PyHome -Recurse -File | Unblock-File
    Write-Host "[runtime] extracted to $PyHome"
}

# The embeddable build has no pip, so use the caller's own Python to fetch the
# wheels; make-portable.ps1 then unpacks them into site-packages.
Write-Host "[runtime] resolving wheels (sherpa-onnx==1.13.4, numpy)"
$py = Get-Command python -ErrorAction SilentlyContinue
if (-not $py) { throw "python not found on PATH - install Python 3.11+ first" }

& $py.Source -m pip download "sherpa-onnx==1.13.4" "numpy" `
    --only-binary=:all: --platform win_amd64 --python-version 312 -d $Wh
if ($LASTEXITCODE -ne 0) { throw "pip download failed" }

foreach ($f in (Get-ChildItem $Wh -Filter "*.whl" | Sort-Object Name)) {
    Write-Host ("[runtime] wheel {0}  {1:N1} MB" -f $f.Name, ($f.Length / 1MB))
}

Write-Host ""
Write-Host "[runtime] done. Next step:"
Write-Host "    powershell -ExecutionPolicy Bypass -File scripts\make-portable.ps1"
