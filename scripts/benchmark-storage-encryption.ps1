[CmdletBinding()]
param(
    [int]$Rows = 100000,
    [int]$Samples = 5,
    [int]$Candidates = 512,
    [int]$GraphMiB = 32,
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$benchmarkRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../tools/storage-encryption-bench'))
$manifest = Join-Path $benchmarkRoot 'Cargo.toml'
& cargo build --locked --release --manifest-path $manifest
if ($LASTEXITCODE -ne 0) { throw 'The isolated benchmark did not compile.' }
$executable = Join-Path $benchmarkRoot 'target/release/carbonpaper-storage-encryption-bench.exe'
$raw = & $executable --rows $Rows --samples $Samples --candidates $Candidates --graph-mib $GraphMiB
if ($LASTEXITCODE -ne 0) { throw 'The isolated benchmark did not complete.' }
$report = ($raw -join "`n") | ConvertFrom-Json
$report | Add-Member -NotePropertyName measured_at_utc -NotePropertyValue ([DateTime]::UtcNow.ToString('o'))
$report | Add-Member -NotePropertyName cpu -NotePropertyValue (((Get-CimInstance Win32_Processor | Select-Object -ExpandProperty Name) -join '; ').Trim())
$report | Add-Member -NotePropertyName os -NotePropertyValue ([Environment]::OSVersion.VersionString)
$report | Add-Member -NotePropertyName rustc -NotePropertyValue ((& rustc --version) -join ' ')
if (-not $OutputPath) {
    $OutputPath = Join-Path ([System.IO.Path]::GetTempPath()) 'carbonpaper-storage-encryption-benchmark.json'
}
$json = ($report | ConvertTo-Json -Depth 12) + "`n"
[System.IO.File]::WriteAllText([System.IO.Path]::GetFullPath($OutputPath), $json, [System.Text.UTF8Encoding]::new($false))
Write-Host ('Synthetic benchmark report: ' + [System.IO.Path]::GetFullPath($OutputPath))
$report.results | ForEach-Object {
    [pscustomobject]@{
        Dimensions = $_.dimensions
        Rows = $_.rows
        PlainScanMs = [Math]::Round($_.full_scan_plain_mmap.median_ms, 3)
        EncryptedScanMs = [Math]::Round($_.full_scan_encrypted_uncached.median_ms, 3)
        ColdCandidatesMs = [Math]::Round($_.candidates_encrypted_cache_cold.median_ms, 3)
        HotCandidatesMs = [Math]::Round($_.candidates_encrypted_cache_hot_100_batches.median_ms / 100, 3)
        GraphDecryptMs = [Math]::Round($_.graph_decrypt.median_ms, 3)
    }
} | Format-Table -AutoSize
