#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$TargetTag,
    [string]$TargetVersion,
    [string]$CandidateZip,
    [string]$LatestJson,
    [string]$SourcePortableZip,
    [string]$PreviousTag,
    [string]$OldPortableZip,
    [string]$ProductName = "carbonpaper",
    [string]$ManifestVersion,
    [string]$ExpectedAppVersion,
    [int]$TimeoutSeconds = 180,
    [string]$MinPreviousVersionWithSmokeHook = $env:CARBONPAPER_UPDATE_SMOKE_MIN_PREVIOUS_VERSION,
    [switch]$SkipIfPreviousReleaseLacksSmokeSupport,
    [switch]$RequireAppliedMarker,
    [switch]$KeepWorkDir
)

$ErrorActionPreference = "Stop"

function Get-RepoRoot {
    return (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path
}

function Read-ProjectVersion {
    param([string]$RepoRoot)

    $tauriConfPath = Join-Path $RepoRoot "src-tauri\tauri.conf.json"
    $tauriConf = Get-Content -LiteralPath $tauriConfPath -Raw | ConvertFrom-Json
    return [string]$tauriConf.version
}

function Assert-UpdatedRustOcrRuntime {
    param(
        [Parameter(Mandatory = $true)][string]$RepoRoot,
        [Parameter(Mandatory = $true)][string]$InstallDir
    )

    $worker = Join-Path $InstallDir "carbonpaper-ml.exe"
    if (-not (Test-Path -LiteralPath $worker -PathType Leaf)) {
        throw "Updated installation is missing carbonpaper-ml.exe: $worker"
    }

    $manifestPath = Join-Path $RepoRoot "scripts\release-assets\ocr-models.json"
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    $bundleDir = Join-Path $InstallDir ([string]$manifest.bundle_path).Replace('/', '\')
    foreach ($asset in $manifest.files) {
        $path = Join-Path $bundleDir ([string]$asset.name)
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Updated installation is missing Rust OCR model asset: $path"
        }
        $size = (Get-Item -LiteralPath $path).Length
        if ($size -ne [long]$asset.size) {
            throw "Updated Rust OCR model asset has wrong size: $path expected=$($asset.size) actual=$size"
        }
        $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
        if ($hash -ne ([string]$asset.sha256).ToLowerInvariant()) {
            throw "Updated Rust OCR model asset checksum mismatch: $path expected=$($asset.sha256) actual=$hash"
        }
    }

    $previousErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $verifyOutput = & $worker --verify-models --model-dir $bundleDir 2>&1
        $workerExitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
    if ($workerExitCode -ne 0) {
        throw "Updated Rust OCR runtime failed model verification (exit $workerExitCode): $($verifyOutput -join "`n")"
    }
    if (($verifyOutput -join "`n") -notmatch '"model_id":"ppocrv5-ch-mobile"') {
        throw "Updated Rust OCR runtime returned an unexpected verification result: $($verifyOutput -join "`n")"
    }
}

function Compare-SemVerCore {
    param(
        [Parameter(Mandatory = $true)][string]$Left,
        [Parameter(Mandatory = $true)][string]$Right
    )

    $leftCore = (($Left.Trim() -replace '^v', '') -split '[-+]')[0]
    $rightCore = (($Right.Trim() -replace '^v', '') -split '[-+]')[0]
    $leftParts = $leftCore -split '\.'
    $rightParts = $rightCore -split '\.'

    for ($i = 0; $i -lt 3; $i++) {
        $leftValue = 0
        $rightValue = 0
        if ($i -lt $leftParts.Count -and $leftParts[$i] -match '^\d+$') {
            $leftValue = [int]$leftParts[$i]
        }
        if ($i -lt $rightParts.Count -and $rightParts[$i] -match '^\d+$') {
            $rightValue = [int]$rightParts[$i]
        }
        if ($leftValue -lt $rightValue) { return -1 }
        if ($leftValue -gt $rightValue) { return 1 }
    }

    return 0
}

function Get-FreeTcpPort {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    try {
        $listener.Start()
        return ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
    } finally {
        $listener.Stop()
    }
}

function Invoke-Gh {
    param([Parameter(Mandatory = $true)][string[]]$Arguments)

    $output = & gh @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "gh $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
    return $output
}

function Resolve-PreviousReleaseTag {
    param([string]$TargetTag)

    $json = Invoke-Gh -Arguments @(
        "release", "list",
        "--limit", "50",
        "--exclude-drafts",
        "--exclude-pre-releases",
        "--json", "tagName,createdAt"
    )
    $releases = $json | ConvertFrom-Json
    foreach ($release in $releases) {
        if ([string]::IsNullOrWhiteSpace($TargetTag) -or $release.tagName -ne $TargetTag) {
            return [string]$release.tagName
        }
    }
    return $null
}

function Download-PreviousPortableZip {
    param(
        [Parameter(Mandatory = $true)][string]$PreviousTag,
        [Parameter(Mandatory = $true)][string]$DestinationDir,
        [Parameter(Mandatory = $true)][string]$ProductName
    )

    New-Item -ItemType Directory -Force -Path $DestinationDir | Out-Null
    Invoke-Gh -Arguments @(
        "release", "download", $PreviousTag,
        "--pattern", "*_x64_portable.zip",
        "--dir", $DestinationDir,
        "--clobber"
    ) | Out-Null

    $preferredPattern = "${ProductName}_*_x64_portable.zip"
    $zip = Get-ChildItem -LiteralPath $DestinationDir -Filter $preferredPattern -File |
        Sort-Object Name |
        Select-Object -First 1
    if (-not $zip) {
        $zip = Get-ChildItem -LiteralPath $DestinationDir -Filter "*_x64_portable.zip" -File |
            Sort-Object Name |
            Select-Object -First 1
    }
    if (-not $zip) {
        throw "Previous release $PreviousTag does not contain a portable zip asset."
    }

    return $zip.FullName
}

function Test-PreviousReleaseSmokeSupport {
    param([Parameter(Mandatory = $true)][string]$PreviousTag)

    $tempDir = Join-Path ([System.IO.Path]::GetTempPath()) "carbonpaper-update-smoke-manifest-$([guid]::NewGuid().ToString('N'))"
    try {
        New-Item -ItemType Directory -Force -Path $tempDir | Out-Null
        try {
            Invoke-Gh -Arguments @(
                "release", "download", $PreviousTag,
                "--pattern", "latest.json",
                "--dir", $tempDir,
                "--clobber"
            ) | Out-Null
        } catch {
            return $false
        }

        $manifestFile = Get-ChildItem -LiteralPath $tempDir -Filter "latest.json" -File |
            Select-Object -First 1
        if (-not $manifestFile) {
            return $false
        }

        $manifest = Get-Content -LiteralPath $manifestFile.FullName -Raw | ConvertFrom-Json
        return ($manifest.update_smoke_supported -eq $true)
    } catch {
        return $false
    } finally {
        Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Write-StaticServerScript {
    param([Parameter(Mandatory = $true)][string]$Path)

    @'
import http from "node:http";
import { createReadStream, existsSync, statSync } from "node:fs";
import path from "node:path";

const root = path.resolve(process.argv[2]);
const port = Number(process.argv[3]);
const rootPrefix = root.endsWith(path.sep) ? root : `${root}${path.sep}`;

function sendText(res, status, text) {
  res.writeHead(status, { "Content-Type": "text/plain; charset=utf-8" });
  res.end(text);
}

const server = http.createServer((req, res) => {
  try {
    const parsed = new URL(req.url, "http://127.0.0.1");
    const rel = decodeURIComponent(parsed.pathname).replace(/^\/+/, "") || "latest.json";
    const filePath = path.resolve(root, rel);

    if (filePath !== root && !filePath.startsWith(rootPrefix)) {
      sendText(res, 403, "forbidden");
      return;
    }
    if (!existsSync(filePath) || !statSync(filePath).isFile()) {
      sendText(res, 404, "not found");
      return;
    }

    const stat = statSync(filePath);
    const contentType = filePath.endsWith(".json")
      ? "application/json"
      : filePath.endsWith(".zip")
        ? "application/zip"
        : "application/octet-stream";
    res.writeHead(200, {
      "Content-Type": contentType,
      "Content-Length": stat.size,
      "Cache-Control": "no-store",
    });
    createReadStream(filePath).pipe(res);
  } catch (error) {
    sendText(res, 500, String(error?.stack || error));
  }
});

server.listen(port, "127.0.0.1", () => {
  console.log(`serving ${root} on http://127.0.0.1:${port}`);
});
'@ | Set-Content -LiteralPath $Path -Encoding UTF8
}

