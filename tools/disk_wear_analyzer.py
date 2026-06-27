#!/usr/bin/env python3
"""
CarbonPaper disk write and wear analyzer.

This is an external Windows diagnostic tool. It combines four signals:

1. Process I/O deltas from GetProcessIoCounters.
2. ETW file/disk events captured with xperf, or WPR as a fallback recorder.
3. Per-file write attribution exported from ETL with xperf dumper or tracerpt.
4. SSD host-write counters from smartctl or Storage Reliability Counters.

The process/file write attribution can be measured. NAND wear is still an
estimate because SSD firmware hides write amplification, compression, SLC cache,
garbage collection, and wear leveling.
"""

from __future__ import annotations

import argparse
import csv
import ctypes
import datetime as dt
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


APP_EXE = "carbonpaper.exe"
NMH_EXE = "carbonpaper-nmh.exe"
PYTHON_EXES = {"python.exe", "pythonw.exe", "python3.exe"}
DEFAULT_INTERVAL_SECONDS = 2.0
DEFAULT_WAFS = (1.5, 2.0, 3.0)


def now_utc_iso() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="milliseconds")


def local_timestamp() -> str:
    return dt.datetime.now().strftime("%Y%m%d_%H%M%S")


def format_bytes(value: float | int | None) -> str:
    if value is None:
        return "n/a"
    value = float(value)
    units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"]
    idx = 0
    while abs(value) >= 1024.0 and idx < len(units) - 1:
        value /= 1024.0
        idx += 1
    if idx == 0:
        return f"{int(value)} {units[idx]}"
    return f"{value:.2f} {units[idx]}"


def bytes_to_gb(value: float | int | None) -> float | None:
    if value is None:
        return None
    return float(value) / (1000.0 ** 3)


def parse_int(value: Any) -> int | None:
    if value is None:
        return None
    if isinstance(value, bool):
        return int(value)
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return int(value)
    text = str(value).strip()
    if not text:
        return None
    text = text.replace(",", "")
    try:
        if text.lower().startswith("0x"):
            return int(text, 16)
        return int(float(text))
    except ValueError:
        return None


def normalize_key(value: str) -> str:
    return re.sub(r"[^a-z0-9]", "", value.lower())


def safe_rel(path: Path, root: Path) -> str:
    try:
        return str(path.resolve().relative_to(root.resolve()))
    except Exception:
        return str(path)


def ensure_windows() -> None:
    if os.name != "nt":
        raise SystemExit("This tool requires Windows because it uses ETW and Windows process I/O APIs.")


def is_admin() -> bool:
    if os.name != "nt":
        return False
    try:
        return bool(ctypes.windll.shell32.IsUserAnAdmin())
    except Exception:
        return False


def run_command(
    args: list[str],
    *,
    timeout: float | None = None,
    cwd: Path | None = None,
    allow_fail: bool = True,
) -> dict[str, Any]:
    started = time.time()
    try:
        completed = subprocess.run(
            args,
            cwd=str(cwd) if cwd else None,
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=timeout,
        )
        result = {
            "cmd": args,
            "returncode": completed.returncode,
            "stdout": completed.stdout,
            "stderr": completed.stderr,
            "elapsed_seconds": time.time() - started,
        }
    except Exception as exc:
        result = {
            "cmd": args,
            "returncode": None,
            "stdout": "",
            "stderr": repr(exc),
            "elapsed_seconds": time.time() - started,
        }
    if not allow_fail and result["returncode"] not in (0, None):
        raise RuntimeError(f"Command failed: {args}\n{result['stderr']}")
    return result


def append_jsonl(path: Path, obj: dict[str, Any]) -> None:
    with path.open("a", encoding="utf-8") as fh:
        fh.write(json.dumps(obj, ensure_ascii=True, sort_keys=True) + "\n")


def powershell_json(script: str, *, timeout: float = 30.0) -> tuple[Any | None, dict[str, Any]]:
    ps = shutil.which("powershell.exe") or shutil.which("pwsh.exe")
    if not ps:
        return None, {"cmd": ["powershell"], "returncode": None, "stdout": "", "stderr": "powershell not found"}
    prefix = (
        "$ErrorActionPreference='Stop'; "
        "[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new(); "
    )
    result = run_command(
        [
            ps,
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            prefix + script,
        ],
        timeout=timeout,
    )
    if result["returncode"] != 0 or not result["stdout"].strip():
        return None, result
    try:
        return json.loads(result["stdout"]), result
    except json.JSONDecodeError as exc:
        result["stderr"] = (result["stderr"] + "\n" + repr(exc)).strip()
        return None, result


def as_list(value: Any) -> list[Any]:
    if value is None:
        return []
    if isinstance(value, list):
        return value
    return [value]


@dataclass
class ProcessRecord:
    pid: int
    ppid: int
    name: str
    exe: str
    cmdline: str

    @property
    def basename(self) -> str:
        base = self.name or Path(self.exe).name
        return base.lower()


@dataclass
class IoCounters:
    read_ops: int
    write_ops: int
    other_ops: int
    read_bytes: int
    write_bytes: int
    other_bytes: int


if os.name == "nt":
    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _OpenProcess = _kernel32.OpenProcess
    _OpenProcess.argtypes = [ctypes.c_uint32, ctypes.c_int, ctypes.c_uint32]
    _OpenProcess.restype = ctypes.c_void_p
    _CloseHandle = _kernel32.CloseHandle
    _CloseHandle.argtypes = [ctypes.c_void_p]
    _CloseHandle.restype = ctypes.c_int
    _GetProcessIoCounters = _kernel32.GetProcessIoCounters
    _GetProcessIoCounters.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    _GetProcessIoCounters.restype = ctypes.c_int
    _QueryDosDeviceW = _kernel32.QueryDosDeviceW
    _QueryDosDeviceW.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_uint32]
    _QueryDosDeviceW.restype = ctypes.c_uint32
else:
    _kernel32 = None


class _IO_COUNTERS_STRUCT(ctypes.Structure):
    _fields_ = [
        ("ReadOperationCount", ctypes.c_ulonglong),
        ("WriteOperationCount", ctypes.c_ulonglong),
        ("OtherOperationCount", ctypes.c_ulonglong),
        ("ReadTransferCount", ctypes.c_ulonglong),
        ("WriteTransferCount", ctypes.c_ulonglong),
        ("OtherTransferCount", ctypes.c_ulonglong),
    ]


PROCESS_QUERY_LIMITED_INFORMATION = 0x1000


def get_process_io_counters(pid: int) -> IoCounters | None:
    if os.name != "nt":
        return None
    handle = _OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, int(pid))
    if not handle:
        return None
    try:
        counters = _IO_COUNTERS_STRUCT()
        ok = _GetProcessIoCounters(handle, ctypes.byref(counters))
        if not ok:
            return None
        return IoCounters(
            read_ops=int(counters.ReadOperationCount),
            write_ops=int(counters.WriteOperationCount),
            other_ops=int(counters.OtherOperationCount),
            read_bytes=int(counters.ReadTransferCount),
            write_bytes=int(counters.WriteTransferCount),
            other_bytes=int(counters.OtherTransferCount),
        )
    finally:
        _CloseHandle(handle)


def enumerate_processes() -> tuple[list[ProcessRecord], dict[str, Any]]:
    script = r"""
$items = Get-CimInstance Win32_Process |
  Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CommandLine
@($items) | ConvertTo-Json -Depth 4 -Compress
"""
    data, command_result = powershell_json(script, timeout=45.0)
    records: list[ProcessRecord] = []
    for item in as_list(data):
        if not isinstance(item, dict):
            continue
        pid = parse_int(item.get("ProcessId"))
        if pid is None:
            continue
        records.append(
            ProcessRecord(
                pid=pid,
                ppid=parse_int(item.get("ParentProcessId")) or 0,
                name=str(item.get("Name") or ""),
                exe=str(item.get("ExecutablePath") or ""),
                cmdline=str(item.get("CommandLine") or ""),
            )
        )
    return records, command_result


def children_map(records: Iterable[ProcessRecord]) -> dict[int, list[int]]:
    result: dict[int, list[int]] = defaultdict(list)
    for rec in records:
        result[rec.ppid].append(rec.pid)
    return result


def descendant_pids(start: set[int], cmap: dict[int, list[int]]) -> set[int]:
    seen: set[int] = set()
    stack = list(start)
    while stack:
        pid = stack.pop()
        for child in cmap.get(pid, []):
            if child in seen:
                continue
            seen.add(child)
            stack.append(child)
    return seen


def is_carbonpaper_exe(rec: ProcessRecord) -> bool:
    return rec.basename == APP_EXE or Path(rec.exe).name.lower() == APP_EXE


def is_nmh_exe(rec: ProcessRecord) -> bool:
    return rec.basename == NMH_EXE or Path(rec.exe).name.lower() == NMH_EXE


def is_python_process(rec: ProcessRecord) -> bool:
    return rec.basename in PYTHON_EXES or Path(rec.exe).name.lower() in PYTHON_EXES


def is_python_launcher(rec: ProcessRecord) -> bool:
    cmd = rec.cmdline.lower()
    return is_carbonpaper_exe(rec) and "--python-launcher" in cmd


def is_direct_monitor_python(rec: ProcessRecord) -> bool:
    cmd = rec.cmdline.lower().replace("/", "\\")
    return is_python_process(rec) and ("monitor.pyz" in cmd or "\\monitor\\main.py" in cmd)


