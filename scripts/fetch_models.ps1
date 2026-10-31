#Requires -Version 5.1
<#
.SYNOPSIS
    Download and verify the sherpa-onnx models sainput needs.

.DESCRIPTION
    Downloads model archives from the official sherpa-onnx GitHub releases into
    <repo>\models\<asset-name>\ , checks file size and required files, then extracts.

    Verified assets (URLs and byte sizes checked against the upstream release):
      ASR   (asr-models)          sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05.tar.bz2   134370008 B
      PUNCT (punctuation-models)  sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8.tar.bz2             64717756 B
        https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05.tar.bz2
        https://github.com/k2-fsa/sherpa-onnx/releases/download/punctuation-models/sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8.tar.bz2

    Other variants use the same release/tag layout; their exact byte size is not
    hard-coded here (0 = skip the size check), but the required-files check still runs.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\fetch_models.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\fetch_models.ps1 -Variant 480ms

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\fetch_models.ps1 -Variant sensevoice -NoPunct
#>
[CmdletBinding()]
param(
    # 160ms is the default model compiled into sainput (see crates/sainput-cli/src/main.rs).
    [ValidateSet('160ms', '480ms', '960ms', 'nemotron', 'bilingual', 'zh14m', 'sensevoice', 'all')]
    [string]$Variant = '160ms',

    # Re-download / re-extract even when the target folder already looks complete.
    [switch]$Force,

    # Skip the punctuation model.
    [switch]$NoPunct,

    # Keep the downloaded .tar.bz2 next to the extracted folder (default: delete after success).
    [switch]$KeepArchive,

    # Only print what would be downloaded. No network access.
    [switch]$DryRun,

    # Override the models directory (default: <repo>\models).
    [string]$ModelsDir = ''
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# Windows PowerShell 5.1 defaults to TLS 1.0 for some hosts; GitHub needs 1.2+.
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
    # Older .NET without Tls12 enum member: ignore, Invoke-WebRequest will report the real error.
}

$RepoRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($ModelsDir)) {
    $ModelsDir = Join-Path $RepoRoot 'models'
}

$AsrBase   = 'https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models'
$PunctBase = 'https://github.com/k2-fsa/sherpa-onnx/releases/download/punctuation-models'

# asset name -> expected size in bytes (0 = unknown, skip size check)
$AsrVariants = [ordered]@{
    '160ms'     = @{ Asset = 'sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05';             Size = 134370008 }
    '480ms'     = @{ Asset = 'sherpa-onnx-x-asr-480ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05';             Size = 0 }
    '960ms'     = @{ Asset = 'sherpa-onnx-x-asr-960ms-streaming-zipformer-transducer-zh-en-int8-2026-06-05';             Size = 0 }
    'nemotron'  = @{ Asset = 'sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-80ms-int8-2026-06-11';                          Size = 0 }
    'bilingual' = @{ Asset = 'sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20';                                Size = 0 }
    'zh14m'     = @{ Asset = 'sherpa-onnx-streaming-zipformer-zh-14M-2023-02-23';                                        Size = 0 }
    # 非流式(离线):给"松手后整段重解"当候选。中英日韩粤五语一个模型,文件是
    # 单个 model.int8.onnx —— 所以它要用的必检文件和流式那几档不同(见 Patterns)。
    'sensevoice' = @{ Asset = 'sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17';                                        Size = 0; Patterns = @('tokens.txt', 'model*.onnx') }
}

$PunctAsset = 'sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8'
$PunctSize  = 64717756

function Get-ToolPath {
    param([string]$Name)
    $cmd = Get-Command $Name -ErrorAction SilentlyContinue
    if ($null -ne $cmd) { return $cmd.Source }
    return $null
}

function Get-MissingFiles {
    param([string]$Dir, [string[]]$Patterns)
    $missing = @()
    foreach ($p in $Patterns) {
        # -Path (not -LiteralPath): patterns like encoder*.onnx must expand.
        if (-not (Test-Path -Path (Join-Path $Dir $p))) { $missing += $p }
    }
    return $missing
}

$AsrPatterns   = @('tokens.txt', 'encoder*.onnx', 'decoder*.onnx', 'joiner*.onnx')
$PunctPatterns = @('model*.onnx')

