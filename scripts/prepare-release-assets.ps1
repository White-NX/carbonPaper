[CmdletBinding()]
param(
    [switch]$Force,
    [switch]$VerifyOnly,
    [string]$RootDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

if ([string]::IsNullOrWhiteSpace($RootDir)) {
    $RootDir = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
} else {
    $RootDir = (Resolve-Path $RootDir).Path
}

$pythonAsset = @{
    Name = "Python 3.12.10 installer"
    Url = "https://www.python.org/ftp/python/3.12.10/python-3.12.10-amd64.exe"
    Output = "python-3.12.10-amd64.exe"
    Sha256 = "67b5635e80ea51072b87941312d00ec8927c4db9ba18938f7ad2d27b328b95fb"
}

$aria2Asset = @{
    Name = "aria2 1.37.0 Windows x64"
    Url = "https://github.com/aria2/aria2/releases/download/release-1.37.0/aria2-1.37.0-win-64bit-build1.zip"
    ZipName = "aria2-1.37.0-win-64bit-build1.zip"
    ZipSha256 = "67d015301eef0b612191212d564c5bb0a14b5b9c4796b76454276a4d28d9b288"
    Output = "aria2c.exe"
    Sha256 = "be2099c214f63a3cb4954b09a0becd6e2e34660b886d4c898d260febfe9d70c2"
}

function Get-Sha256 {
    param([Parameter(Mandatory = $true)][string]$Path)

    $stream = [System.IO.File]::OpenRead($Path)
    try {
        $sha256 = [System.Security.Cryptography.SHA256]::Create()
        try {
            $hash = $sha256.ComputeHash($stream)
            return -join ($hash | ForEach-Object { $_.ToString("x2") })
        } finally {
            $sha256.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
}

function Assert-Sha256 {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Expected,
        [Parameter(Mandatory = $true)][string]$Label
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label is missing: $Path"
    }

    $actual = Get-Sha256 -Path $Path
    if ($actual -ne $Expected.ToLowerInvariant()) {
        throw "$Label checksum mismatch. Expected $Expected, got $actual at $Path"
    }

    Write-Host "$Label verified: $actual"
}

function Invoke-Download {
    param(
        [Parameter(Mandatory = $true)][string]$Url,
        [Parameter(Mandatory = $true)][string]$OutFile
    )

    Write-Host "Downloading $Url"
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

    $params = @{
        Uri = $Url
        OutFile = $OutFile
    }
    if ((Get-Command Invoke-WebRequest).Parameters.ContainsKey("UseBasicParsing")) {
        $params["UseBasicParsing"] = $true
    }

    Invoke-WebRequest @params
}

function Ensure-DownloadedFile {
    param(
        [Parameter(Mandatory = $true)][hashtable]$Asset
    )

    $outPath = Join-Path $RootDir $Asset.Output
    if ((Test-Path -LiteralPath $outPath -PathType Leaf) -and -not $Force) {
        Assert-Sha256 -Path $outPath -Expected $Asset.Sha256 -Label $Asset.Name
        return
    }

    if ($VerifyOnly) {
        Assert-Sha256 -Path $outPath -Expected $Asset.Sha256 -Label $Asset.Name
        return
    }

    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("carbonpaper-release-assets-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $tmp | Out-Null

    try {
        $downloadPath = Join-Path $tmp $Asset.Output
        Invoke-Download -Url $Asset.Url -OutFile $downloadPath
        Assert-Sha256 -Path $downloadPath -Expected $Asset.Sha256 -Label $Asset.Name
        Move-Item -LiteralPath $downloadPath -Destination $outPath -Force
        Assert-Sha256 -Path $outPath -Expected $Asset.Sha256 -Label $Asset.Name
    } finally {
        if ((Test-Path -LiteralPath $tmp) -and $tmp.StartsWith([System.IO.Path]::GetTempPath(), [System.StringComparison]::OrdinalIgnoreCase)) {
            Remove-Item -LiteralPath $tmp -Recurse -Force
        }
    }
}

function Ensure-Aria2 {
    param([Parameter(Mandatory = $true)][hashtable]$Asset)

    $outPath = Join-Path $RootDir $Asset.Output
    if ((Test-Path -LiteralPath $outPath -PathType Leaf) -and -not $Force) {
        Assert-Sha256 -Path $outPath -Expected $Asset.Sha256 -Label $Asset.Name
        return
    }

    if ($VerifyOnly) {
        Assert-Sha256 -Path $outPath -Expected $Asset.Sha256 -Label $Asset.Name
        return
    }

    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("carbonpaper-release-assets-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $tmp | Out-Null

    try {
        $zipPath = Join-Path $tmp $Asset.ZipName
        $extractDir = Join-Path $tmp "aria2"
        Invoke-Download -Url $Asset.Url -OutFile $zipPath
        Assert-Sha256 -Path $zipPath -Expected $Asset.ZipSha256 -Label "$($Asset.Name) archive"

        Expand-Archive -LiteralPath $zipPath -DestinationPath $extractDir -Force
        $aria2c = Get-ChildItem -LiteralPath $extractDir -Recurse -Filter "aria2c.exe" -File |
            Select-Object -First 1
        if ($null -eq $aria2c) {
            throw "aria2c.exe was not found in $zipPath"
        }

        Assert-Sha256 -Path $aria2c.FullName -Expected $Asset.Sha256 -Label $Asset.Name
        Move-Item -LiteralPath $aria2c.FullName -Destination $outPath -Force
        Assert-Sha256 -Path $outPath -Expected $Asset.Sha256 -Label $Asset.Name
    } finally {
        if ((Test-Path -LiteralPath $tmp) -and $tmp.StartsWith([System.IO.Path]::GetTempPath(), [System.StringComparison]::OrdinalIgnoreCase)) {
            Remove-Item -LiteralPath $tmp -Recurse -Force
        }
    }
}

Write-Host "Preparing CarbonPaper release assets in $RootDir"
Ensure-DownloadedFile -Asset $pythonAsset
Ensure-Aria2 -Asset $aria2Asset
Write-Host "Release assets are ready."
