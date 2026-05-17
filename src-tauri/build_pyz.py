#!/usr/bin/env python3
"""Deterministic zipapp packager for monitor.pyz (replaces `python -m zipapp`).

Used by ``src-tauri/build.rs`` to produce a byte-stable zip archive whose
SHA-256 can be safely embedded into the Rust binary at compile time.

Unlike ``python -m zipapp``, every entry written here uses a fixed timestamp,
fixed external_attr / create_system / create_version, fixed compresslevel,
and entries are written in sorted order — so the output is identical for
identical input regardless of filesystem mtimes, OS, or walk order.

Note on cross-machine reproducibility: the underlying ``zlib`` deflate stream
is byte-identical given the same input and same compresslevel on the same
``zlib`` version. CPython on Windows bundles a fixed ``zlib``, so as long as
all builders use the same Python minor version the bytes match. Across
Python upgrades the hash may shift — that's fine because the binary and the
.pyz are produced together.

Usage:
    python build_pyz.py <source_dir> <output_pyz> <module>:<function>

The generated archive mimics what ``python -m zipapp -m "module:function"``
would produce, including the auto-generated ``__main__.py`` entry point.
"""

import os
import sys
import zipfile


# Earliest representable date in ZIP format: 1980-01-01 00:00:00.
# Using the absolute minimum makes the timestamp deterministic and obviously
# synthetic (no real source file ever has this mtime).
FIXED_DT = (1980, 1, 1, 0, 0, 0)

# Mirrors ``zipapp.MAIN_TEMPLATE`` from CPython's ``Lib/zipapp.py``.
MAIN_TEMPLATE = "# -*- coding: utf-8 -*-\nimport {module}\n{module}.{fn}()\n"


def _add_entry(zf: zipfile.ZipFile, archive_name: str, data: bytes) -> None:
    info = zipfile.ZipInfo(filename=archive_name, date_time=FIXED_DT)
    info.compress_type = zipfile.ZIP_DEFLATED
    info.create_system = 3        # Unix; chosen for cross-platform determinism
    info.create_version = 20
    info.extract_version = 20
    info.external_attr = (0o100644 & 0xFFFF) << 16  # regular file, rw-r--r--
    zf.writestr(info, data)


def main() -> int:
    if len(sys.argv) != 4:
        print(
            f"Usage: {sys.argv[0]} <source_dir> <output_pyz> <module>:<function>",
            file=sys.stderr,
        )
        return 2

    src_dir = os.path.abspath(sys.argv[1])
    out_pyz = os.path.abspath(sys.argv[2])
    main_spec = sys.argv[3]
    if ":" not in main_spec:
        print(
            f"main spec must be in 'module:function' form (got {main_spec!r})",
            file=sys.stderr,
        )
        return 2
    main_module, main_fn = main_spec.split(":", 1)

    if not os.path.isdir(src_dir):
        print(f"source dir does not exist: {src_dir}", file=sys.stderr)
        return 2

    # Walk in sorted order so entry order is deterministic across filesystems.
    entries = []
    for root, dirs, files in os.walk(src_dir):
        dirs.sort()
        for fname in sorted(files):
            full = os.path.join(root, fname)
            rel = os.path.relpath(full, src_dir).replace(os.sep, "/")
            entries.append((rel, full))

    # Ensure parent dir exists for the output file.
    out_parent = os.path.dirname(out_pyz)
    if out_parent:
        os.makedirs(out_parent, exist_ok=True)

    with zipfile.ZipFile(
        out_pyz,
        mode="w",
        compression=zipfile.ZIP_DEFLATED,
        compresslevel=6,
    ) as zf:
        for archive_name, full_path in entries:
            with open(full_path, "rb") as f:
                data = f.read()
            _add_entry(zf, archive_name, data)
        # Auto-generated entry point, matching ``python -m zipapp -m "main:main"``.
        main_src = MAIN_TEMPLATE.format(module=main_module, fn=main_fn)
        _add_entry(zf, "__main__.py", main_src.encode("utf-8"))

    return 0


if __name__ == "__main__":
    sys.exit(main())