function Show-Plan {
    param([string]$Url, [string]$Dest, [long]$Size)
    $sizeText = if ($Size -gt 0) { '{0:N0} B' -f $Size } else { 'size not pinned' }
    Write-Host ("  URL    : {0}" -f $Url)
    Write-Host ("  target : {0}" -f $Dest)
    Write-Host ("  expect : {0}" -f $sizeText)
}

function Install-Model {
    param(
        [Parameter(Mandatory = $true)][string]$Asset,
        [Parameter(Mandatory = $true)][string]$Url,
        [long]$ExpectedSize = 0,
        [Parameter(Mandatory = $true)][string[]]$RequiredFiles,
        [Parameter(Mandatory = $true)][string]$Kind
    )

    $destDir  = Join-Path $ModelsDir $Asset
    $tarPath  = Join-Path $ModelsDir ($Asset + '.tar.bz2')
    $downloaded = $false

    Write-Host ''
    Write-Host ("[{0}] {1}" -f $Kind, $Asset)

    # ---------- already installed? ----------
    if ((Test-Path -LiteralPath $destDir) -and -not $Force) {
        $missing = Get-MissingFiles -Dir $destDir -Patterns $RequiredFiles
        if ($missing.Count -eq 0) {
            Write-Host ("  SKIP   already present and complete: {0}" -f $destDir) -ForegroundColor Green
            return $true
        }
        Write-Host ("  WARN   folder exists but is incomplete (missing: {0})" -f ($missing -join ', ')) -ForegroundColor Yellow
        if (Test-Path -LiteralPath $tarPath) {
            Write-Host ("  INFO   archive found, re-extracting: {0}" -f $tarPath)
        } else {
            Write-Host '  ERROR  no archive to re-extract from. Re-run with -Force to download again.' -ForegroundColor Red
            Write-Host ("         (or delete the folder: {0})" -f $destDir)
            return $false
        }
    }

    # ---------- download ----------
    if (-not (Test-Path -LiteralPath $tarPath)) {
        if ($DryRun) {
            Write-Host '  [dry-run] would download:'
            Show-Plan -Url $Url -Dest $destDir -Size $ExpectedSize
            return $true
        }

        Write-Host '  GET    downloading...'
        Show-Plan -Url $Url -Dest $destDir -Size $ExpectedSize
        try {
            Invoke-WebRequest -Uri $Url -OutFile $tarPath -UseBasicParsing
        } catch {
            Write-Host ("  ERROR  download failed: {0}" -f $_.Exception.Message) -ForegroundColor Red
            Write-Host '         Check network / proxy, then re-run this script.'
            if (Test-Path -LiteralPath $tarPath) { Remove-Item -LiteralPath $tarPath -Force }
            return $false
        }
        $downloaded = $true
    } else {
        Write-Host ("  INFO   archive already downloaded: {0}" -f $tarPath)
    }

    # ---------- size check ----------
    $actual = (Get-Item -LiteralPath $tarPath).Length
    if ($ExpectedSize -gt 0 -and $actual -ne $ExpectedSize) {
        Write-Host ("  ERROR  size mismatch: got {0:N0} B, expected {1:N0} B" -f $actual, $ExpectedSize) -ForegroundColor Red
        Write-Host '         The download is incomplete or the upstream asset changed.'
        Write-Host '         Delete the archive and re-run this script.'
        if ($downloaded) {
            Remove-Item -LiteralPath $tarPath -Force
            Write-Host '         (removed the incomplete archive)'
        }
        return $false
    }
    if ($ExpectedSize -eq 0) {
        Write-Host ("  OK     archive size {0:N0} B (not pinned upstream)" -f $actual)
    } else {
        Write-Host ("  OK     archive size {0:N0} B matches" -f $actual)
    }

    # ---------- extract ----------
    $tarExe = Get-ToolPath -Name 'tar'
    if ($null -eq $tarExe) {
        Write-Host '  ERROR  tar.exe not found. It ships with Windows 10 1803+.' -ForegroundColor Red
        Write-Host '         Alternatives: extract manually with 7-Zip:'
        Write-Host ("           7z x `"{0}`" -o`"{1}`"" -f $tarPath, $ModelsDir)
        return $false
    }

    if ($DryRun) {
        Write-Host ("  [dry-run] would extract into: {0}" -f $ModelsDir)
        return $true
    }

    Write-Host ("  XZ     extracting into {0}" -f $ModelsDir)
    & $tarExe -xjf $tarPath -C $ModelsDir
    if ($LASTEXITCODE -ne 0) {
        Write-Host ("  ERROR  extraction failed (tar exit code {0})" -f $LASTEXITCODE) -ForegroundColor Red
        Write-Host '         The archive may be corrupt. Re-run with -Force.'
        return $false
    }

    # ---------- verify ----------
    if (-not (Test-Path -LiteralPath $destDir)) {
        Write-Host ("  ERROR  archive did not create the expected folder: {0}" -f $destDir) -ForegroundColor Red
        Write-Host '         Upstream may have renamed the archive root. Check models\ and use --model-dir.'
        return $false
    }
    $missing = Get-MissingFiles -Dir $destDir -Patterns $RequiredFiles
    if ($missing.Count -gt 0) {
        Write-Host ("  ERROR  extracted folder is missing: {0}" -f ($missing -join ', ')) -ForegroundColor Red
        return $false
    }

    Write-Host ("  DONE   {0}" -f $destDir) -ForegroundColor Green

    # ---------- cleanup ----------
    if ($downloaded -and -not $KeepArchive -and -not $Force) {
        Remove-Item -LiteralPath $tarPath -Force
        Write-Host '  INFO   removed the archive (use -KeepArchive to keep it)'
    }
    return $true
}

# ------------------------------------------------------------------ main
if ($Variant -eq 'all') {
    $names = @($AsrVariants.Keys)
} else {
    $names = @($Variant)
}

Write-Host 'sainput model fetcher'
Write-Host ("  repo   : {0}" -f $RepoRoot)
Write-Host ("  models : {0}" -f $ModelsDir)
Write-Host ("  python : {0}" -f (Get-ToolPath -Name 'python'))
if ($DryRun) { Write-Host '  mode   : dry-run (nothing is downloaded)' }

if (-not (Test-Path -LiteralPath $ModelsDir)) {
    if ($DryRun) {
        Write-Host ("  [dry-run] would create {0}" -f $ModelsDir)
    } else {
        New-Item -ItemType Directory -Path $ModelsDir -Force | Out-Null
    }
}

$ok = $true

foreach ($name in $names) {
    $v = $AsrVariants[$name]
    if ($null -eq $v) {
        Write-Host ("  ERROR  unknown variant '{0}'" -f $name) -ForegroundColor Red
        $ok = $false
        continue
    }
    $url = $AsrBase + '/' + $v.Asset + '.tar.bz2'
    # 必检文件按档给:流式那几档是 encoder/decoder/joiner int8 三件套,
    # 非流式的 sensevoice 只有一个 model.int8.onnx。
    $need = if ($v.ContainsKey('Patterns')) { $v['Patterns'] } else { $AsrPatterns }
    $r = Install-Model -Asset $v.Asset -Url $url -ExpectedSize $v.Size -RequiredFiles $need -Kind "ASR $name"
    if (-not $r) { $ok = $false }
}

if (-not $NoPunct) {
    $url = $PunctBase + '/' + $PunctAsset + '.tar.bz2'
    $r = Install-Model -Asset $PunctAsset -Url $url -ExpectedSize $PunctSize -RequiredFiles $PunctPatterns -Kind 'PUNCT'
    if (-not $r) { $ok = $false }
}

Write-Host ''
if (-not $ok) {
    Write-Host 'FAILED - at least one model is missing. See the messages above.' -ForegroundColor Red
    exit 1
}

if ($DryRun) {
    Write-Host 'dry-run complete.' -ForegroundColor Green
} else {
    Write-Host 'All requested models are in place.' -ForegroundColor Green
    Write-Host 'Next: sainput expects them under <repo>\models\ (config keys: model_dir / punct_dir).'
}
exit 0
