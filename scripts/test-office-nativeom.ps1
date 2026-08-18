[CmdletBinding()]
param(
    [string]$RootDir,
    [string]$WorkerPath,
    [ValidateSet("word", "excel", "power_point")]
    [string[]]$Applications = @("word", "excel", "power_point")
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

if ([string]::IsNullOrWhiteSpace($RootDir)) {
    $RootDir = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
} else {
    $RootDir = (Resolve-Path $RootDir).Path
}

if ([string]::IsNullOrWhiteSpace($WorkerPath)) {
    $WorkerPath = Join-Path $RootDir "src-tauri\target\debug\carbonpaper-office.exe"
}
$WorkerPath = [System.IO.Path]::GetFullPath($WorkerPath)
if (-not (Test-Path -LiteralPath $WorkerPath -PathType Leaf)) {
    throw "Office worker is missing: $WorkerPath. Run npm run build:office first."
}

if (-not ("CarbonPaper.OfficeProbe.Win32" -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

namespace CarbonPaper.OfficeProbe {
    public static class Win32 {
        private delegate bool EnumChildProc(IntPtr hwnd, IntPtr lParam);

        [DllImport("user32.dll")]
        private static extern bool EnumChildWindows(
            IntPtr parent,
            EnumChildProc callback,
            IntPtr lParam
        );

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        private static extern int GetClassName(
            IntPtr hwnd,
            StringBuilder className,
            int maxCount
        );

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        private static extern int GetWindowText(
            IntPtr hwnd,
            StringBuilder text,
            int maxCount
        );

        [DllImport("user32.dll")]
        private static extern uint GetWindowThreadProcessId(
            IntPtr hwnd,
            out uint processId
        );

        [DllImport("user32.dll")]
        private static extern bool IsWindowVisible(IntPtr hwnd);

        public static long FindChild(long rootHwnd, uint expectedPid, string[] expectedClasses) {
            long first = 0;
            int firstPriority = Int32.MaxValue;
            long visible = 0;
            int visiblePriority = Int32.MaxValue;
            EnumChildWindows(new IntPtr(rootHwnd), (hwnd, _) => {
                uint pid;
                GetWindowThreadProcessId(hwnd, out pid);
                if (pid != expectedPid) return true;

                var className = new StringBuilder(256);
                GetClassName(hwnd, className, className.Capacity);
                int priority = Array.FindIndex(expectedClasses, candidate => String.Equals(
                    className.ToString(),
                    candidate,
                    StringComparison.OrdinalIgnoreCase
                ));
                if (priority < 0) return true;

                if (priority < firstPriority) {
                    first = hwnd.ToInt64();
                    firstPriority = priority;
                }
                if (IsWindowVisible(hwnd) && priority < visiblePriority) {
                    visible = hwnd.ToInt64();
                    visiblePriority = priority;
                }
                return true;
            }, IntPtr.Zero);
            return visible != 0 ? visible : first;
        }

        public static string DescribeChildClasses(long rootHwnd, uint expectedPid) {
            var classes = new List<string>();
            EnumChildWindows(new IntPtr(rootHwnd), (hwnd, _) => {
                uint pid;
                GetWindowThreadProcessId(hwnd, out pid);
                if (pid != expectedPid) return true;

                var className = new StringBuilder(256);
                GetClassName(hwnd, className, className.Capacity);
                var value = className.ToString();
                if (value.Length > 0 && !classes.Contains(value)) {
                    classes.Add(value);
                }
                return true;
            }, IntPtr.Zero);
            return String.Join(", ", classes);
        }

        public static string WindowTitle(long hwnd) {
            var title = new StringBuilder(512);
            GetWindowText(new IntPtr(hwnd), title, title.Capacity);
            return title.ToString();
        }

        public static uint WindowPid(long hwnd) {
            uint pid;
            GetWindowThreadProcessId(new IntPtr(hwnd), out pid);
            return pid;
        }
    }
}
"@
}

function Read-ExactBytes {
    param(
        [Parameter(Mandatory = $true)][System.IO.Stream]$Stream,
        [Parameter(Mandatory = $true)][int]$Count,
        [int]$TimeoutMs = 10000
    )

    $buffer = [byte[]]::new($Count)
    $offset = 0
    while ($offset -lt $Count) {
        $task = $Stream.ReadAsync($buffer, $offset, $Count - $offset)
        if (-not $task.Wait($TimeoutMs)) {
            throw "Timed out reading Office worker output"
        }
        $read = $task.Result
        if ($read -le 0) {
            throw "Office worker closed its output"
        }
        $offset += $read
    }
    return $buffer
}

function Read-OfficeFrame {
    param([Parameter(Mandatory = $true)][System.IO.Stream]$Stream)

    $lengthBytes = Read-ExactBytes -Stream $Stream -Count 4
    $length = [BitConverter]::ToUInt32($lengthBytes, 0)
    if ($length -lt 1 -or $length -gt 65536) {
        throw "Invalid Office worker frame length: $length"
    }
    $body = Read-ExactBytes -Stream $Stream -Count ([int]$length)
    return [Text.Encoding]::UTF8.GetString($body)
}

function Write-OfficeFrame {
    param(
        [Parameter(Mandatory = $true)][System.IO.Stream]$Stream,
        [Parameter(Mandatory = $true)][string]$Json
    )

    $body = [Text.Encoding]::UTF8.GetBytes($Json)
    if ($body.Length -gt 65536) {
        throw "Office worker request exceeds the 64 KiB protocol limit"
    }
    $lengthBytes = [BitConverter]::GetBytes([uint32]$body.Length)
    $Stream.Write($lengthBytes, 0, $lengthBytes.Length)
    $Stream.Write($body, 0, $body.Length)
    $Stream.Flush()
}

function Wait-OfficeWindowContext {
    param(
        [Parameter(Mandatory = $true)][scriptblock]$GetRootHwnd,
        [Parameter(Mandatory = $true)][string[]]$NativeClasses,
        [int]$TimeoutMs = 10000
    )

    $nativeClassLabel = $NativeClasses -join "/"
    Write-Verbose "Waiting for a stable $nativeClassLabel NativeOM surface"
    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
    $previous = $null
    $attempt = 0
    while ([DateTime]::UtcNow -lt $deadline) {
        $attempt++
        $rootHwnd = [long](& $GetRootHwnd)
        if ($rootHwnd -ne 0) {
            $windowPid = [CarbonPaper.OfficeProbe.Win32]::WindowPid($rootHwnd)
            $title = [CarbonPaper.OfficeProbe.Win32]::WindowTitle($rootHwnd)
            $documentHwnd = [CarbonPaper.OfficeProbe.Win32]::FindChild(
                $rootHwnd,
                $windowPid,
                $NativeClasses
            )
            if ($windowPid -ne 0 -and $documentHwnd -ne 0 -and -not [string]::IsNullOrEmpty($title)) {
                $current = "$rootHwnd|$documentHwnd|$windowPid|$title"
                if ($current -eq $previous) {
                    Write-Verbose "Found stable $nativeClassLabel surface root=$rootHwnd document=$documentHwnd pid=$windowPid"
                    return [pscustomobject]@{
                        RootHwnd = $rootHwnd
                        DocumentHwnd = $documentHwnd
                        Pid = $windowPid
                        Title = $title
                    }
                }
                $previous = $current
            }
        }
        if (($attempt % 5) -eq 0) {
            Write-Verbose "NativeOM wait attempt=$attempt root=$rootHwnd"
            if ($attempt -eq 5 -and $rootHwnd -ne 0) {
                $rootPid = [CarbonPaper.OfficeProbe.Win32]::WindowPid($rootHwnd)
                $classes = [CarbonPaper.OfficeProbe.Win32]::DescribeChildClasses(
                    $rootHwnd,
                    $rootPid
                )
                Write-Verbose "NativeOM child classes: $classes"
            }
        }
        Start-Sleep -Milliseconds 200
    }
    throw "Office window did not expose a stable $nativeClassLabel NativeOM surface"
}

function Invoke-OfficeResolution {
    param(
        [Parameter(Mandatory = $true)][string]$Application,
        [Parameter(Mandatory = $true)]$WindowContext,
        [Parameter(Mandatory = $true)][string]$ExpectedPath
    )

    Write-Verbose "Starting Office worker for $Application"
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $WorkerPath
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardInput = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $worker = [Diagnostics.Process]::Start($startInfo)

    try {
        $stdin = $worker.StandardInput.BaseStream
        $stdout = $worker.StandardOutput.BaseStream
        $ready = Read-OfficeFrame -Stream $stdout | ConvertFrom-Json
        if ($ready.status -ne "ready" -or $ready.protocol_version -ne 1) {
            throw "Office worker returned an invalid handshake"
        }
        Write-Verbose "Office worker handshake completed for $Application"

        $request = @{
            command = "resolve"
            request_id = 1
            generation = 1
            application = $Application
            root_hwnd = $WindowContext.RootHwnd
            document_hwnd = $WindowContext.DocumentHwnd
            pid = $WindowContext.Pid
            title = $WindowContext.Title
        } | ConvertTo-Json -Compress
        Write-OfficeFrame -Stream $stdin -Json $request
        Write-Verbose "Sent NativeOM resolution request for $Application"
        $response = Read-OfficeFrame -Stream $stdout | ConvertFrom-Json

        if ($response.status -ne "resolved" -or $null -eq $response.document) {
            $details = $response | ConvertTo-Json -Compress -Depth 8
            throw "Office NativeOM resolution failed: $details"
        }
        if ($response.document.application -ne $Application) {
            throw "Office worker returned the wrong application"
        }
        if ($response.document.kind -ne "local_file") {
            throw "Office worker returned $($response.document.kind), expected local_file"
        }

        $expected = [System.IO.Path]::GetFullPath($ExpectedPath)
        $actual = [System.IO.Path]::GetFullPath([string]$response.document.locator)
        if (-not $actual.Equals($expected, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Office locator mismatch: expected $expected, got $actual"
        }

        Write-Verbose "Resolved $Application document in $($response.elapsed_ms) ms"
        return [pscustomobject]@{
            application = $Application
            display_name = [string]$response.document.display_name
            kind = [string]$response.document.kind
            native_hwnd = [long]$WindowContext.DocumentHwnd
            elapsed_ms = [double]$response.elapsed_ms
            locator_matches = $true
        }
    } finally {
        if (-not $worker.HasExited) {
            try { $worker.Kill() } catch {}
            try { $worker.WaitForExit(3000) | Out-Null } catch {}
        }
        $worker.Dispose()
    }
}

function Release-ComObject {
    param($Value)
    if ($null -ne $Value -and [Runtime.InteropServices.Marshal]::IsComObject($Value)) {
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($Value)
    }
}

function Assert-NoOfficeProcess {
    param(
        [Parameter(Mandatory = $true)][string]$ProcessName,
        [Parameter(Mandatory = $true)][string]$ApplicationName
    )

    if (Get-Process -Name $ProcessName -ErrorAction SilentlyContinue) {
        throw "$ApplicationName must be closed before running its NativeOM integration probe"
    }
}

function Find-OfficeTemplate {
    param(
        [Parameter(Mandatory = $true)][string[]]$Patterns
    )

    $roots = @(
        (Join-Path ${env:ProgramFiles} "Microsoft Office\root\Templates"),
        (Join-Path ${env:ProgramFiles(x86)} "Microsoft Office\root\Templates"),
        (Join-Path ${env:ProgramFiles} "Microsoft Office\root\Office16\2052"),
        (Join-Path ${env:ProgramFiles(x86)} "Microsoft Office\root\Office16\2052")
    ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) -and (Test-Path -LiteralPath $_) }

    foreach ($root in $roots) {
        foreach ($pattern in $Patterns) {
            $match = Get-ChildItem -LiteralPath $root -Recurse -File -Filter $pattern -ErrorAction SilentlyContinue |
                Select-Object -First 1
            if ($null -ne $match) {
                return $match.FullName
            }
        }
    }
    throw "No installed Office template matched: $($Patterns -join ', ')"
}

function Test-WordNativeOm {
    param([Parameter(Mandatory = $true)][string]$TempDir)

    $application = $null
    $document = $null
    $window = $null
    try {
        Assert-NoOfficeProcess -ProcessName "WINWORD" -ApplicationName "Word"
        $source = Find-OfficeTemplate -Patterns @("*.dotx", "*.dotm")
        $path = Join-Path $TempDir "CarbonPaper Word Probe.dotx"
        [IO.File]::Copy($source, $path, $true)
        Write-Verbose "Opening Word template copy $path"
        Write-Verbose "Creating Word automation instance"
        $application = New-Object -ComObject Word.Application
        Write-Verbose "Word automation instance created"
        $application.DisplayAlerts = 0
        $application.Visible = $true
        Write-Verbose "Opening Word document"
        $document = $application.Documents.Open($path, $false, $true)
        Write-Verbose "Word document opened"
        $window = $application.ActiveWindow
        $window.WindowState = 2
        $context = Wait-OfficeWindowContext -NativeClasses @("_WwG") -GetRootHwnd {
            [long]$window.Hwnd
        }
        return Invoke-OfficeResolution -Application "word" -WindowContext $context -ExpectedPath $path
    } finally {
        if ($null -ne $document) {
            try { $document.Close(0) } catch { Write-Verbose "Word document close failed: $($_.Exception.Message)" }
        }
        if ($null -ne $application) {
            try { $application.Quit() } catch { Write-Verbose "Word application quit failed: $($_.Exception.Message)" }
        }
        Release-ComObject $window
        Release-ComObject $document
        Release-ComObject $application
    }
}

function Test-ExcelNativeOm {
    param([Parameter(Mandatory = $true)][string]$TempDir)

    $application = $null
    $workbook = $null
    try {
        Assert-NoOfficeProcess -ProcessName "EXCEL" -ApplicationName "Excel"
        $source = Find-OfficeTemplate -Patterns @("*.xltx", "*.xltm")
        $path = Join-Path $TempDir "CarbonPaper Excel Probe.xltx"
        [IO.File]::Copy($source, $path, $true)
        Write-Verbose "Using Excel template source $source"
        Write-Verbose "Opening Excel template copy $path"
        Write-Verbose "Creating Excel automation instance"
        $application = New-Object -ComObject Excel.Application
        Write-Verbose "Excel automation instance created"
        $application.DisplayAlerts = $false
        $application.Visible = $true
        Write-Verbose "Opening Excel workbook"
        # The Editable argument makes Excel open an .xltx as the template
        # itself; without it Excel creates a new unsaved workbook.
        $missing = [Type]::Missing
        $workbook = $application.Workbooks.Open(
            $path,
            0,
            $false,
            $missing,
            $missing,
            $missing,
            $false,
            $missing,
            $missing,
            $true
        )
        Write-Verbose "Excel workbook opened"
        $application.WindowState = -4140
        $context = Wait-OfficeWindowContext -NativeClasses @("EXCEL7") -GetRootHwnd {
            [long]$application.Hwnd
        }
        return Invoke-OfficeResolution -Application "excel" -WindowContext $context -ExpectedPath $path
    } finally {
        if ($null -ne $workbook) {
            try { $workbook.Close($false) } catch {}
        }
        if ($null -ne $application) {
            try { $application.Quit() } catch {}
        }
        Release-ComObject $workbook
        Release-ComObject $application
    }
}

function Test-PowerPointNativeOm {
    param([Parameter(Mandatory = $true)][string]$TempDir)

    $application = $null
    $presentation = $null
    $window = $null
    try {
        Assert-NoOfficeProcess -ProcessName "POWERPNT" -ApplicationName "PowerPoint"
        $source = Find-OfficeTemplate -Patterns @("*.potx", "*.potm")
        $path = Join-Path $TempDir "CarbonPaper PowerPoint Probe.potx"
        [IO.File]::Copy($source, $path, $true)
        Write-Verbose "Using PowerPoint template source $source"
        Write-Verbose "Opening PowerPoint template copy $path"
        Write-Verbose "Creating PowerPoint automation instance"
        $application = New-Object -ComObject PowerPoint.Application
        Write-Verbose "PowerPoint automation instance created"
        $application.Visible = -1
        Write-Verbose "Opening PowerPoint presentation"
        $presentation = $application.Presentations.Open($path)
        Write-Verbose "PowerPoint presentation opened"
        $window = $application.ActiveWindow
        $powerPointProcess = Get-Process -Name "POWERPNT" -ErrorAction Stop |
            Select-Object -First 1
        $context = Wait-OfficeWindowContext -NativeClasses @("paneClassDC", "mdiClass") -GetRootHwnd {
            $powerPointProcess.Refresh()
            [long]$powerPointProcess.MainWindowHandle
        }
        return Invoke-OfficeResolution -Application "power_point" -WindowContext $context -ExpectedPath $path
    } finally {
        if ($null -ne $presentation) {
            try { $presentation.Close() } catch {}
        }
        if ($null -ne $application) {
            try { $application.Quit() } catch {}
        }
        Release-ComObject $window
        Release-ComObject $presentation
        Release-ComObject $application
    }
}

$tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$tempDir = Join-Path $tempRoot ("carbonpaper-office-nativeom-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tempDir | Out-Null

try {
    $results = foreach ($application in $Applications) {
        Write-Verbose "Beginning $application NativeOM probe"
        switch ($application) {
            "word" { Test-WordNativeOm -TempDir $tempDir }
            "excel" { Test-ExcelNativeOm -TempDir $tempDir }
            "power_point" { Test-PowerPointNativeOm -TempDir $tempDir }
        }
    }
    $results | ConvertTo-Json -Compress
} finally {
    [GC]::Collect()
    [GC]::WaitForPendingFinalizers()
    $resolvedTemp = [System.IO.Path]::GetFullPath($tempDir)
    if (
        (Test-Path -LiteralPath $resolvedTemp) -and
        $resolvedTemp.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)
    ) {
        Remove-Item -LiteralPath $resolvedTemp -Recurse -Force
    }
}
