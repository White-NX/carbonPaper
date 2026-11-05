#Requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$CandidateZip,
    [Parameter(Mandatory = $true)][string]$LatestJson,
    [string]$ProductName = 'carbonpaper'
)
$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$candidatePath = (Resolve-Path -LiteralPath $CandidateZip).Path
$manifestPath = (Resolve-Path -LiteralPath $LatestJson).Path
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
if ((Get-FileHash -LiteralPath $candidatePath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $manifest.sha256.ToLowerInvariant()) {
    throw 'Candidate ZIP does not match latest.json'
}
$tempParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
$workDir = Join-Path $tempParent ('carbonpaper-update-smoke-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $workDir | Out-Null
try {
    Expand-Archive -LiteralPath $candidatePath -DestinationPath $workDir
    $executable = Join-Path $workDir "$ProductName.exe"
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'Candidate executable is missing' }
    & node (Join-Path $PSScriptRoot 'verify-update-policy.mjs') $executable $manifestPath (Join-Path $repoRoot 'src-tauri/update-public-key.txt')
    if ($LASTEXITCODE -ne 0) { throw 'Packaged update verification policy failed' }
    Write-Host 'Release update policy checks passed. Native installation behavior is covered by windows::update::tests.'
} finally {
    $resolved = (Resolve-Path -LiteralPath $workDir).Path
    if (-not [string]::Equals([IO.Path]::GetDirectoryName($resolved), $tempParent, [StringComparison]::OrdinalIgnoreCase) -or
        -not [IO.Path]::GetFileName($resolved).StartsWith('carbonpaper-update-smoke-')) { throw 'Unsafe smoke cleanup path' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
