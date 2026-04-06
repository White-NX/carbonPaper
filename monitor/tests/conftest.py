import os
import sys
from pathlib import Path

import pytest


ROOT_DIR = Path(__file__).resolve().parents[2]
MONITOR_DIR = ROOT_DIR / "monitor"

if str(MONITOR_DIR) not in sys.path:
    sys.path.insert(0, str(MONITOR_DIR))


@pytest.fixture(autouse=True)
def isolate_runtime_dirs(tmp_path, monkeypatch):
    data_dir = tmp_path / "carbonpaper-data"
    local_appdata = tmp_path / "localappdata"
    monkeypatch.setenv("CARBONPAPER_DATA_DIR", str(data_dir))
    monkeypatch.setenv("LOCALAPPDATA", str(local_appdata))
    os.makedirs(data_dir, exist_ok=True)
    os.makedirs(local_appdata, exist_ok=True)
