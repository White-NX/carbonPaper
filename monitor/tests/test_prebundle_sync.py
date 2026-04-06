from pathlib import Path


def _normalise_text(text: str) -> str:
    return text.replace("\r\n", "\n")


def test_storage_client_source_matches_prebundle_copy():
    root = Path(__file__).resolve().parents[2]
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
