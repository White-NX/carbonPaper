[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Zip,
    [Parameter(Mandatory = $true)][string]$Metadata,
    [Parameter(Mandatory = $true)][string]$PublicKey,
    [Parameter(Mandatory = $true)][string]$Version
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$Zip = (Resolve-Path -LiteralPath $Zip).Path
if ((Get-Item -LiteralPath $Zip).Length -gt 64MB) { throw 'Service repair ZIP exceeds 64 MiB' }
$temporaryBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\', '/')
$temporary = Join-Path $temporaryBase ('carbonpaper-repair-verify-' + [guid]::NewGuid().ToString('N'))
[System.IO.Directory]::CreateDirectory($temporary) | Out-Null
$archive = $null
try {
    $archive = [System.IO.Compression.ZipFile]::OpenRead($Zip)
    $expected = @('carbonpaper-key-service.exe', 'carbonpaper-protected-setup.exe', 'protected-runtime.json', 'protected-runtime.sig')
    if ($archive.Entries.Count -ne $expected.Count) { throw 'Service repair ZIP must contain exactly four files' }
    $expanded = 0L
    foreach ($entry in $archive.Entries) {
        if ($entry.FullName -cnotin $expected) { throw "Unexpected service repair entry: $($entry.FullName)" }
        $expanded += $entry.Length
        if ($expanded -gt 128MB) { throw 'Expanded service repair ZIP exceeds 128 MiB' }
        $target = Join-Path $temporary $entry.FullName
        # Fixed leaf names and no overwrite also reject duplicate entries.
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $target, $false)
    }
    & node (Join-Path $PSScriptRoot 'verify-service-repair.mjs') $temporary $Zip $Metadata $PublicKey $Version
    if ($LASTEXITCODE -ne 0) { throw 'Service repair package verification failed' }
} finally {
    if ($archive) { $archive.Dispose() }
    $resolved = [System.IO.Path]::GetFullPath((Resolve-Path -LiteralPath $temporary).Path)
    $item = Get-Item -LiteralPath $resolved -Force
    if ([System.IO.Path]::GetDirectoryName($resolved) -ne $temporaryBase -or
        -not $item.Name.StartsWith('carbonpaper-repair-verify-') -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint)) {
        throw 'Refusing to remove an unexpected repair verification directory'
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
