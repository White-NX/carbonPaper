[CmdletBinding()]
param([string]$RootDir)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($RootDir)) {
    $RootDir = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
} else {
    $RootDir = (Resolve-Path $RootDir).Path
}

Add-Type -AssemblyName System.IO.Compression.FileSystem

$tauriConfig = Get-Content -LiteralPath (Join-Path $RootDir "src-tauri\tauri.conf.json") -Raw | ConvertFrom-Json
$version = [string]$tauriConfig.version
$bundleDir = Join-Path $RootDir "src-tauri\target\release\bundle\nsis"
$portableZip = Join-Path $bundleDir "carbonpaper_${version}_x64_portable.zip"
$installer = Join-Path $bundleDir "carbonpaper_${version}_x64-setup.exe"
$manifest = Get-Content -LiteralPath (Join-Path $RootDir "scripts\release-assets\ocr-models.json") -Raw | ConvertFrom-Json

function Get-StreamSha256 {
    param([Parameter(Mandatory = $true)][System.IO.Stream]$Stream)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return -join ($sha.ComputeHash($Stream) | ForEach-Object { $_.ToString("x2") })
    } finally {
        $sha.Dispose()
    }
}

function Get-PathSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        return Get-StreamSha256 -Stream $stream
    } finally {
        $stream.Dispose()
    }
}

if (-not (Test-Path -LiteralPath $portableZip -PathType Leaf)) {
    throw "Portable bundle is missing: $portableZip"
}
$archive = [System.IO.Compression.ZipFile]::OpenRead($portableZip)
try {
    $duplicates = $archive.Entries | Group-Object FullName | Where-Object Count -gt 1
    if ($duplicates) {
        throw "Portable bundle contains duplicate entries: $($duplicates.Name -join ', ')"
    }
    foreach ($required in @("carbonpaper.exe", "carbonpaper-ml.exe", "carbonpaper-nmh.exe", "THIRD_PARTY_NOTICES.md")) {
        if (-not $archive.GetEntry($required)) {
            throw "Portable bundle is missing $required"
        }
    }
    foreach ($asset in $manifest.files) {
        $entryName = (([string]$manifest.bundle_path).TrimEnd('/') + '/' + [string]$asset.name)
        $entry = $archive.GetEntry($entryName)
        if (-not $entry) {
            throw "Portable bundle is missing $entryName"
        }
        if ($entry.Length -ne [long]$asset.size) {
            throw "Portable bundle asset size mismatch for $entryName"
        }
        $stream = $entry.Open()
        try {
            $hash = Get-StreamSha256 -Stream $stream
        } finally {
            $stream.Dispose()
        }
        if ($hash -ne ([string]$asset.sha256).ToLowerInvariant()) {
            throw "Portable bundle asset checksum mismatch for $entryName"
        }
    }
} finally {
    $archive.Dispose()
}

if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) {
    throw "NSIS installer is missing: $installer"
}
$sevenZip = Get-Command 7z -ErrorAction SilentlyContinue
if (-not $sevenZip) {
    $sevenZip = Get-Command 7z.exe -ErrorAction SilentlyContinue
}
if (-not $sevenZip) {
    throw "7-Zip is required to verify NSIS bundle contents"
}

$listing = @(& $sevenZip.Source l $installer)
foreach ($required in @("carbonpaper-ml.exe", "carbonpaper-nmh.exe", "THIRD_PARTY_NOTICES.md")) {
    $count = @($listing | Where-Object { $_ -match ("\s" + [regex]::Escape($required) + "$") }).Count
    if ($count -ne 1) {
        throw "NSIS installer must contain exactly one $required entry; found $count"
    }
}
foreach ($asset in $manifest.files) {
    if (-not ($listing -match [regex]::Escape([string]$asset.name))) {
        throw "NSIS installer is missing $($asset.name)"
    }
}

$tempRoot = [System.IO.Path]::GetTempPath()
$extractDir = Join-Path $tempRoot ("carbonpaper-nsis-verify-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $extractDir | Out-Null
try {
    & $sevenZip.Source x $installer "-o$extractDir" -y | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to extract NSIS installer for verification"
    }
    $modelDir = Join-Path $extractDir ([string]$manifest.bundle_path).Replace('/', '\')
    foreach ($asset in $manifest.files) {
        $path = Join-Path $modelDir ([string]$asset.name)
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Extracted NSIS installer is missing $path"
        }
        $hash = Get-PathSha256 -Path $path
        if ($hash -ne ([string]$asset.sha256).ToLowerInvariant()) {
            throw "Extracted NSIS model checksum mismatch for $path"
        }
    }
    $worker = Join-Path $extractDir "carbonpaper-ml.exe"
    $previousErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $output = & $worker --verify-models --model-dir $modelDir 2>&1
        $workerExitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
    if ($workerExitCode -ne 0 -or ($output -join "`n") -notmatch '"model_id":"ppocrv5-ch-mobile"') {
        throw "Extracted NSIS Rust OCR runtime verification failed: $($output -join "`n")"
    }
} finally {
    $resolvedExtractDir = [System.IO.Path]::GetFullPath($extractDir)
    if ((Test-Path -LiteralPath $resolvedExtractDir) -and $resolvedExtractDir.StartsWith($tempRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $resolvedExtractDir -Recurse -Force
    }
}

Write-Host "Portable and NSIS Rust OCR runtime bundles verified."