def classify_processes(records: Iterable[ProcessRecord]) -> dict[int, dict[str, str]]:
    records = list(records)
    cmap = children_map(records)
    monitor_roots = {rec.pid for rec in records if is_python_launcher(rec) or is_direct_monitor_python(rec)}
    monitor_desc = descendant_pids(monitor_roots, cmap)
    groups: dict[int, dict[str, str]] = {}

    for rec in records:
        cmd = rec.cmdline.lower()
        group = ""
        role = ""
        if is_nmh_exe(rec):
            group = "rust_nmh"
            role = "native_messaging_host"
        elif rec.pid in monitor_roots:
            group = "python_monitor"
            role = "monitor_root"
        elif rec.pid in monitor_desc:
            if is_python_process(rec):
                group = "python_worker"
                role = "monitor_descendant_python"
            elif is_carbonpaper_exe(rec) and "--python-launcher" in cmd:
                group = "python_monitor"
                role = "nested_monitor_launcher"
            else:
                group = "python_child"
                role = "monitor_descendant"
        elif is_carbonpaper_exe(rec):
            if "--silent-install-python" in cmd:
                group = "rust_helper"
                role = "python_installer_helper"
            elif "--cng-unlock" in cmd:
                group = "rust_helper"
                role = "cng_unlock_helper"
            elif "--python-launcher" in cmd:
                group = "python_monitor"
                role = "monitor_root"
            else:
                group = "rust_main"
                role = "tauri_app"
        elif is_direct_monitor_python(rec):
            group = "python_monitor"
            role = "direct_monitor_python"

        if group:
            groups[rec.pid] = {"group": group, "role": role}
    return groups


class ProcessIoSampler:
    def __init__(self, out_dir: Path, interval: float) -> None:
        self.out_dir = out_dir
        self.interval = interval
        self.samples_path = out_dir / "process_samples.jsonl"
        self.commands_path = out_dir / "commands.jsonl"
        self.last_by_pid: dict[int, IoCounters] = {}
        self.meta_by_pid: dict[int, dict[str, Any]] = {}
        self.totals_by_pid: dict[int, dict[str, int]] = defaultdict(lambda: defaultdict(int))
        self.totals_by_group: dict[str, dict[str, int]] = defaultdict(lambda: defaultdict(int))
        self.pid_groups: dict[int, str] = {}
        self.sample_count = 0
        self.enumeration_failures = 0

    def sample_once(self) -> None:
        records, command_result = enumerate_processes()
        if command_result.get("returncode") != 0:
            self.enumeration_failures += 1
            append_jsonl(self.commands_path, {"kind": "process_enumeration", **command_result})
        groups = classify_processes(records)
        by_pid = {rec.pid: rec for rec in records}
        timestamp = now_utc_iso()
        for pid, info in groups.items():
            rec = by_pid.get(pid)
            if not rec:
                continue
            counters = get_process_io_counters(pid)
            self.pid_groups[pid] = info["group"]
            self.meta_by_pid[pid] = {
                "pid": pid,
                "ppid": rec.ppid,
                "name": rec.name,
                "exe": rec.exe,
                "cmdline": rec.cmdline,
                "group": info["group"],
                "role": info["role"],
            }
            row = {
                "timestamp": timestamp,
                **self.meta_by_pid[pid],
                "io_available": counters is not None,
            }
            if counters:
                row.update(
                    {
                        "read_bytes": counters.read_bytes,
                        "write_bytes": counters.write_bytes,
                        "other_bytes": counters.other_bytes,
                        "read_ops": counters.read_ops,
                        "write_ops": counters.write_ops,
                        "other_ops": counters.other_ops,
                    }
                )
                previous = self.last_by_pid.get(pid)
                if previous:
                    delta = {
                        "read_bytes": max(0, counters.read_bytes - previous.read_bytes),
                        "write_bytes": max(0, counters.write_bytes - previous.write_bytes),
                        "other_bytes": max(0, counters.other_bytes - previous.other_bytes),
                        "read_ops": max(0, counters.read_ops - previous.read_ops),
                        "write_ops": max(0, counters.write_ops - previous.write_ops),
                        "other_ops": max(0, counters.other_ops - previous.other_ops),
                    }
                    for key, value in delta.items():
                        self.totals_by_pid[pid][key] += value
                        self.totals_by_group[info["group"]][key] += value
                    row.update({f"delta_{key}": value for key, value in delta.items()})
                self.last_by_pid[pid] = counters
            append_jsonl(self.samples_path, row)
        self.sample_count += 1

    def write_summary(self) -> dict[str, Any]:
        csv_path = self.out_dir / "process_io_summary.csv"
        fields = [
            "group",
            "role",
            "pid",
            "name",
            "write_bytes",
            "write_ops",
            "read_bytes",
            "read_ops",
            "other_bytes",
            "other_ops",
            "exe",
            "cmdline",
        ]
        with csv_path.open("w", encoding="utf-8", newline="") as fh:
            writer = csv.DictWriter(fh, fieldnames=fields)
            writer.writeheader()
            for pid, totals in sorted(
                self.totals_by_pid.items(),
                key=lambda item: item[1].get("write_bytes", 0),
                reverse=True,
            ):
                meta = self.meta_by_pid.get(pid, {})
                writer.writerow(
                    {
                        "group": meta.get("group", ""),
                        "role": meta.get("role", ""),
                        "pid": pid,
                        "name": meta.get("name", ""),
                        "write_bytes": totals.get("write_bytes", 0),
                        "write_ops": totals.get("write_ops", 0),
                        "read_bytes": totals.get("read_bytes", 0),
                        "read_ops": totals.get("read_ops", 0),
                        "other_bytes": totals.get("other_bytes", 0),
                        "other_ops": totals.get("other_ops", 0),
                        "exe": meta.get("exe", ""),
                        "cmdline": meta.get("cmdline", ""),
                    }
                )
        return {
            "sample_count": self.sample_count,
            "enumeration_failures": self.enumeration_failures,
            "pid_groups": self.pid_groups,
            "totals_by_pid": {str(k): dict(v) for k, v in self.totals_by_pid.items()},
            "totals_by_group": {k: dict(v) for k, v in self.totals_by_group.items()},
            "meta_by_pid": {str(k): v for k, v in self.meta_by_pid.items()},
            "csv": str(csv_path),
        }


def resolve_tool(name: str, override: str | None = None) -> str | None:
    if override:
        path = Path(override)
        if path.is_file():
            return str(path)
        found = shutil.which(override)
        if found:
            return found
    found = shutil.which(name)
    if found:
        return found
    candidates = []
    if name.lower() in {"xperf.exe", "wpaexporter.exe"}:
        for root in (
            Path("C:/Program Files (x86)/Windows Kits/10/Windows Performance Toolkit"),
            Path("C:/Program Files/Windows Kits/10/Windows Performance Toolkit"),
        ):
            candidates.append(root / name)
    if name.lower() == "wpr.exe":
        system_root = Path(os.environ.get("SystemRoot", "C:/Windows"))
        candidates.append(system_root / "System32" / "wpr.exe")
    if name.lower() == "tracerpt.exe":
        system_root = Path(os.environ.get("SystemRoot", "C:/Windows"))
        candidates.append(system_root / "System32" / "tracerpt.exe")
    for candidate in candidates:
        if candidate.is_file():
            return str(candidate)
    return None


class TraceSession:
    def __init__(self, out_dir: Path, backend: str, xperf: str | None, wpr: str | None) -> None:
        self.out_dir = out_dir
        self.backend = backend
        self.xperf = xperf
        self.wpr = wpr
        self.started_backend: str | None = None
        self.etl_path = out_dir / "carbonpaper_disk_trace.etl"
        self.xperf_raw_path = out_dir / "carbonpaper_disk_trace_raw.etl"
        self.commands_path = out_dir / "commands.jsonl"
        self.start_result: dict[str, Any] | None = None
        self.stop_result: dict[str, Any] | None = None

    def start(self) -> dict[str, Any]:
        if self.backend == "none":
            return {"started": False, "backend": "none", "reason": "trace backend disabled"}
        if self.backend in {"auto", "xperf"} and self.xperf:
            result = self._start_xperf()
            append_jsonl(self.commands_path, {"kind": "trace_start", "backend": "xperf", **result})
            if result.get("returncode") == 0:
                self.started_backend = "xperf"
                self.start_result = result
                return {"started": True, "backend": "xperf", "etl": str(self.etl_path)}
            if self.backend == "xperf":
                self.start_result = result
                return {"started": False, "backend": "xperf", "error": result.get("stderr") or result.get("stdout")}

        if self.backend in {"auto", "wpr"} and self.wpr:
            result = self._start_wpr()
            append_jsonl(self.commands_path, {"kind": "trace_start", "backend": "wpr", **result})
            if result.get("returncode") == 0:
                self.started_backend = "wpr"
                self.start_result = result
                return {"started": True, "backend": "wpr", "etl": str(self.etl_path)}
            self.start_result = result
            return {"started": False, "backend": "wpr", "error": result.get("stderr") or result.get("stdout")}

        return {
            "started": False,
            "backend": self.backend,
            "error": "No usable trace backend found. Install Windows Performance Toolkit for xperf, or use wpr.",
        }

    def _start_xperf(self) -> dict[str, Any]:
        assert self.xperf
        flags = "PROC_THREAD+LOADER+FILE_IO+FILE_IO_INIT+DISK_IO+DISK_IO_INIT"
        stackwalk = "FileCreate+FileWrite+FileFlush+DiskWriteInit"
        return run_command(
            [
                self.xperf,
                "-on",
                flags,
                "-stackwalk",
                stackwalk,
                "-buffersize",
                "1024",
                "-minbuffers",
                "256",
                "-maxbuffers",
                "1024",
                "-f",
                str(self.xperf_raw_path),
            ],
            timeout=30.0,
        )

    def _start_wpr(self) -> dict[str, Any]:
        assert self.wpr
        profiles = [
            ["DiskIO", "FileIO"],
            ["FileIO"],
            ["DiskIO"],
            ["GeneralProfile"],
        ]
        last: dict[str, Any] | None = None
        for profile in profiles:
            start_args = [self.wpr]
            for item in profile:
                start_args.extend(["-start", item])
            start_args.append("-filemode")
            result = run_command(start_args, timeout=45.0)
            if result.get("returncode") == 0:
                result["selected_profile"] = profile
                return result
            last = result
        return last or {"cmd": [self.wpr], "returncode": 1, "stdout": "", "stderr": "no WPR profile worked"}

    def stop(self) -> dict[str, Any]:
        if not self.started_backend:
            return {"stopped": False, "reason": "trace was not started"}
        if self.started_backend == "xperf":
            assert self.xperf
            result = run_command([self.xperf, "-d", str(self.etl_path)], timeout=180.0)
        elif self.started_backend == "wpr":
            assert self.wpr
            result = run_command([self.wpr, "-stop", str(self.etl_path)], timeout=180.0)
        else:
            result = {"cmd": [], "returncode": None, "stdout": "", "stderr": "unknown backend"}
        append_jsonl(self.commands_path, {"kind": "trace_stop", "backend": self.started_backend, **result})
        self.stop_result = result
        return {
            "stopped": result.get("returncode") == 0,
            "backend": self.started_backend,
            "etl": str(self.etl_path),
            "returncode": result.get("returncode"),
            "stdout": result.get("stdout", ""),
            "stderr": result.get("stderr", ""),
        }


