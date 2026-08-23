[CmdletBinding()]
param(
    [switch]$AllRust
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot

function Invoke-CheckedNative {
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    Write-Host $Label
    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Label failed with exit code $LASTEXITCODE"
    }
}

Push-Location $repoRoot
try {
    Invoke-CheckedNative `
        -Label 'Checking Rust formatting...' `
        -FilePath 'cargo' `
        -Arguments @('fmt', '--manifest-path', 'src-tauri/Cargo.toml', '--', '--check')

    Invoke-CheckedNative `
        -Label 'Checking command security policy...' `
        -FilePath 'node' `
        -Arguments @('scripts/security-guards.cjs')

    $tests = @(
        'storage::database_snapshot',
        'storage::migration::data_dir',
        'storage::connection::tests',
        'storage::mode',
        'credential_manager::tests::import_snapshot_restores_credential_file_and_cached_state'
    )

    foreach ($test in $tests) {
        Invoke-CheckedNative `
            -Label "Running Rust tests: $test" `
            -FilePath 'cargo' `
            -Arguments @(
                'test', '--manifest-path', 'src-tauri/Cargo.toml',
                '--lib', $test, '--no-default-features'
            )
    }

    if ($AllRust) {
        Invoke-CheckedNative `
            -Label 'Running the complete Rust library suite...' `
            -FilePath 'cargo' `
            -Arguments @(
                'test', '--manifest-path', 'src-tauri/Cargo.toml',
                '--lib', '--no-default-features'
            )
    }

    Write-Host 'WAL foundation verification passed.'
}
finally {
    Pop-Location
}
