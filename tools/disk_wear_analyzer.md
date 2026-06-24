# CarbonPaper Disk Wear Analyzer

`disk_wear_analyzer.py` is an external Windows diagnostic tool for measuring
CarbonPaper disk-write behavior and estimating SSD wear.

It combines:

- Process I/O deltas from `GetProcessIoCounters`.
- ETW file/disk events captured with `xperf`, or `wpr` as a fallback recorder.
- Per-file write attribution exported from ETL with `xperf -a dumper` or
  `tracerpt`.
- Whole-disk host-write deltas from `smartctl` or Windows Storage Reliability
  Counters.
- WAF/TBW-based wear estimates.

## Requirements

- Windows.
- An elevated terminal for ETW capture.
- Windows Performance Toolkit for best results:
  `xperf.exe` is the preferred recorder/exporter.
- Optional: `smartctl.exe` from smartmontools for better SMART/NVMe host-write
  counters.

`wpr.exe` and `tracerpt.exe` are usually present on Windows. The script starts
`DiskIO + FileIO` profiles when WPR is used, but `xperf` still gives better
automated file-path attribution.

## Preflight

```powershell
python tools\disk_wear_analyzer.py preflight
```

This checks:

- Administrator status.
- `xperf`, `wpr`, `tracerpt`, and `wpaexporter` availability.
- Current CarbonPaper process detection.
- SMART/storage write counter availability.

## Full Capture

Run CarbonPaper, then start an elevated PowerShell in this repository:

```powershell
python tools\disk_wear_analyzer.py run --duration-seconds 600 --tbw-tb 600
```

For a fixed workload command:

```powershell
python tools\disk_wear_analyzer.py run --tbw-tb 600 --workload-command "npm run debug"
```

Useful options:

- `--trace-backend auto|xperf|wpr|none`
- `--interval 2`
- `--data-dir C:\Users\<you>\AppData\Local\carbonpaper`
- `--waf 1.5,2.0,3.0`
- `--out-dir <path>`

## Baseline Comparison

For a stronger estimate, capture an idle baseline and then the workload. Start
CarbonPaper and leave it idle for the baseline phase, then run the workload phase:

```powershell
python tools\disk_wear_analyzer.py compare --duration-seconds 600 --tbw-tb 600
```

With an explicit workload command:

```powershell
python tools\disk_wear_analyzer.py compare --duration-seconds 600 --tbw-tb 600 --workload-command "npm run debug"
```

The comparison report subtracts the baseline write rate from the workload write
rate, then scales the result to the shorter phase duration. This helps separate
CarbonPaper workload writes from normal system/background writes.

Useful options:

- `--baseline-duration-seconds <seconds>`
- `--workload-duration-seconds <seconds>`
- `--pause-seconds 10`
- `--baseline-command <command>`
- `--workload-command <command>`

## Parse Existing ETL

```powershell
python tools\disk_wear_analyzer.py parse-etl path\to\trace.etl
```

This exports ETW events and tries to generate file-write attribution from the
existing trace.

## Output

Each run writes an output directory under `tools/disk-wear-runs/<timestamp>` by
default.

Important files:

- `disk_wear_report.md`: human-readable report.
- `comparison_report.md`: baseline-adjusted report from `compare`.
- `analysis_report.json`: complete machine-readable report.
- `comparison_report.json`: complete machine-readable comparison report.
- `process_samples.jsonl`: raw per-sample process counters.
- `process_io_summary.csv`: process/group write deltas.
- `file_write_attribution.csv`: ETW per-file write attribution.
- `carbonpaper_disk_trace.etl`: ETW trace, when capture succeeded.
- `xperf_dumper.csv` or `tracerpt_events.csv`: exported ETW events.
- `commands.jsonl`: recorder/export command results.

## Interpretation

Measured directly:

- CarbonPaper process logical write bytes.
- ETW-attributed file write bytes and paths.
- Whole-disk host-write deltas, when SMART/storage counters expose them.

Estimated:

- NAND writes after write amplification.
- TBW percentage consumed.

For a cleaner experiment:

- Close other write-heavy apps.
- Keep the capture window fixed.
- Run the same workload more than once.
- Compare idle baseline versus CarbonPaper workload.
- Prefer `xperf` over WPR-only capture when file paths matter.