def build_device_path_map() -> dict[str, str]:
    mapping: dict[str, str] = {}
    if os.name != "nt":
        return mapping
    for letter_ord in range(ord("A"), ord("Z") + 1):
        drive = f"{chr(letter_ord)}:"
        buffer = ctypes.create_unicode_buffer(4096)
        length = _QueryDosDeviceW(drive, buffer, len(buffer))
        if length:
            device = buffer.value
            if device:
                mapping[device.lower()] = drive
    return mapping


def normalize_event_path(path: str, device_map: dict[str, str]) -> str:
    text = (path or "").strip().strip('"')
    if not text:
        return text
    if text.startswith("\\??\\"):
        text = text[4:]
    if text.startswith("\\\\?\\"):
        text = text[4:]
    lower = text.lower()
    for device, drive in sorted(device_map.items(), key=lambda item: len(item[0]), reverse=True):
        if lower.startswith(device):
            return drive + text[len(device) :]
    return text


def sniff_csv(path: Path) -> tuple[list[str], csv.Dialect]:
    with path.open("r", encoding="utf-8-sig", errors="replace", newline="") as fh:
        sample = fh.read(8192)
        fh.seek(0)
        try:
            dialect = csv.Sniffer().sniff(sample, delimiters=",\t;")
        except csv.Error:
            dialect = csv.excel
        reader = csv.reader(fh, dialect)
        for row in reader:
            if row and any(cell.strip() for cell in row):
                return row, dialect
    return [], csv.excel


def find_column(columns: list[str], candidates: Iterable[str]) -> str | None:
    normalized = {normalize_key(col): col for col in columns}
    for candidate in candidates:
        key = normalize_key(candidate)
        if key in normalized:
            return normalized[key]
    for col in columns:
        col_key = normalize_key(col)
        for candidate in candidates:
            cand_key = normalize_key(candidate)
            if cand_key and cand_key in col_key:
                return col
    return None


def open_csv_dicts(path: Path):
    header, dialect = sniff_csv(path)
    fh = path.open("r", encoding="utf-8-sig", errors="replace", newline="")
    reader = csv.DictReader(fh, dialect=dialect)
    return fh, reader, header


def row_value(row: dict[str, Any], column: str | None) -> str:
    if not column:
        return ""
    value = row.get(column)
    return "" if value is None else str(value)


def is_write_row(row: dict[str, Any], event_cols: list[str]) -> bool:
    text = " ".join(row_value(row, col) for col in event_cols).lower()
    if "write" in text or "flush" in text:
        return True
    return False


def path_category(path: str, project_root: Path, data_dir: Path | None) -> str:
    lower = path.lower().replace("/", "\\")
    roots: list[tuple[str, str]] = []
    roots.append((str(project_root).lower().replace("/", "\\"), "project"))
    if data_dir:
        roots.append((str(data_dir).lower().replace("/", "\\"), "app_data"))
    for root, category in roots:
        if root and lower.startswith(root):
            if "\\screenshots\\" in lower:
                return f"{category}:screenshots"
            if "\\chroma_db\\" in lower or "chroma.sqlite" in lower:
                return f"{category}:chroma"
            if lower.endswith((".db", ".sqlite", ".sqlite3", ".db-wal", ".db-shm")):
                return f"{category}:database"
            if "\\logs\\" in lower or lower.endswith(".log"):
                return f"{category}:logs"
            if "\\monitor\\" in lower:
                return f"{category}:monitor"
            return category
    if "\\temp\\" in lower or "\\tmp\\" in lower:
        return "temp"
    if "\\pip" in lower or "\\site-packages\\" in lower:
        return "python_env"
    if "huggingface" in lower or "\\torch\\" in lower or "\\onnxruntime" in lower:
        return "model_cache"
    if lower.endswith((".db", ".sqlite", ".sqlite3", ".db-wal", ".db-shm")):
        return "database"
    if lower.endswith((".log", ".jsonl")):
        return "logs"
    return "other"


def parse_process_records_from_event_csv(path: Path) -> list[ProcessRecord]:
    records_by_pid: dict[int, ProcessRecord] = {}
    try:
        fh, reader, header = open_csv_dicts(path)
    except Exception:
        return []
    with fh:
        if not header:
            return []
        pid_col = find_column(header, ["PID", "ProcessID", "Process Id"])
        ppid_col = find_column(header, ["ParentID", "Parent Process ID", "ParentProcessId"])
        name_col = find_column(header, ["Process Name", "ProcessName", "ImageFileName", "Image Name"])
        exe_col = find_column(header, ["ExecutablePath", "Image Path", "ImagePath", "Path"])
        cmd_col = find_column(header, ["CommandLine", "Command Line"])
        if not pid_col:
            return []
        for row in reader:
            pid = parse_int(row.get(pid_col))
            if pid is None or pid in records_by_pid:
                continue
            name = row_value(row, name_col)
            exe = row_value(row, exe_col)
            cmdline = row_value(row, cmd_col)
            if not (name or exe or cmdline):
                continue
            records_by_pid[pid] = ProcessRecord(
                pid=pid,
                ppid=parse_int(row.get(ppid_col)) or 0,
                name=name,
                exe=exe,
                cmdline=cmdline,
            )
    return list(records_by_pid.values())


