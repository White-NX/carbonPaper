[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$TestBinary,
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [string]$ExpectedHash,
    [switch]$Elevated
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$serviceTest = 'windows::install::tests::existing_service_repair_reapplies_restart_policy'
$TestBinary = (Resolve-Path -LiteralPath $TestBinary).Path
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
[System.IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
$isAdministrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

function Quote-PowerShellLiteral([string]$Value) {
    return "'" + $Value.Replace("'", "''") + "'"
}

function Get-TestHash([string]$File) {
    # Use .NET directly so Windows PowerShell does not depend on the launching
    # shell's module search path to discover the scripted Get-FileHash command.
    $stream = [System.IO.File]::OpenRead($File)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return [BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-', '')
    } finally {
        $sha.Dispose()
        $stream.Dispose()
    }
}

if (-not $isAdministrator) {
    if ($Elevated -or $env:CI) {
        throw 'The isolated service regression requires an administrator Windows test process.'
    }
    Write-Host '[app-bound] Windows will request administrator access for one temporary service test.'
    $temporaryBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\', '/')
    $temporary = Join-Path $temporaryBase ('carbonpaper-native-check-' + [guid]::NewGuid().ToString('N'))
    [System.IO.Directory]::CreateDirectory($temporary) | Out-Null
    try {
        $copiedTest = Join-Path $temporary 'app-bound-tests.exe'
        Copy-Item -LiteralPath $TestBinary -Destination $copiedTest
        $copiedHash = Get-TestHash $copiedTest
        # Literal quoting is separate from command-line encoding: neither paths
        # nor their dollar signs/backticks are interpreted as PowerShell code.
        $command = '& ' + (Quote-PowerShellLiteral $PSCommandPath) +
            ' -TestBinary ' + (Quote-PowerShellLiteral $copiedTest) +
            ' -OutputDirectory ' + (Quote-PowerShellLiteral $temporary) +
            ' -ExpectedHash ' + (Quote-PowerShellLiteral $copiedHash) + ' -Elevated'
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
        $powershell = Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe'
        $child = Start-Process -FilePath $powershell -Verb RunAs -WindowStyle Hidden -Wait -PassThru `
            -ArgumentList @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', $encoded)
        foreach ($name in @('service-stdout.txt', 'service-stderr.txt', 'service-result.json')) {
            $source = Join-Path $temporary $name
            if (Test-Path -LiteralPath $source) {
                Copy-Item -LiteralPath $source -Destination (Join-Path $OutputDirectory $name)
            }
        }
        foreach ($name in @('service-stdout.txt', 'service-stderr.txt')) {
            $log = Join-Path $OutputDirectory $name
            if (Test-Path -LiteralPath $log) { Get-Content -LiteralPath $log }
        }
        if ($child.ExitCode -ne 0) {
            throw ('The administrator test helper exited with code ' + $child.ExitCode + '.')
        }
        $result = Get-Content -LiteralPath (Join-Path $OutputDirectory 'service-result.json') -Raw | ConvertFrom-Json
        if ($result.exit_code -ne 0 -or $result.tests_passed -ne 1 -or $result.test -ne $serviceTest) {
            throw 'The isolated administrator service regression failed.'
        }
    } finally {
        # Delete only the unique temporary directory created by this invocation.
        $resolved = [System.IO.Path]::GetFullPath((Resolve-Path -LiteralPath $temporary).Path)
        $item = Get-Item -LiteralPath $resolved -Force
        if ([System.IO.Path]::GetDirectoryName($resolved) -ne $temporaryBase -or
            -not $item.Name.StartsWith('carbonpaper-native-check-') -or
            ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint)) {
            throw 'Refusing to remove an unexpected native-test temporary directory.'
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
    exit 0
}

$result = @{ test = $serviceTest; exit_code = 1; tests_passed = 0 }
try {
    if ($ExpectedHash -and (Get-TestHash $TestBinary) -ne $ExpectedHash) {
        throw 'The test executable changed after preparation.'
    }
    $stdout = Join-Path $OutputDirectory 'service-stdout.txt'
    $stderr = Join-Path $OutputDirectory 'service-stderr.txt'
    $process = Start-Process -FilePath $TestBinary -WindowStyle Hidden -Wait -PassThru `
        -ArgumentList @($serviceTest, '--ignored', '--exact', '--nocapture') `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    $result.exit_code = $process.ExitCode
    $output = Get-Content -LiteralPath $stdout -Raw
    if ($output -match '(?m)^test result: ok\. 1 passed; 0 failed; 0 ignored;') {
        $result.tests_passed = 1
    }
    if ($result.exit_code -eq 0 -and $result.tests_passed -ne 1) {
        $result.exit_code = 1
        throw 'The selected service regression did not run; zero tests is not a passing check.'
    }
    Get-Content -LiteralPath $stdout
    Get-Content -LiteralPath $stderr
} catch {
    $result.error = $_.Exception.Message
    $result.exit_code = 1
    Write-Host ('[app-bound] ' + $result.error)
} finally {
    $result | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'service-result.json') -Encoding UTF8
}
exit $result.exit_code