function Wait-ForServer {
    param(
        [Parameter(Mandatory = $true)][string]$ManifestUrl,
        [Parameter(Mandatory = $true)]$ServerProcess,
        [Parameter(Mandatory = $true)][string]$ServerErrorLog
    )

    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline) {
        if ($ServerProcess.HasExited) {
            $serverError = if (Test-Path -LiteralPath $ServerErrorLog) {
                Get-Content -LiteralPath $ServerErrorLog -Raw
            } else {
                ""
            }
            throw "Local update manifest server exited early. $serverError"
        }

        try {
            Invoke-WebRequest -Uri $ManifestUrl -UseBasicParsing -TimeoutSec 2 | Out-Null
            return
        } catch {
            Start-Sleep -Milliseconds 500
        }
    }

    throw "Timed out waiting for local update manifest server at $ManifestUrl"
}

function Write-SmokeManifest {
    param(
        [Parameter(Mandatory = $true)][string]$LatestJson,
        [Parameter(Mandatory = $true)][string]$OutPath,
        [Parameter(Mandatory = $true)][string]$CandidateUrl,
        [Parameter(Mandatory = $true)][string]$ManifestVersion,
        [Parameter(Mandatory = $true)][string]$CandidateSha256
    )

    $manifest = Get-Content -LiteralPath $LatestJson -Raw | ConvertFrom-Json
    if ($manifest.PSObject.Properties.Name -contains "version") {
        $manifest.version = $ManifestVersion
    } else {
        $manifest | Add-Member -NotePropertyName "version" -NotePropertyValue $ManifestVersion
    }
    if ($manifest.PSObject.Properties.Name -contains "url") {
        $manifest.url = $CandidateUrl
    } else {
        $manifest | Add-Member -NotePropertyName "url" -NotePropertyValue $CandidateUrl
    }
    if ($manifest.PSObject.Properties.Name -contains "sha256") {
        $manifest.sha256 = $CandidateSha256
    } else {
        $manifest | Add-Member -NotePropertyName "sha256" -NotePropertyValue $CandidateSha256
    }
    $manifest | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $OutPath -Encoding UTF8
}

