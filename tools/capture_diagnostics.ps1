# CarbonPaper System Diagnostics Capture
# Usage: Right-click -> Run with PowerShell, or: powershell -ExecutionPolicy Bypass -File capture_diagnostics.ps1
# Press Ctrl+C to stop. Output: diagnostics_<timestamp>.csv

$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$outFile = "$PSScriptRoot\diagnostics_$timestamp.csv"

# CSV header
$header = "Timestamp,SystemAvailableMB,SystemCommittedMB,SystemCommitLimitMB,PageFilePct," +
          "PythonPID,PythonWorkingSetMB,PythonPrivateMB,PythonPageFaultsDelta," +
          "CarbonPaperPID,CarbonPaperWorkingSetMB,CarbonPaperPrivateMB," +
          "DiskQueueLength,DiskReadKBps,DiskWriteKBps," +
          "GPUUsagePct,GPUMemUsedMB,GPUMemTotalMB"
$header | Out-File -FilePath $outFile -Encoding UTF8

Write-Host "=== CarbonPaper Diagnostics Capture ===" -ForegroundColor Cyan
Write-Host "Output: $outFile" -ForegroundColor Green
Write-Host "Press Ctrl+C to stop." -ForegroundColor Yellow
Write-Host ""

# Cache previous page fault counts for delta calculation
$prevPythonPageFaults = 0

# Try to get GPU info via performance counters
$hasGpuCounters = $false
try {
    $gpuEngines = (Get-Counter -ListSet "GPU Engine" -ErrorAction Stop).Paths
    $hasGpuCounters = $true
} catch {
    Write-Host "[WARN] GPU performance counters not available, GPU columns will be empty." -ForegroundColor DarkYellow
}

while ($true) {
    $now = Get-Date -Format "yyyy-MM-ddTHH:mm:ss.fff"

    # --- System Memory ---
    $os = Get-CimInstance Win32_OperatingSystem
    $availMB = [math]::Round($os.FreePhysicalMemory / 1024, 1)
    $totalMB = [math]::Round($os.TotalVisibleMemorySize / 1024, 1)
    $commitLimitMB = [math]::Round(($os.TotalVirtualMemorySize) / 1024, 1)
    $committedMB = [math]::Round(($totalMB - $availMB), 1)  # approximation

    # Page file usage
    try {
        $pageFiles = Get-CimInstance Win32_PageFileUsage
        $pfUsed = ($pageFiles | Measure-Object -Property CurrentUsage -Sum).Sum
        $pfTotal = ($pageFiles | Measure-Object -Property AllocatedBaseSize -Sum).Sum
        if ($pfTotal -gt 0) {
            $pfPct = [math]::Round($pfUsed / $pfTotal * 100, 1)
        } else {
            $pfPct = 0
        }
    } catch {
        $pfPct = -1
    }

    # Committed bytes (more accurate)
    try {
        $perfOS = Get-CimInstance Win32_PerfFormattedData_PerfOS_Memory
        $committedMB = [math]::Round($perfOS.CommittedBytes / 1MB, 1)
        $commitLimitMB = [math]::Round($perfOS.CommitLimit / 1MB, 1)
    } catch {}

    # --- Python Process ---
    $pythonPID = ""; $pyWsMB = ""; $pyPrivMB = ""; $pyPfDelta = ""
    $pyProcs = Get-Process -Name "python","python3","pythonw" -ErrorAction SilentlyContinue |
               Where-Object { $_.CommandLine -match "monitor" -or $_.MainWindowTitle -eq "" }
    if (-not $pyProcs) {
        # Fallback: find python child of carbonpaper
        $pyProcs = Get-Process -Name "python","python3","pythonw" -ErrorAction SilentlyContinue
    }
    if ($pyProcs) {
        # Take the one with highest working set (likely the monitor process)
        $py = $pyProcs | Sort-Object WorkingSet64 -Descending | Select-Object -First 1
        $pythonPID = $py.Id
        $pyWsMB = [math]::Round($py.WorkingSet64 / 1MB, 1)
        $pyPrivMB = [math]::Round($py.PrivateMemorySize64 / 1MB, 1)

        $currentPf = $py.PageFaultCount  # note: this is a cumulative counter
        if ($prevPythonPageFaults -gt 0) {
            $pyPfDelta = $currentPf - $prevPythonPageFaults
        } else {
            $pyPfDelta = 0
        }
        $prevPythonPageFaults = $currentPf
    }

    # --- CarbonPaper Process ---
    $cpPID = ""; $cpWsMB = ""; $cpPrivMB = ""
    $cpProc = Get-Process -Name "carbonpaper","CarbonPaper" -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $cpProc) {
        $cpProc = Get-Process | Where-Object { $_.ProcessName -match "carbon" } | Select-Object -First 1
    }
    if ($cpProc) {
        $cpPID = $cpProc.Id
        $cpWsMB = [math]::Round($cpProc.WorkingSet64 / 1MB, 1)
        $cpPrivMB = [math]::Round($cpProc.PrivateMemorySize64 / 1MB, 1)
    }

    # --- Disk I/O ---
    $diskQ = ""; $diskR = ""; $diskW = ""
    try {
        $diskPerf = Get-CimInstance Win32_PerfFormattedData_PerfDisk_PhysicalDisk |
                    Where-Object { $_.Name -eq "_Total" }
        if ($diskPerf) {
            $diskQ = $diskPerf.CurrentDiskQueueLength
            $diskR = [math]::Round($diskPerf.DiskReadBytesPerSec / 1KB, 1)
            $diskW = [math]::Round($diskPerf.DiskWriteBytesPerSec / 1KB, 1)
        }
    } catch {}

    # --- GPU ---
    $gpuPct = ""; $gpuMemUsed = ""; $gpuMemTotal = ""
    try {
        # Use nvidia-smi if available (most reliable for NVIDIA)
        $nvsmi = & "nvidia-smi" --query-gpu=utilization.gpu,memory.used,memory.total --format=csv,noheader,nounits 2>$null
        if ($LASTEXITCODE -eq 0 -and $nvsmi) {
            $parts = $nvsmi.Split(",") | ForEach-Object { $_.Trim() }
            $gpuPct = $parts[0]
            $gpuMemUsed = $parts[1]
            $gpuMemTotal = $parts[2]
        }
    } catch {}

    # --- Write CSV row ---
    $row = "$now,$availMB,$committedMB,$commitLimitMB,$pfPct," +
           "$pythonPID,$pyWsMB,$pyPrivMB,$pyPfDelta," +
           "$cpPID,$cpWsMB,$cpPrivMB," +
           "$diskQ,$diskR,$diskW," +
           "$gpuPct,$gpuMemUsed,$gpuMemTotal"
    $row | Out-File -FilePath $outFile -Append -Encoding UTF8

    # --- Console output (compact) ---
    $memBar = [math]::Round(($totalMB - $availMB) / $totalMB * 100, 0)
    $status = "[$now] Mem: ${memBar}% (avail: ${availMB}MB) | PageFile: ${pfPct}%"
    if ($pyWsMB) { $status += " | Py: ${pyWsMB}MB(ws)/${pyPrivMB}MB(priv) pf:$pyPfDelta" }
    if ($cpWsMB) { $status += " | CP: ${cpWsMB}MB" }
    if ($diskQ) { $status += " | DiskQ: $diskQ R:${diskR}KB/s W:${diskW}KB/s" }
    if ($gpuPct) { $status += " | GPU: ${gpuPct}% VRAM: ${gpuMemUsed}/${gpuMemTotal}MB" }
    Write-Host $status

    Start-Sleep -Seconds 1
}
