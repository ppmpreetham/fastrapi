#!/usr/bin/env python3
"""
Generate static Python type stubs (.pyi) for fastrapi using maturin.
"""

import os
import shutil
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
PYTHON_DIR = REPO_ROOT / "python"
FASTRAPI_DIR = PYTHON_DIR / "fastrapi"

INIT_PY_CONTENT = """from .fastrapi import *

__doc__ = fastrapi.__doc__
if hasattr(fastrapi, "__all__"):
    __all__ = fastrapi.__all__
"""


def main() -> int:
    print("Generating static type stubs for fastrapi...")

    # Find maturin in current environment or system PATH
    maturin_cmd = shutil.which("maturin") or "maturin"

    # Forward current python interpreter if PYO3_PYTHON is not set
    env = os.environ.copy()
    if "PYO3_PYTHON" not in env:
        env["PYO3_PYTHON"] = sys.executable

    cmd = [maturin_cmd, "generate-stubs", "--out", str(PYTHON_DIR)]
    print(f"Running: {' '.join(cmd)}")

    try:
        subprocess.run(cmd, cwd=REPO_ROOT, env=env, check=True)
    except FileNotFoundError:
        print(
            "Error: 'maturin' executable not found. Make sure maturin is installed "
            "and your virtual environment is active.",
            file=sys.stderr,
        )
        return 1
    except subprocess.CalledProcessError as e:
        print(
            f"Error: maturin generate-stubs failed with exit code {e.returncode}.",
            file=sys.stderr,
        )
        return e.returncode

    # Ensure __init__.py exists
    init_py = FASTRAPI_DIR / "__init__.py"
    if not init_py.exists():
        print(f"Creating {init_py.relative_to(REPO_ROOT)}...")
        init_py.write_text(INIT_PY_CONTENT, encoding="utf-8")

    # Ensure PEP 561 py.typed marker exists
    py_typed = FASTRAPI_DIR / "py.typed"
    if not py_typed.exists():
        print(f"Creating {py_typed.relative_to(REPO_ROOT)}...")
        py_typed.write_text("", encoding="utf-8")

    # Report generated stubs
    stubs = list(FASTRAPI_DIR.glob("**/*.pyi"))
    print(
        f"\nSuccessfully generated {len(stubs)} type stub (.pyi) files in "
        f"{FASTRAPI_DIR.relative_to(REPO_ROOT)}:"
    )
    for stub in sorted(stubs):
        print(f"  - {stub.relative_to(PYTHON_DIR)}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