function Save-ProcessEnvironment {
    param([string[]]$Names)

    $saved = @{}
    foreach ($name in $Names) {
        $saved[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
    }
    return $saved
}

function Restore-ProcessEnvironment {
    param([hashtable]$Saved)

    foreach ($name in $Saved.Keys) {
        if ($null -eq $Saved[$name]) {
            Remove-Item -LiteralPath "Env:\$name" -ErrorAction SilentlyContinue
        } else {
            Set-Item -LiteralPath "Env:\$name" -Value $Saved[$name]
        }
    }
}

function Stop-SmokeProcesses {
    param(
        [Parameter(Mandatory = $true)][string]$ProductName,
        [Parameter(Mandatory = $true)][string]$InstallDir
    )

    $installPrefix = (Resolve-Path -LiteralPath $InstallDir).Path
    Get-Process -Name $ProductName -ErrorAction SilentlyContinue |
        Where-Object {
            try {
                $_.Path -and $_.Path.StartsWith($installPrefix, [System.StringComparison]::OrdinalIgnoreCase)
            } catch {
                $false
            }
        } |
        Stop-Process -Force -ErrorAction SilentlyContinue
}

$repoRoot = Get-RepoRoot
if ([string]::IsNullOrWhiteSpace($TargetVersion)) {
    if (-not [string]::IsNullOrWhiteSpace($TargetTag)) {
        $TargetVersion = $TargetTag.TrimStart("v")
    } else {
        $TargetVersion = Read-ProjectVersion -RepoRoot $repoRoot
    }
}
if ([string]::IsNullOrWhiteSpace($TargetTag)) {
    $TargetTag = "v$TargetVersion"
}
if ([string]::IsNullOrWhiteSpace($ManifestVersion)) {
    $ManifestVersion = $TargetVersion
}
if ([string]::IsNullOrWhiteSpace($ExpectedAppVersion)) {
    $ExpectedAppVersion = $TargetVersion
}
if ([string]::IsNullOrWhiteSpace($CandidateZip)) {
    $CandidateZip = Join-Path $repoRoot "src-tauri\target\release\bundle\nsis\${ProductName}_${TargetVersion}_x64_portable.zip"
}
if ([string]::IsNullOrWhiteSpace($LatestJson)) {
    $LatestJson = Join-Path $repoRoot "src-tauri\target\release\bundle\nsis\latest.json"
}
if ([string]::IsNullOrWhiteSpace($SourcePortableZip) -and -not [string]::IsNullOrWhiteSpace($OldPortableZip)) {
    $SourcePortableZip = $OldPortableZip
}

$candidateZipPath = (Resolve-Path -LiteralPath $CandidateZip).Path
$latestJsonPath = (Resolve-Path -LiteralPath $LatestJson).Path

if ([string]::IsNullOrWhiteSpace($SourcePortableZip)) {
    if ([string]::IsNullOrWhiteSpace($PreviousTag)) {
        $PreviousTag = Resolve-PreviousReleaseTag -TargetTag $TargetTag
    }
    if ([string]::IsNullOrWhiteSpace($PreviousTag)) {
        Write-Host "No previous stable release found. Skipping update smoke test for bootstrap release."
        exit 0
    }

    if (-not [string]::IsNullOrWhiteSpace($MinPreviousVersionWithSmokeHook)) {
        $previousVersion = $PreviousTag.TrimStart("v")
        if ((Compare-SemVerCore -Left $previousVersion -Right $MinPreviousVersionWithSmokeHook) -lt 0) {
            Write-Host "Previous release $PreviousTag is older than smoke-hook baseline $MinPreviousVersionWithSmokeHook. Skipping bootstrap smoke test."
            exit 0
        }
    }

    if ($SkipIfPreviousReleaseLacksSmokeSupport) {
        if (-not (Test-PreviousReleaseSmokeSupport -PreviousTag $PreviousTag)) {
            Write-Host "Previous release $PreviousTag does not advertise update_smoke_supported=true. Skipping bootstrap smoke test."
            exit 0
        }
    }
}

$existing = Get-Process -Name $ProductName -ErrorAction SilentlyContinue
if ($existing) {
    $ids = ($existing | ForEach-Object { $_.Id }) -join ", "
    throw "$ProductName is already running (pid: $ids). Stop it before running update smoke test."
}

$workDir = Join-Path ([System.IO.Path]::GetTempPath()) "carbonpaper-update-smoke-$([guid]::NewGuid().ToString('N'))"
$httpRoot = Join-Path $workDir "http"
$oldInstallDir = Join-Path $workDir "old-install"
$previousDownloadDir = Join-Path $workDir "previous-release"
$localAppData = Join-Path $workDir "localappdata"
$roamingAppData = Join-Path $workDir "appdata"
$resultFile = Join-Path $workDir "update-smoke-result.json"
$serverScript = Join-Path $workDir "server.mjs"
$serverOut = Join-Path $workDir "server.out.log"
$serverErr = Join-Path $workDir "server.err.log"
$serverProcess = $null
$succeeded = $false
$envNames = @(
    "CARBONPAPER_UPDATE_SMOKE_TEST",
    "CARBONPAPER_UPDATE_MANIFEST_URL",
    "CARBONPAPER_UPDATE_SMOKE_PUBLIC_KEY",
    "CARBONPAPER_UPDATE_SMOKE_RESULT",
    "CARBONPAPER_UPDATE_SMOKE_EXPECTED_VERSION",
    "CARBONPAPER_UPDATE_SMOKE_EXPECTED_MANIFEST_VERSION",
    "CARBONPAPER_UPDATE_SMOKE_REQUIRE_APPLIED",
    "CARBONPAPER_UPDATE_SMOKE_APPLIED",
    "CARBONPAPER_START_HIDDEN",
    "LOCALAPPDATA",
    "APPDATA"
)
$savedEnv = Save-ProcessEnvironment -Names $envNames

try {
    New-Item -ItemType Directory -Force -Path $httpRoot, $oldInstallDir, $localAppData, $roamingAppData | Out-Null

    if ([string]::IsNullOrWhiteSpace($SourcePortableZip)) {
        Write-Host "Downloading previous portable zip from release $PreviousTag..."
        $SourcePortableZip = Download-PreviousPortableZip -PreviousTag $PreviousTag -DestinationDir $previousDownloadDir -ProductName $ProductName
    }
    $sourcePortableZipPath = (Resolve-Path -LiteralPath $SourcePortableZip).Path

    Write-Host "Expanding source portable zip: $sourcePortableZipPath"
    Expand-Archive -LiteralPath $sourcePortableZipPath -DestinationPath $oldInstallDir -Force
    $oldExe = Join-Path $oldInstallDir "$ProductName.exe"
    if (-not (Test-Path -LiteralPath $oldExe)) {
        $oldExe = Get-ChildItem -LiteralPath $oldInstallDir -Filter "$ProductName.exe" -Recurse -File |
            Select-Object -First 1 |
            ForEach-Object { $_.FullName }
    }
    if (-not $oldExe -or -not (Test-Path -LiteralPath $oldExe)) {
        throw "Could not find $ProductName.exe in extracted previous portable zip."
    }

    $candidateZipName = Split-Path -Leaf $candidateZipPath
    $candidateSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $candidateZipPath).Hash.ToLowerInvariant()
    Copy-Item -LiteralPath $candidateZipPath -Destination (Join-Path $httpRoot $candidateZipName) -Force

    $port = Get-FreeTcpPort
    $candidateUrl = "http://127.0.0.1:$port/$candidateZipName"
    $manifestUrl = "http://127.0.0.1:$port/latest.json"
    $smokeManifestPath = Join-Path $httpRoot "latest.json"
    Write-SmokeManifest -LatestJson $latestJsonPath -OutPath $smokeManifestPath -CandidateUrl $candidateUrl -ManifestVersion $ManifestVersion -CandidateSha256 $candidateSha256
    $signScript = Join-Path $repoRoot "scripts\sign-update-manifest.mjs"
    $smokePublicKey = [string](@(& node $signScript $smokeManifestPath) | Select-Object -Last 1)
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($smokePublicKey)) {
        throw "Failed to sign update smoke manifest with $signScript"
    }
    $smokePublicKey = $smokePublicKey.Trim()
    Write-StaticServerScript -Path $serverScript

    Write-Host "Starting local update server: $manifestUrl"
    $serverProcess = Start-Process -FilePath "node" `
        -ArgumentList @("`"$serverScript`"", "`"$httpRoot`"", "$port") `
        -PassThru `
        -WindowStyle Hidden `
        -RedirectStandardOutput $serverOut `
        -RedirectStandardError $serverErr
    Wait-ForServer -ManifestUrl $manifestUrl -ServerProcess $serverProcess -ServerErrorLog $serverErr

    $env:CARBONPAPER_UPDATE_SMOKE_TEST = "1"
    $env:CARBONPAPER_UPDATE_MANIFEST_URL = $manifestUrl
    $env:CARBONPAPER_UPDATE_SMOKE_PUBLIC_KEY = $smokePublicKey
    $env:CARBONPAPER_UPDATE_SMOKE_RESULT = $resultFile
    $env:CARBONPAPER_UPDATE_SMOKE_EXPECTED_VERSION = $ExpectedAppVersion
    $env:CARBONPAPER_UPDATE_SMOKE_EXPECTED_MANIFEST_VERSION = $ManifestVersion
    if ($RequireAppliedMarker) {
        $env:CARBONPAPER_UPDATE_SMOKE_REQUIRE_APPLIED = "1"
    } else {
        Remove-Item -LiteralPath "Env:\CARBONPAPER_UPDATE_SMOKE_REQUIRE_APPLIED" -ErrorAction SilentlyContinue
    }
    Remove-Item -LiteralPath "Env:\CARBONPAPER_UPDATE_SMOKE_APPLIED" -ErrorAction SilentlyContinue
    $env:CARBONPAPER_START_HIDDEN = "1"
    $env:LOCALAPPDATA = $localAppData
    $env:APPDATA = $roamingAppData

    $updateErrorLog = Join-Path $localAppData "CarbonPaper\update_error.log"
    Remove-Item -LiteralPath $updateErrorLog -Force -ErrorAction SilentlyContinue

    Write-Host "Starting source app for update smoke test: $oldExe"
    $oldProcess = Start-Process -FilePath $oldExe `
        -ArgumentList @("--hidden") `
        -WorkingDirectory (Split-Path -Parent $oldExe) `
        -PassThru `
        -WindowStyle Hidden

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    $lastPhase = $null
    $lastStatus = $null
    $result = $null

    while ((Get-Date) -lt $deadline) {
        if ($serverProcess.HasExited) {
            $serverError = if (Test-Path -LiteralPath $serverErr) { Get-Content -LiteralPath $serverErr -Raw } else { "" }
            throw "Local update server exited during smoke test. $serverError"
        }

        if (Test-Path -LiteralPath $resultFile) {
            try {
                $result = Get-Content -LiteralPath $resultFile -Raw | ConvertFrom-Json
            } catch {
                $result = $null
            }

            if ($result -and ($result.phase -ne $lastPhase -or $result.status -ne $lastStatus)) {
                Write-Host "Smoke status: status=$($result.status) phase=$($result.phase) current=$($result.current_version) target=$($result.target_version)"
                $lastPhase = $result.phase
                $lastStatus = $result.status
            }

            if ($result -and $result.status -eq "success") {
                if ($result.current_version -ne $ExpectedAppVersion) {
                    throw "Updated app reported version $($result.current_version), expected $ExpectedAppVersion."
                }
                Assert-UpdatedRustOcrRuntime -RepoRoot $repoRoot -InstallDir (Split-Path -Parent $oldExe)
                if (Test-Path -LiteralPath $updateErrorLog) {
                    $updateError = Get-Content -LiteralPath $updateErrorLog -Raw
                    throw "Update script wrote update_error.log: $updateError"
                }
                $succeeded = $true
                Write-Host "Update smoke test passed: source app updated via manifest $ManifestVersion and started app version $ExpectedAppVersion."
                break
            }

            if ($result -and $result.status -eq "failure") {
                throw "Update smoke test failed in app: $($result.error)"
            }
        }

        if ($oldProcess.HasExited -and -not (Test-Path -LiteralPath $resultFile)) {
            throw "Previous app exited before writing update smoke status."
        }

        Start-Sleep -Seconds 1
    }

    if (-not $succeeded) {
        $statusDump = if (Test-Path -LiteralPath $resultFile) {
            Get-Content -LiteralPath $resultFile -Raw
        } else {
            "<no status file>"
        }
        throw "Timed out after $TimeoutSeconds seconds waiting for update smoke success. Last status: $statusDump"
    }
} finally {
    Restore-ProcessEnvironment -Saved $savedEnv

    if ($serverProcess -and -not $serverProcess.HasExited) {
        Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
    }

    if (Test-Path -LiteralPath $oldInstallDir) {
        Stop-SmokeProcesses -ProductName $ProductName -InstallDir $oldInstallDir
    }

    if ($succeeded -and -not $KeepWorkDir) {
        Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction SilentlyContinue
    } else {
        Write-Host "Update smoke work directory: $workDir"
    }
}