def parse_event_csv(
    path: Path,
    *,
    known_pid_groups: dict[int, str],
    project_root: Path,
    data_dir: Path | None,
) -> dict[str, Any]:
    device_map = build_device_path_map()
    etw_records = parse_process_records_from_event_csv(path)
    etw_groups = {pid: info["group"] for pid, info in classify_processes(etw_records).items()}
    pid_groups = dict(etw_groups)
    pid_groups.update(known_pid_groups)

    try:
        fh, reader, header = open_csv_dicts(path)
    except Exception as exc:
        return {"parsed": False, "error": repr(exc), "source": str(path)}

    file_totals: dict[tuple[str, str], dict[str, Any]] = {}
    category_totals: dict[str, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    group_totals: dict[str, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    pid_totals: dict[int, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    rows_seen = 0
    rows_used = 0
    unknown_carbonpaper_rows = 0

    with fh:
        if not header:
            return {"parsed": False, "error": "CSV header not found", "source": str(path)}
        pid_col = find_column(header, ["PID", "ProcessID", "Process Id"])
        process_col = find_column(header, ["Process Name", "ProcessName", "ImageFileName", "Image Name"])
        path_col = find_column(header, ["FileName", "File Name", "FilePath", "File Path", "Path"])
        size_col = find_column(header, ["IoSize", "IO Size", "TransferSize", "Transfer Size", "Size", "ByteCount", "Bytes", "Length"])
        event_col = find_column(header, ["Event Name", "EventName", "Event", "Task Name", "TaskName"])
        opcode_col = find_column(header, ["Opcode", "Opcode Name", "Operation", "Event Type", "Type"])
        event_cols = [col for col in (event_col, opcode_col) if col]
        if not path_col or not size_col:
            return {
                "parsed": False,
                "error": "Required FileName/IoSize-like columns not found",
                "source": str(path),
                "columns": header,
            }

        for row in reader:
            rows_seen += 1
            if event_cols and not is_write_row(row, event_cols):
                continue
            size = parse_int(row.get(size_col))
            if not size or size <= 0:
                continue
            pid = parse_int(row.get(pid_col))
            process_name = row_value(row, process_col)
            group = pid_groups.get(pid or -1, "")
            if not group and process_name.lower() in {APP_EXE, NMH_EXE}:
                group = "carbonpaper_unknown"
                unknown_carbonpaper_rows += 1
            if not group:
                continue
            file_path = normalize_event_path(row_value(row, path_col), device_map)
            if not file_path:
                continue
            category = path_category(file_path, project_root, data_dir)
            key = (group, file_path)
            item = file_totals.setdefault(
                key,
                {
                    "group": group,
                    "path": file_path,
                    "category": category,
                    "bytes": 0,
                    "events": 0,
                    "pids": set(),
                    "process_names": set(),
                },
            )
            item["bytes"] += size
            item["events"] += 1
            if pid is not None:
                item["pids"].add(pid)
                pid_totals[pid]["bytes"] += size
                pid_totals[pid]["events"] += 1
            if process_name:
                item["process_names"].add(process_name)
            category_totals[category]["bytes"] += size
            category_totals[category]["events"] += 1
            group_totals[group]["bytes"] += size
            group_totals[group]["events"] += 1
            rows_used += 1

    file_rows = []
    for item in file_totals.values():
        row = dict(item)
        row["pids"] = sorted(row["pids"])
        row["process_names"] = sorted(row["process_names"])
        file_rows.append(row)
    file_rows.sort(key=lambda item: item["bytes"], reverse=True)

    return {
        "parsed": True,
        "source": str(path),
        "rows_seen": rows_seen,
        "rows_used": rows_used,
        "unknown_carbonpaper_rows": unknown_carbonpaper_rows,
        "pid_groups": {str(k): v for k, v in pid_groups.items()},
        "group_totals": {k: dict(v) for k, v in group_totals.items()},
        "category_totals": {k: dict(v) for k, v in category_totals.items()},
        "pid_totals": {str(k): dict(v) for k, v in pid_totals.items()},
        "file_totals": file_rows,
    }


def export_etl(
    etl_path: Path,
    out_dir: Path,
    *,
    xperf: str | None,
    tracerpt: str | None,
) -> dict[str, Any]:
    exports: dict[str, Any] = {"commands": [], "files": {}}
    if not etl_path.is_file():
        exports["error"] = f"ETL not found: {etl_path}"
        return exports
    if xperf:
        dumper_csv = out_dir / "xperf_dumper.csv"
        fileio_txt = out_dir / "xperf_fileio.txt"
        diskio_txt = out_dir / "xperf_diskio.txt"
        commands = [
            [xperf, "-i", str(etl_path), "-o", str(dumper_csv), "-a", "dumper"],
            [xperf, "-i", str(etl_path), "-o", str(fileio_txt), "-a", "fileio"],
            [xperf, "-i", str(etl_path), "-o", str(diskio_txt), "-a", "diskio"],
        ]
        for cmd in commands:
            result = run_command(cmd, timeout=300.0)
            exports["commands"].append(result)
        if dumper_csv.is_file():
            exports["files"]["xperf_dumper_csv"] = str(dumper_csv)
        if fileio_txt.is_file():
            exports["files"]["xperf_fileio_txt"] = str(fileio_txt)
        if diskio_txt.is_file():
            exports["files"]["xperf_diskio_txt"] = str(diskio_txt)
    if tracerpt:
        tracerpt_csv = out_dir / "tracerpt_events.csv"
        result = run_command([tracerpt, str(etl_path), "-of", "CSV", "-o", str(tracerpt_csv), "-y"], timeout=300.0)
        exports["commands"].append(result)
        if tracerpt_csv.is_file():
            exports["files"]["tracerpt_csv"] = str(tracerpt_csv)
    return exports


def write_file_attribution_csv(attribution: dict[str, Any], out_dir: Path, max_rows: int | None = None) -> str | None:
    if not attribution.get("parsed"):
        return None
    path = out_dir / "file_write_attribution.csv"
    rows = attribution.get("file_totals", [])
    if max_rows:
        rows = rows[:max_rows]
    fields = ["group", "category", "bytes", "events", "pids", "process_names", "path"]
    with path.open("w", encoding="utf-8", newline="") as fh:
        writer = csv.DictWriter(fh, fieldnames=fields)
        writer.writeheader()
        for row in rows:
            writer.writerow(
                {
                    "group": row.get("group", ""),
                    "category": row.get("category", ""),
                    "bytes": row.get("bytes", 0),
                    "events": row.get("events", 0),
                    "pids": " ".join(str(pid) for pid in row.get("pids", [])),
                    "process_names": " ".join(row.get("process_names", [])),
                    "path": row.get("path", ""),
                }
            )
    return str(path)


def snapshot_smartctl() -> dict[str, Any] | None:
    smartctl = shutil.which("smartctl.exe") or shutil.which("smartctl")
    if not smartctl:
        return None
    scan = run_command([smartctl, "--scan-open", "-j"], timeout=30.0)
    if scan.get("returncode") != 0 or not scan.get("stdout", "").strip():
        return {"provider": "smartctl", "available": False, "scan": scan, "disks": []}
    try:
        scan_data = json.loads(scan["stdout"])
    except json.JSONDecodeError:
        return {"provider": "smartctl", "available": False, "scan": scan, "disks": []}
    disks = []
    for device in scan_data.get("devices", []):
        name = device.get("name")
        if not name:
            continue
        detail = run_command([smartctl, "-a", "-j", name], timeout=45.0)
        disk: dict[str, Any] = {
            "key": name,
            "device": name,
            "bytes_written": None,
            "source": "smartctl",
            "raw": None,
        }
        if detail.get("returncode") == 0 and detail.get("stdout", "").strip():
            try:
                data = json.loads(detail["stdout"])
                disk["raw"] = data
                disk["model"] = data.get("model_name") or data.get("device", {}).get("name")
                disk["serial"] = data.get("serial_number")
                disk["key"] = disk.get("serial") or name
                nvme = data.get("nvme_smart_health_information_log") or {}
                duw = parse_int(nvme.get("data_units_written"))
                if duw is not None:
                    disk["bytes_written"] = duw * 512000
                    disk["bytes_written_source"] = "nvme_data_units_written"
                attrs = data.get("ata_smart_attributes", {}).get("table", [])
                for attr in attrs:
                    attr_name = str(attr.get("name") or "").lower()
                    raw_value = attr.get("raw", {}).get("value")
                    raw_int = parse_int(raw_value)
                    if raw_int is None:
                        continue
                    if "total_lbas_written" in attr_name:
                        disk["bytes_written"] = raw_int * 512
                        disk["bytes_written_source"] = "ata_total_lbas_written"
                    elif "host_writes_32mib" in attr_name and disk["bytes_written"] is None:
                        disk["bytes_written"] = raw_int * 32 * 1024 * 1024
                        disk["bytes_written_source"] = "ata_host_writes_32mib"
                    elif "nand_writes_1gib" in attr_name and disk.get("nand_bytes_written") is None:
                        disk["nand_bytes_written"] = raw_int * 1024 * 1024 * 1024
                disks.append(disk)
            except json.JSONDecodeError:
                disk["detail_error"] = "smartctl JSON parse failed"
                disks.append(disk)
        else:
            disk["detail_error"] = detail.get("stderr", "")
            disks.append(disk)
    return {"provider": "smartctl", "available": True, "scan": scan, "disks": disks}


def snapshot_storage_reliability() -> dict[str, Any] | None:
    script = r"""
$items = Get-PhysicalDisk | ForEach-Object {
  $disk = $_
  $counter = $null
  try { $counter = $disk | Get-StorageReliabilityCounter } catch {}
  [pscustomobject]@{
    FriendlyName = $disk.FriendlyName
    SerialNumber = $disk.SerialNumber
    UniqueId = $disk.UniqueId
    MediaType = [string]$disk.MediaType
    BusType = [string]$disk.BusType
    Size = $disk.Size
    BytesWritten = if ($counter) { $counter.BytesWritten } else { $null }
    Wear = if ($counter) { $counter.Wear } else { $null }
    Temperature = if ($counter) { $counter.Temperature } else { $null }
  }
}
@($items) | ConvertTo-Json -Depth 5 -Compress
"""
    data, result = powershell_json(script, timeout=45.0)
    if data is None:
        return {"provider": "storage_reliability", "available": False, "command": result, "disks": []}
    disks = []
    for item in as_list(data):
        if not isinstance(item, dict):
            continue
        key = item.get("SerialNumber") or item.get("UniqueId") or item.get("FriendlyName")
        disks.append(
            {
                "key": str(key or ""),
                "device": item.get("FriendlyName"),
                "serial": item.get("SerialNumber"),
                "unique_id": item.get("UniqueId"),
                "media_type": item.get("MediaType"),
                "bus_type": item.get("BusType"),
                "size": parse_int(item.get("Size")),
                "bytes_written": parse_int(item.get("BytesWritten")),
                "wear": parse_int(item.get("Wear")),
                "temperature": parse_int(item.get("Temperature")),
                "source": "storage_reliability",
            }
        )
    return {"provider": "storage_reliability", "available": True, "command": result, "disks": disks}


def smart_snapshot() -> dict[str, Any]:
    smartctl = snapshot_smartctl()
    storage = snapshot_storage_reliability()
    providers = [provider for provider in (smartctl, storage) if provider]
    selected_disks: dict[str, dict[str, Any]] = {}
    for provider in providers:
        for disk in provider.get("disks", []):
            key = str(disk.get("key") or disk.get("device") or "")
            if not key:
                continue
            current = selected_disks.get(key)
            if not current or (current.get("bytes_written") is None and disk.get("bytes_written") is not None):
                selected_disks[key] = disk
    return {
        "timestamp": now_utc_iso(),
        "providers": providers,
        "disks": list(selected_disks.values()),
    }


def smart_delta(before: dict[str, Any] | None, after: dict[str, Any] | None) -> dict[str, Any]:
    if not before or not after:
        return {"available": False, "disks": [], "total_delta_bytes": None}
    before_by_key = {str(d.get("key") or ""): d for d in before.get("disks", [])}
    disks = []
    total = 0
    any_delta = False
    for after_disk in after.get("disks", []):
        key = str(after_disk.get("key") or "")
        before_disk = before_by_key.get(key)
        before_bytes = parse_int(before_disk.get("bytes_written")) if before_disk else None
        after_bytes = parse_int(after_disk.get("bytes_written"))
        delta = None
        if before_bytes is not None and after_bytes is not None and after_bytes >= before_bytes:
            delta = after_bytes - before_bytes
            total += delta
            any_delta = True
        disks.append(
            {
                "key": key,
                "device": after_disk.get("device"),
                "serial": after_disk.get("serial"),
                "source": after_disk.get("source"),
                "before_bytes_written": before_bytes,
                "after_bytes_written": after_bytes,
                "delta_bytes": delta,
                "wear_before": before_disk.get("wear") if before_disk else None,
                "wear_after": after_disk.get("wear"),
            }
        )
    return {"available": any_delta, "disks": disks, "total_delta_bytes": total if any_delta else None}


def estimate_wear(
    *,
    logical_write_bytes: int | float | None,
    smart_host_delta_bytes: int | float | None,
    tbw_tb: float | None,
    wafs: Iterable[float],
) -> list[dict[str, Any]]:
    rows = []
    basis_values = []
    if logical_write_bytes is not None:
        basis_values.append(("carbonpaper_logical_writes", float(logical_write_bytes)))
    if smart_host_delta_bytes is not None:
        basis_values.append(("whole_disk_host_writes", float(smart_host_delta_bytes)))
    for basis_name, basis_bytes in basis_values:
        for waf in wafs:
            nand_bytes = basis_bytes * float(waf)
            row = {
                "basis": basis_name,
                "waf": float(waf),
                "basis_bytes": int(basis_bytes),
                "estimated_nand_bytes": int(nand_bytes),
                "tbw_used_percent": None,
            }
            if tbw_tb and tbw_tb > 0:
                tbw_bytes = tbw_tb * (1000.0 ** 4)
                row["tbw_used_percent"] = nand_bytes / tbw_bytes * 100.0
            rows.append(row)
    return rows


def sum_write_bytes_from_process_summary(summary: dict[str, Any]) -> int:
    total = 0
    for group in summary.get("totals_by_group", {}).values():
        total += int(group.get("write_bytes", 0))
    return total


def sum_write_bytes_from_attribution(attribution: dict[str, Any]) -> int | None:
    if not attribution.get("parsed"):
        return None
    total = 0
    for group in attribution.get("group_totals", {}).values():
        total += int(group.get("bytes", 0))
    return total


def choose_attribution(exports: dict[str, Any], known_pid_groups: dict[int, str], project_root: Path, data_dir: Path | None) -> dict[str, Any]:
    files = exports.get("files", {})
    candidates = []
    for key in ("xperf_dumper_csv", "tracerpt_csv"):
        value = files.get(key)
        if value:
            candidates.append(Path(value))
    errors = []
    for candidate in candidates:
        parsed = parse_event_csv(
            candidate,
            known_pid_groups=known_pid_groups,
            project_root=project_root,
            data_dir=data_dir,
        )
        if parsed.get("parsed"):
            return parsed
        errors.append(parsed)
    return {"parsed": False, "errors": errors, "source_candidates": [str(c) for c in candidates]}


def write_report(report: dict[str, Any], out_dir: Path, *, max_file_rows: int) -> Path:
    path = out_dir / "disk_wear_report.md"
    process_summary = report.get("process_io", {})
    attribution = report.get("file_attribution", {})
    smart = report.get("smart_delta", {})
    estimates = report.get("wear_estimates", [])

    lines: list[str] = []
    lines.append("# CarbonPaper Disk Wear Report")
    lines.append("")
    lines.append(f"- Started: `{report.get('started_at')}`")
    lines.append(f"- Finished: `{report.get('finished_at')}`")
    lines.append(f"- Duration: `{report.get('duration_seconds'):.1f}s`")
    lines.append(f"- Trace backend: `{report.get('trace', {}).get('backend')}`")
    lines.append(f"- Output directory: `{out_dir}`")
    lines.append("")
    lines.append("## Confidence")
    lines.append("")
    confidence = []
    if attribution.get("parsed"):
        confidence.append("ETW file-write attribution was parsed successfully.")
    else:
        confidence.append("ETW file-write attribution was not parsed; inspect exported ETL/CSV artifacts.")
    if smart.get("available"):
        confidence.append("Whole-disk host-write delta was captured from SMART/storage counters.")
    else:
        confidence.append("Whole-disk host-write delta was unavailable.")
    if process_summary.get("sample_count", 0) >= 2:
        confidence.append("Process I/O deltas were sampled.")
    else:
        confidence.append("Process I/O sampling has too few samples for deltas.")
    for item in confidence:
        lines.append(f"- {item}")
    lines.append("- SSD wear remains an estimate; firmware-level NAND writes are not process-attributable.")
    lines.append("")

    lines.append("## Process I/O Totals")
    lines.append("")
    group_totals = process_summary.get("totals_by_group", {})
    if group_totals:
        lines.append("| Group | Write | Write ops | Read |")
        lines.append("| --- | ---: | ---: | ---: |")
        for group, values in sorted(group_totals.items(), key=lambda item: item[1].get("write_bytes", 0), reverse=True):
            lines.append(
                f"| `{group}` | {format_bytes(values.get('write_bytes', 0))} | "
                f"{values.get('write_ops', 0)} | {format_bytes(values.get('read_bytes', 0))} |"
            )
    else:
        lines.append("No CarbonPaper process I/O deltas were captured.")
    lines.append("")

    lines.append("## ETW File Write Attribution")
    lines.append("")
    if attribution.get("parsed"):
        lines.append(f"- ETW rows seen: `{attribution.get('rows_seen')}`")
        lines.append(f"- ETW write rows attributed to CarbonPaper: `{attribution.get('rows_used')}`")
        lines.append("")
        lines.append("### By Group")
        lines.append("")
        lines.append("| Group | Write | Events |")
        lines.append("| --- | ---: | ---: |")
        for group, values in sorted(attribution.get("group_totals", {}).items(), key=lambda item: item[1].get("bytes", 0), reverse=True):
            lines.append(f"| `{group}` | {format_bytes(values.get('bytes', 0))} | {values.get('events', 0)} |")
        lines.append("")
        lines.append("### By Category")
        lines.append("")
        lines.append("| Category | Write | Events |")
        lines.append("| --- | ---: | ---: |")
        for category, values in sorted(attribution.get("category_totals", {}).items(), key=lambda item: item[1].get("bytes", 0), reverse=True):
            lines.append(f"| `{category}` | {format_bytes(values.get('bytes', 0))} | {values.get('events', 0)} |")
        lines.append("")
        lines.append(f"### Top {max_file_rows} Files")
        lines.append("")
        lines.append("| Write | Events | Group | Category | Path |")
        lines.append("| ---: | ---: | --- | --- | --- |")
        for row in attribution.get("file_totals", [])[:max_file_rows]:
            path_text = str(row.get("path", "")).replace("|", "\\|")
            lines.append(
                f"| {format_bytes(row.get('bytes', 0))} | {row.get('events', 0)} | "
                f"`{row.get('group')}` | `{row.get('category')}` | `{path_text}` |"
            )
    else:
        lines.append("No parsed ETW file attribution is available.")
        if attribution.get("errors"):
            lines.append("")
            lines.append("Parser errors:")
            for error in attribution.get("errors", []):
                lines.append(f"- `{error.get('source')}`: {error.get('error')}")
    lines.append("")

    lines.append("## Whole-Disk Host Writes")
    lines.append("")
    if smart.get("available"):
        lines.append("| Disk | Source | Delta | Wear before | Wear after |")
        lines.append("| --- | --- | ---: | ---: | ---: |")
        for disk in smart.get("disks", []):
            lines.append(
                f"| `{disk.get('device') or disk.get('key')}` | `{disk.get('source')}` | "
                f"{format_bytes(disk.get('delta_bytes'))} | {disk.get('wear_before')} | {disk.get('wear_after')} |"
            )
        lines.append(f"\nTotal whole-disk host-write delta: **{format_bytes(smart.get('total_delta_bytes'))}**")
    else:
        lines.append("SMART/storage host-write delta was unavailable.")
    lines.append("")

    lines.append("## Wear Estimates")
    lines.append("")
    if estimates:
        lines.append("| Basis | WAF | Basis write | Estimated NAND write | TBW used |")
        lines.append("| --- | ---: | ---: | ---: | ---: |")
        for row in estimates:
            pct = row.get("tbw_used_percent")
            pct_text = "n/a" if pct is None else f"{pct:.6f}%"
            lines.append(
                f"| `{row.get('basis')}` | {row.get('waf')} | {format_bytes(row.get('basis_bytes'))} | "
                f"{format_bytes(row.get('estimated_nand_bytes'))} | {pct_text} |"
            )
    else:
        lines.append("No wear estimates were generated.")
    lines.append("")

    lines.append("## Artifacts")
    lines.append("")
    for artifact in (
        "process_samples.jsonl",
        "process_io_summary.csv",
        "file_write_attribution.csv",
        "analysis_report.json",
        "commands.jsonl",
    ):
        artifact_path = out_dir / artifact
        if artifact_path.exists():
            lines.append(f"- `{artifact}`")
    etl = report.get("trace", {}).get("etl")
    if etl:
        lines.append(f"- `{Path(etl).name}`")
    lines.append("")
    path.write_text("\n".join(lines), encoding="utf-8")
    return path


def run_workload(command: str | None, cwd: Path | None) -> subprocess.Popen | None:
    if not command:
        return None
    return subprocess.Popen(command, cwd=str(cwd) if cwd else None, shell=True)


def parse_wafs(text: str | None) -> tuple[float, ...]:
    if not text:
        return DEFAULT_WAFS
    values = []
    for part in text.split(","):
        part = part.strip()
        if not part:
            continue
        values.append(float(part))
    return tuple(values) or DEFAULT_WAFS


def serializable_args(args: argparse.Namespace) -> dict[str, Any]:
    result = {}
    for key, value in vars(args).items():
        if key == "func" or callable(value):
            continue
        result[key] = value
    return result


def default_data_dir() -> Path | None:
    local_appdata = os.environ.get("LOCALAPPDATA")
    if not local_appdata:
        return None
    return Path(local_appdata) / "carbonpaper"


def preflight(args: argparse.Namespace) -> int:
    ensure_windows()
    xperf = resolve_tool("xperf.exe", args.xperf)
    wpr = resolve_tool("wpr.exe", args.wpr)
    tracerpt = resolve_tool("tracerpt.exe", None)
    wpaexporter = resolve_tool("wpaexporter.exe", None)
    records, command_result = enumerate_processes()
    groups = classify_processes(records)
    smart = smart_snapshot()

    print("CarbonPaper disk wear analyzer preflight")
    print(f"  admin: {is_admin()}")
    print(f"  xperf: {xperf or 'not found'}")
    print(f"  wpr: {wpr or 'not found'}")
    print(f"  tracerpt: {tracerpt or 'not found'}")
    print(f"  wpaexporter: {wpaexporter or 'not found'}")
    print(f"  process enumeration: {'ok' if command_result.get('returncode') == 0 else 'failed'}")
    print(f"  matching CarbonPaper processes: {len(groups)}")
    for rec in records:
        if rec.pid in groups:
            info = groups[rec.pid]
            print(f"    pid={rec.pid} ppid={rec.ppid} group={info['group']} role={info['role']} name={rec.name}")
    smart_disks = smart.get("disks", [])
    writable = [disk for disk in smart_disks if disk.get("bytes_written") is not None]
    print(f"  SMART/storage disks: {len(smart_disks)} ({len(writable)} with bytes_written)")
    if not is_admin():
        print("  warning: ETW tracing normally requires an elevated terminal.")
    if not xperf and not wpr:
        print("  warning: install Windows Performance Toolkit for xperf, or use WPR if available.")
    return 0


def run_capture(args: argparse.Namespace) -> int:
    ensure_windows()
    project_root = Path(args.project_root).resolve()
    data_dir = Path(args.data_dir).resolve() if args.data_dir else default_data_dir()
    out_dir = Path(args.out_dir) if args.out_dir else project_root / "tools" / "disk-wear-runs" / local_timestamp()
    out_dir.mkdir(parents=True, exist_ok=True)
    xperf = resolve_tool("xperf.exe", args.xperf)
    wpr = resolve_tool("wpr.exe", args.wpr)
    tracerpt = resolve_tool("tracerpt.exe", None)

    started_at = time.time()
    report: dict[str, Any] = {
        "started_at": now_utc_iso(),
        "project_root": str(project_root),
        "data_dir": str(data_dir) if data_dir else None,
        "admin": is_admin(),
        "tools": {"xperf": xperf, "wpr": wpr, "tracerpt": tracerpt},
        "args": serializable_args(args),
    }
    (out_dir / "run_config.json").write_text(json.dumps(report, ensure_ascii=True, indent=2), encoding="utf-8")

    print(f"Output directory: {out_dir}")
    if args.trace_backend != "none" and not is_admin():
        print("Warning: ETW tracing usually requires an elevated terminal. Process sampling will still run.")

    print("Taking SMART/storage snapshot before workload...")
    smart_before = smart_snapshot()

    trace = TraceSession(out_dir, args.trace_backend, xperf, wpr)
    trace_start = trace.start()
    print(f"Trace start: {trace_start}")

    sampler = ProcessIoSampler(out_dir, args.interval)
    workload = run_workload(args.workload_command, project_root)
    stop_requested = False

    def _handle_signal(signum, frame):  # noqa: ARG001
        nonlocal stop_requested
        stop_requested = True
        print("Stop requested; finalizing trace and report...")

    previous_sigint = signal.getsignal(signal.SIGINT)
    signal.signal(signal.SIGINT, _handle_signal)
    try:
        deadline = time.time() + args.duration_seconds if args.duration_seconds else None
        while True:
            sampler.sample_once()
            if stop_requested:
                break
            if deadline and time.time() >= deadline:
                break
            if workload and workload.poll() is not None and not deadline:
                break
            sleep_for = max(0.1, min(args.interval, (deadline - time.time()) if deadline else args.interval))
            time.sleep(sleep_for)
    finally:
        signal.signal(signal.SIGINT, previous_sigint)
        if workload and workload.poll() is None and args.terminate_workload_on_stop:
            workload.terminate()
        trace_stop = trace.stop()

    print("Taking SMART/storage snapshot after workload...")
    smart_after = smart_snapshot()
    smart_diff = smart_delta(smart_before, smart_after)
    process_summary = sampler.write_summary()

    exports = {}
    attribution: dict[str, Any] = {"parsed": False, "reason": "trace not available"}
    if trace.etl_path.is_file():
        print("Exporting ETL for file attribution...")
        exports = export_etl(trace.etl_path, out_dir, xperf=xperf, tracerpt=tracerpt)
        attribution = choose_attribution(exports, sampler.pid_groups, project_root, data_dir)
        attr_csv = write_file_attribution_csv(attribution, out_dir, max_rows=None)
        if attr_csv:
            attribution["csv"] = attr_csv

    process_logical = sum_write_bytes_from_process_summary(process_summary)
    etw_logical = sum_write_bytes_from_attribution(attribution)
    logical_for_estimate = etw_logical if etw_logical is not None else process_logical
    wafs = parse_wafs(args.waf)
    estimates = estimate_wear(
        logical_write_bytes=logical_for_estimate,
        smart_host_delta_bytes=smart_diff.get("total_delta_bytes"),
        tbw_tb=args.tbw_tb,
        wafs=wafs,
    )

    report.update(
        {
            "finished_at": now_utc_iso(),
            "duration_seconds": time.time() - started_at,
            "trace": {
                "backend": trace.started_backend or args.trace_backend,
                "start": trace_start,
                "stop": trace_stop,
                "etl": str(trace.etl_path) if trace.etl_path.exists() else None,
            },
            "smart_before": smart_before,
            "smart_after": smart_after,
            "smart_delta": smart_diff,
            "process_io": process_summary,
            "etl_exports": exports,
            "file_attribution": attribution,
            "logical_write_bytes_for_estimate": logical_for_estimate,
            "wear_estimates": estimates,
        }
    )
    json_path = out_dir / "analysis_report.json"
    json_path.write_text(json.dumps(report, ensure_ascii=True, indent=2, default=str), encoding="utf-8")
    md_path = write_report(report, out_dir, max_file_rows=args.max_file_rows)
    print(f"Report: {md_path}")
    print(f"JSON: {json_path}")
    return 0


def parse_existing_etl(args: argparse.Namespace) -> int:
    ensure_windows()
    etl_path = Path(args.etl).resolve()
    project_root = Path(args.project_root).resolve()
    data_dir = Path(args.data_dir).resolve() if args.data_dir else default_data_dir()
    out_dir = Path(args.out_dir) if args.out_dir else etl_path.parent / f"{etl_path.stem}_parsed_{local_timestamp()}"
    out_dir.mkdir(parents=True, exist_ok=True)
    xperf = resolve_tool("xperf.exe", args.xperf)
    tracerpt = resolve_tool("tracerpt.exe", None)
    known_pid_groups: dict[int, str] = {}
    exports = export_etl(etl_path, out_dir, xperf=xperf, tracerpt=tracerpt)
    attribution = choose_attribution(exports, known_pid_groups, project_root, data_dir)
    attr_csv = write_file_attribution_csv(attribution, out_dir, max_rows=None)
    if attr_csv:
        attribution["csv"] = attr_csv
    report = {
        "started_at": now_utc_iso(),
        "finished_at": now_utc_iso(),
        "duration_seconds": 0.0,
        "trace": {"backend": "parse_existing", "etl": str(etl_path)},
        "project_root": str(project_root),
        "data_dir": str(data_dir) if data_dir else None,
        "etl_exports": exports,
        "file_attribution": attribution,
        "process_io": {},
        "smart_delta": {},
        "wear_estimates": [],
    }
    (out_dir / "analysis_report.json").write_text(json.dumps(report, ensure_ascii=True, indent=2, default=str), encoding="utf-8")
    md_path = write_report(report, out_dir, max_file_rows=args.max_file_rows)
    print(f"Report: {md_path}")
    return 0 if attribution.get("parsed") else 2


def get_nested(mapping: dict[str, Any], path: Iterable[str], default: Any = None) -> Any:
    current: Any = mapping
    for key in path:
        if not isinstance(current, dict) or key not in current:
            return default
        current = current[key]
    return current


def metric_delta(
    baseline: float | int | None,
    workload: float | int | None,
    baseline_duration: float,
    workload_duration: float,
) -> dict[str, Any]:
    baseline_value = float(baseline or 0)
    workload_value = float(workload or 0)
    baseline_rate = baseline_value / baseline_duration if baseline_duration > 0 else 0.0
    workload_rate = workload_value / workload_duration if workload_duration > 0 else 0.0
    excess_rate = workload_rate - baseline_rate
    comparable_duration = min(baseline_duration, workload_duration) if baseline_duration and workload_duration else workload_duration
    return {
        "baseline": int(baseline_value),
        "workload": int(workload_value),
        "delta_raw": int(workload_value - baseline_value),
        "baseline_per_second": baseline_rate,
        "workload_per_second": workload_rate,
        "excess_per_second": excess_rate,
        "baseline_adjusted_delta": int(excess_rate * comparable_duration),
        "comparable_duration_seconds": comparable_duration,
    }


def compare_named_totals(
    baseline: dict[str, Any],
    workload: dict[str, Any],
    *,
    value_keys: tuple[str, ...],
    baseline_duration: float,
    workload_duration: float,
) -> dict[str, dict[str, Any]]:
    names = set(baseline) | set(workload)
    result: dict[str, dict[str, Any]] = {}
    for name in names:
        row: dict[str, Any] = {}
        for key in value_keys:
            row[key] = metric_delta(
                get_nested(baseline, [name, key], 0),
                get_nested(workload, [name, key], 0),
                baseline_duration,
                workload_duration,
            )
        result[name] = row
    return dict(sorted(result.items(), key=lambda item: item[1][value_keys[0]]["baseline_adjusted_delta"], reverse=True))


def compare_file_totals(
    baseline_rows: list[dict[str, Any]],
    workload_rows: list[dict[str, Any]],
    *,
    baseline_duration: float,
    workload_duration: float,
) -> list[dict[str, Any]]:
    baseline: dict[tuple[str, str], dict[str, Any]] = {}
    workload: dict[tuple[str, str], dict[str, Any]] = {}
    for row in baseline_rows:
        baseline[(str(row.get("group", "")), str(row.get("path", "")))] = row
    for row in workload_rows:
        workload[(str(row.get("group", "")), str(row.get("path", "")))] = row
    keys = set(baseline) | set(workload)
    result = []
    for key in keys:
        b = baseline.get(key, {})
        w = workload.get(key, {})
        bytes_delta = metric_delta(b.get("bytes", 0), w.get("bytes", 0), baseline_duration, workload_duration)
        events_delta = metric_delta(b.get("events", 0), w.get("events", 0), baseline_duration, workload_duration)
        result.append(
            {
                "group": key[0],
                "path": key[1],
                "category": w.get("category") or b.get("category") or "",
                "bytes": bytes_delta,
                "events": events_delta,
                "workload_process_names": w.get("process_names", []),
                "baseline_process_names": b.get("process_names", []),
            }
        )
    result.sort(key=lambda row: row["bytes"]["baseline_adjusted_delta"], reverse=True)
    return result


def run_phase_subprocess(args: argparse.Namespace, out_dir: Path, duration: float, workload_command: str | None) -> dict[str, Any]:
    command = [
        sys.executable,
        str(Path(__file__).resolve()),
        "--project-root",
        str(Path(args.project_root).resolve()),
        "run",
        "--out-dir",
        str(out_dir),
        "--duration-seconds",
        str(duration),
        "--interval",
        str(args.interval),
        "--trace-backend",
        args.trace_backend,
        "--waf",
        args.waf,
        "--max-file-rows",
        str(args.max_file_rows),
    ]
    if args.xperf:
        command.extend(["--xperf", args.xperf])
    if args.wpr:
        command.extend(["--wpr", args.wpr])
    if args.data_dir:
        command.extend(["--data-dir", args.data_dir])
    if args.tbw_tb is not None:
        command.extend(["--tbw-tb", str(args.tbw_tb)])
    if workload_command:
        command.extend(["--workload-command", workload_command])
        if args.terminate_workload_on_stop:
            command.append("--terminate-workload-on-stop")
    timeout = duration + float(args.phase_timeout_slack_seconds)
    return run_command(command, timeout=timeout, cwd=Path(args.project_root).resolve())


def write_comparison_report(comparison: dict[str, Any], out_dir: Path, *, max_file_rows: int) -> Path:
    path = out_dir / "comparison_report.md"
    lines: list[str] = []
    lines.append("# CarbonPaper Baseline Comparison")
    lines.append("")
    lines.append(f"- Started: `{comparison.get('started_at')}`")
    lines.append(f"- Finished: `{comparison.get('finished_at')}`")
    lines.append(f"- Baseline report: `{comparison.get('baseline_report_path')}`")
    lines.append(f"- Workload report: `{comparison.get('workload_report_path')}`")
    lines.append("")
    lines.append("Baseline-adjusted values subtract the baseline write rate from the workload write rate, then scale to the shorter phase duration.")
    lines.append("Negative values mean the workload phase wrote less than the baseline phase for that metric.")
    lines.append("")

    process = comparison.get("process_groups", {})
    lines.append("## Process I/O Excess")
    lines.append("")
    if process:
        lines.append("| Group | Baseline write | Workload write | Baseline-adjusted write | Workload write/s | Baseline write/s |")
        lines.append("| --- | ---: | ---: | ---: | ---: | ---: |")
        for group, values in process.items():
            write = values.get("write_bytes", {})
            lines.append(
                f"| `{group}` | {format_bytes(write.get('baseline'))} | {format_bytes(write.get('workload'))} | "
                f"{format_bytes(write.get('baseline_adjusted_delta'))} | {format_bytes(write.get('workload_per_second'))}/s | "
                f"{format_bytes(write.get('baseline_per_second'))}/s |"
            )
    else:
        lines.append("No process comparison data.")
    lines.append("")

    etw_groups = comparison.get("etw_groups", {})
    lines.append("## ETW File Write Excess By Group")
    lines.append("")
    if etw_groups:
        lines.append("| Group | Baseline write | Workload write | Baseline-adjusted write |")
        lines.append("| --- | ---: | ---: | ---: |")
        for group, values in etw_groups.items():
            write = values.get("bytes", {})
            lines.append(
                f"| `{group}` | {format_bytes(write.get('baseline'))} | {format_bytes(write.get('workload'))} | "
                f"{format_bytes(write.get('baseline_adjusted_delta'))} |"
            )
    else:
        lines.append("No ETW group comparison data.")
    lines.append("")

    categories = comparison.get("etw_categories", {})
    lines.append("## ETW File Write Excess By Category")
    lines.append("")
    if categories:
        lines.append("| Category | Baseline write | Workload write | Baseline-adjusted write |")
        lines.append("| --- | ---: | ---: | ---: |")
        for category, values in categories.items():
            write = values.get("bytes", {})
            lines.append(
                f"| `{category}` | {format_bytes(write.get('baseline'))} | {format_bytes(write.get('workload'))} | "
                f"{format_bytes(write.get('baseline_adjusted_delta'))} |"
            )
    else:
        lines.append("No ETW category comparison data.")
    lines.append("")

    lines.append(f"## Top {max_file_rows} Baseline-Adjusted Files")
    lines.append("")
    files = comparison.get("etw_files", [])
    if files:
        lines.append("| Excess write | Workload write | Baseline write | Group | Category | Path |")
        lines.append("| ---: | ---: | ---: | --- | --- | --- |")
        for row in files[:max_file_rows]:
            write = row.get("bytes", {})
            path_text = str(row.get("path", "")).replace("|", "\\|")
            lines.append(
                f"| {format_bytes(write.get('baseline_adjusted_delta'))} | {format_bytes(write.get('workload'))} | "
                f"{format_bytes(write.get('baseline'))} | `{row.get('group')}` | `{row.get('category')}` | `{path_text}` |"
            )
    else:
        lines.append("No ETW file comparison data.")
    lines.append("")

    smart = comparison.get("smart_host_writes", {})
    lines.append("## Whole-Disk Host Write Excess")
    lines.append("")
    if smart:
        lines.append(
            f"- Baseline: `{format_bytes(smart.get('baseline'))}`\n"
            f"- Workload: `{format_bytes(smart.get('workload'))}`\n"
            f"- Baseline-adjusted: `{format_bytes(smart.get('baseline_adjusted_delta'))}`"
        )
    else:
        lines.append("No SMART/storage comparison data.")
    lines.append("")

    estimates = comparison.get("wear_estimates", [])
    lines.append("## Baseline-Adjusted Wear Estimates")
    lines.append("")
    if estimates:
        lines.append("| Basis | WAF | Basis write | Estimated NAND write | TBW used |")
        lines.append("| --- | ---: | ---: | ---: | ---: |")
        for row in estimates:
            pct = row.get("tbw_used_percent")
            pct_text = "n/a" if pct is None else f"{pct:.6f}%"
            lines.append(
                f"| `{row.get('basis')}` | {row.get('waf')} | {format_bytes(row.get('basis_bytes'))} | "
                f"{format_bytes(row.get('estimated_nand_bytes'))} | {pct_text} |"
            )
    else:
        lines.append("No baseline-adjusted wear estimates.")
    lines.append("")
    path.write_text("\n".join(lines), encoding="utf-8")
    return path


def run_compare(args: argparse.Namespace) -> int:
    ensure_windows()
    project_root = Path(args.project_root).resolve()
    out_dir = Path(args.out_dir) if args.out_dir else project_root / "tools" / "disk-wear-runs" / f"compare_{local_timestamp()}"
    baseline_dir = out_dir / "baseline"
    workload_dir = out_dir / "workload"
    out_dir.mkdir(parents=True, exist_ok=True)
    comparison_started_at = now_utc_iso()

    baseline_duration = float(args.baseline_duration_seconds or args.duration_seconds)
    workload_duration = float(args.workload_duration_seconds or args.duration_seconds)
    if baseline_duration <= 0 or workload_duration <= 0:
        raise SystemExit("compare requires positive baseline/workload durations.")

    print(f"Comparison output directory: {out_dir}")
    print(f"Running baseline phase for {baseline_duration:.1f}s...")
    baseline_result = run_phase_subprocess(args, baseline_dir, baseline_duration, args.baseline_command)
    append_jsonl(out_dir / "compare_commands.jsonl", {"phase": "baseline", **baseline_result})
    if baseline_result.get("returncode") != 0:
        print(baseline_result.get("stdout", ""))
        print(baseline_result.get("stderr", ""), file=sys.stderr)
        return int(baseline_result.get("returncode") or 1)

    if args.pause_seconds > 0:
        print(f"Pausing {args.pause_seconds:.1f}s before workload phase...")
        time.sleep(args.pause_seconds)

    print(f"Running workload phase for {workload_duration:.1f}s...")
    workload_result = run_phase_subprocess(args, workload_dir, workload_duration, args.workload_command)
    append_jsonl(out_dir / "compare_commands.jsonl", {"phase": "workload", **workload_result})
    if workload_result.get("returncode") != 0:
        print(workload_result.get("stdout", ""))
        print(workload_result.get("stderr", ""), file=sys.stderr)
        return int(workload_result.get("returncode") or 1)

    baseline_report_path = baseline_dir / "analysis_report.json"
    workload_report_path = workload_dir / "analysis_report.json"
    baseline = json.loads(baseline_report_path.read_text(encoding="utf-8"))
    workload = json.loads(workload_report_path.read_text(encoding="utf-8"))

    baseline_duration_actual = float(baseline.get("duration_seconds") or baseline_duration)
    workload_duration_actual = float(workload.get("duration_seconds") or workload_duration)
    process_groups = compare_named_totals(
        get_nested(baseline, ["process_io", "totals_by_group"], {}),
        get_nested(workload, ["process_io", "totals_by_group"], {}),
        value_keys=("write_bytes", "write_ops", "read_bytes", "read_ops"),
        baseline_duration=baseline_duration_actual,
        workload_duration=workload_duration_actual,
    )
    etw_groups = compare_named_totals(
        get_nested(baseline, ["file_attribution", "group_totals"], {}),
        get_nested(workload, ["file_attribution", "group_totals"], {}),
        value_keys=("bytes", "events"),
        baseline_duration=baseline_duration_actual,
        workload_duration=workload_duration_actual,
    )
    etw_categories = compare_named_totals(
        get_nested(baseline, ["file_attribution", "category_totals"], {}),
        get_nested(workload, ["file_attribution", "category_totals"], {}),
        value_keys=("bytes", "events"),
        baseline_duration=baseline_duration_actual,
        workload_duration=workload_duration_actual,
    )
    etw_files = compare_file_totals(
        get_nested(baseline, ["file_attribution", "file_totals"], []),
        get_nested(workload, ["file_attribution", "file_totals"], []),
        baseline_duration=baseline_duration_actual,
        workload_duration=workload_duration_actual,
    )
    smart_delta = metric_delta(
        get_nested(baseline, ["smart_delta", "total_delta_bytes"], 0),
        get_nested(workload, ["smart_delta", "total_delta_bytes"], 0),
        baseline_duration_actual,
        workload_duration_actual,
    )

    etw_adjusted = sum(max(0, values["bytes"]["baseline_adjusted_delta"]) for values in etw_groups.values()) if etw_groups else None
    process_adjusted = sum(max(0, values["write_bytes"]["baseline_adjusted_delta"]) for values in process_groups.values()) if process_groups else None
    logical_basis = etw_adjusted if etw_adjusted is not None else process_adjusted
    estimates = estimate_wear(
        logical_write_bytes=logical_basis,
        smart_host_delta_bytes=max(0, smart_delta["baseline_adjusted_delta"]),
        tbw_tb=args.tbw_tb,
        wafs=parse_wafs(args.waf),
    )

    comparison = {
        "started_at": comparison_started_at,
        "finished_at": now_utc_iso(),
        "project_root": str(project_root),
        "baseline_report_path": str(baseline_report_path),
        "workload_report_path": str(workload_report_path),
        "baseline_duration_seconds": baseline_duration_actual,
        "workload_duration_seconds": workload_duration_actual,
        "process_groups": process_groups,
        "etw_groups": etw_groups,
        "etw_categories": etw_categories,
        "etw_files": etw_files,
        "smart_host_writes": smart_delta,
        "wear_estimates": estimates,
    }
    comparison_json = out_dir / "comparison_report.json"
    comparison_json.write_text(json.dumps(comparison, ensure_ascii=True, indent=2, default=str), encoding="utf-8")
    comparison_md = write_comparison_report(comparison, out_dir, max_file_rows=args.max_file_rows)
    print(f"Comparison report: {comparison_md}")
    print(f"Comparison JSON: {comparison_json}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Capture CarbonPaper process writes, ETW per-file writes, SMART host-write deltas, and wear estimates.",
    )
    parser.add_argument("--project-root", default=str(Path(__file__).resolve().parents[1]), help="CarbonPaper repository root.")
    sub = parser.add_subparsers(dest="command", required=True)

    pre = sub.add_parser("preflight", help="Check privileges, tools, SMART counters, and current CarbonPaper processes.")
    pre.add_argument("--xperf", default=None, help="Path to xperf.exe.")
    pre.add_argument("--wpr", default=None, help="Path to wpr.exe.")
    pre.set_defaults(func=preflight)

    run = sub.add_parser("run", help="Run a full capture and generate a report.")
    run.add_argument("--out-dir", default=None, help="Output directory. Defaults to tools/disk-wear-runs/<timestamp>.")
    run.add_argument("--duration-seconds", type=float, default=None, help="Capture duration. If omitted, capture until Ctrl+C or workload exits.")
    run.add_argument("--interval", type=float, default=DEFAULT_INTERVAL_SECONDS, help="Process I/O sample interval in seconds.")
    run.add_argument("--trace-backend", choices=["auto", "xperf", "wpr", "none"], default="auto", help="ETW trace backend.")
    run.add_argument("--xperf", default=None, help="Path to xperf.exe.")
    run.add_argument("--wpr", default=None, help="Path to wpr.exe.")
    run.add_argument("--data-dir", default=None, help="CarbonPaper data directory for path categorization.")
    run.add_argument("--tbw-tb", type=float, default=None, help="SSD rated TBW in decimal TB for percentage estimates.")
    run.add_argument("--waf", default=",".join(str(v) for v in DEFAULT_WAFS), help="Comma-separated write amplification factors.")
    run.add_argument("--max-file-rows", type=int, default=50, help="Top file rows shown in Markdown report.")
    run.add_argument("--workload-command", default=None, help="Optional shell command to launch after trace starts.")
    run.add_argument("--terminate-workload-on-stop", action="store_true", help="Terminate the workload command when capture stops.")
    run.set_defaults(func=run_capture)

    compare = sub.add_parser("compare", help="Run baseline and workload captures, then generate a baseline-adjusted comparison report.")
    compare.add_argument("--out-dir", default=None, help="Output directory. Defaults to tools/disk-wear-runs/compare_<timestamp>.")
    compare.add_argument("--duration-seconds", type=float, default=300.0, help="Default duration for both baseline and workload phases.")
    compare.add_argument("--baseline-duration-seconds", type=float, default=None, help="Baseline phase duration. Defaults to --duration-seconds.")
    compare.add_argument("--workload-duration-seconds", type=float, default=None, help="Workload phase duration. Defaults to --duration-seconds.")
    compare.add_argument("--pause-seconds", type=float, default=10.0, help="Pause between baseline and workload phases.")
    compare.add_argument("--interval", type=float, default=DEFAULT_INTERVAL_SECONDS, help="Process I/O sample interval in seconds.")
    compare.add_argument("--trace-backend", choices=["auto", "xperf", "wpr", "none"], default="auto", help="ETW trace backend.")
    compare.add_argument("--xperf", default=None, help="Path to xperf.exe.")
    compare.add_argument("--wpr", default=None, help="Path to wpr.exe.")
    compare.add_argument("--data-dir", default=None, help="CarbonPaper data directory for path categorization.")
    compare.add_argument("--tbw-tb", type=float, default=None, help="SSD rated TBW in decimal TB for percentage estimates.")
    compare.add_argument("--waf", default=",".join(str(v) for v in DEFAULT_WAFS), help="Comma-separated write amplification factors.")
    compare.add_argument("--max-file-rows", type=int, default=50, help="Top file rows shown in Markdown reports.")
    compare.add_argument("--baseline-command", default=None, help="Optional shell command to launch during the baseline phase.")
    compare.add_argument("--workload-command", default=None, help="Optional shell command to launch during the workload phase.")
    compare.add_argument("--terminate-workload-on-stop", action="store_true", help="Terminate phase workload commands when capture stops.")
    compare.add_argument("--phase-timeout-slack-seconds", type=float, default=900.0, help="Extra subprocess timeout for export/SMART overhead after each phase.")
    compare.set_defaults(func=run_compare)

    parse = sub.add_parser("parse-etl", help="Export and parse an existing ETL file.")
    parse.add_argument("etl", help="Path to ETL file.")
    parse.add_argument("--out-dir", default=None, help="Output directory for parsed artifacts.")
    parse.add_argument("--xperf", default=None, help="Path to xperf.exe.")
    parse.add_argument("--data-dir", default=None, help="CarbonPaper data directory for path categorization.")
    parse.add_argument("--max-file-rows", type=int, default=50, help="Top file rows shown in Markdown report.")
    parse.set_defaults(func=parse_existing_etl)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        return int(args.func(args))
    except KeyboardInterrupt:
        print("Interrupted.", file=sys.stderr)
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
