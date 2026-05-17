import zipfile
from pathlib import Path


def _normalise_text(text: str) -> str:
    return text.replace("\r\n", "\n")


def _project_root() -> Path:
    return Path(__file__).resolve().parents[2]


def _pyz_path() -> Path:
    return _project_root() / "src-tauri" / "pre-bundle" / "monitor.pyz"


def test_storage_client_source_matches_prebundle_copy():
    root = _project_root()
    source = root / "monitor" / "storage_client.py"
    prebundle = root / "src-tauri" / "pre-bundle" / "monitor" / "storage_client.py"

    assert prebundle.exists(), (
        "Missing pre-bundle copy: run Rust build/tests first so src-tauri/build.rs "
        "can generate src-tauri/pre-bundle/monitor/storage_client.py"
    )

    source_text = _normalise_text(source.read_text(encoding="utf-8"))
    prebundle_text = _normalise_text(prebundle.read_text(encoding="utf-8"))

    assert source_text == prebundle_text, (
        "monitor/storage_client.py and src-tauri/pre-bundle/monitor/storage_client.py "
        "must stay in sync"
    )


def test_monitor_pyz_exists_after_build():
    assert _pyz_path().exists(), (
        "Missing monitor.pyz: run `cargo build` (or `cargo test --lib`) so "
        "src-tauri/build.rs can produce src-tauri/pre-bundle/monitor.pyz"
    )


def test_monitor_pyz_is_valid_zip():
    with zipfile.ZipFile(_pyz_path()) as z:
        bad = z.testzip()
        assert bad is None, f"monitor.pyz contains corrupt member: {bad}"


def test_monitor_pyz_contains_entry_point():
    with zipfile.ZipFile(_pyz_path()) as z:
        names = set(z.namelist())
    # zipapp -m "main:main" generates a top-level __main__.py
    assert "__main__.py" in names, f"missing __main__.py in pyz; got {sorted(names)[:20]}"
    assert "main.py" in names, "missing main.py in pyz"
    assert "monitor/__init__.py" in names, "missing monitor/__init__.py (subpackage) in pyz"


def test_monitor_pyz_excludes_tests():
    with zipfile.ZipFile(_pyz_path()) as z:
        names = z.namelist()
    leaked = [n for n in names if n.startswith("tests/") or "/tests/" in n]
    assert not leaked, f"Test files leaked into production monitor.pyz: {leaked}"


def test_monitor_pyz_excludes_pycache():
    with zipfile.ZipFile(_pyz_path()) as z:
        names = z.namelist()
    leaked = [n for n in names if "__pycache__" in n]
    assert not leaked, f"__pycache__ leaked into production monitor.pyz: {leaked}"


def test_monitor_pyz_excludes_chroma_db():
    """chroma_db/ 是运行时数据目录，决不能进 .pyz。"""
    with zipfile.ZipFile(_pyz_path()) as z:
        names = z.namelist()
    leaked = [n for n in names if n.startswith("chroma_db/") or "/chroma_db/" in n]
    assert not leaked, f"chroma_db files leaked into production monitor.pyz: {leaked}"


def test_monitor_pyz_contains_current_source_files():
    """监管陈旧文件不会因为 build.rs 的某条路径偷偷流入 .pyz。
    对每个源 .py 文件，确认其在 .pyz 内的副本与源内容一致。"""
    root = _project_root()
    src_monitor = root / "monitor"
    # 收集源 monitor/ 下应该进 .pyz 的 .py 文件（与 build.rs 的过滤一致）
    source_py_files = []
    for p in src_monitor.rglob("*.py"):
        rel = p.relative_to(src_monitor).as_posix()
        if rel.startswith("tests/") or "__pycache__" in rel or rel.startswith(".venv/"):
            continue
        source_py_files.append((rel, p))

    assert source_py_files, "No source .py files found — sanity check failed"

    with zipfile.ZipFile(_pyz_path()) as z:
        pyz_members = set(z.namelist())
        for rel, src_path in source_py_files:
            assert rel in pyz_members, f"Source file {rel} missing from monitor.pyz"
            pyz_content = _normalise_text(z.read(rel).decode("utf-8"))
            src_content = _normalise_text(src_path.read_text(encoding="utf-8"))
            assert pyz_content == src_content, (
                f"monitor.pyz content for {rel} differs from source"
            )
